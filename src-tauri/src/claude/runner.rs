//! Claude workspace: runner. Spawns `claude -p` in bidirectional stream-json mode (see the
//! protocol in mod.rs), forwards every stdout object to the frontend as `claude-msg`, and
//! keeps per-run bookkeeping: session id, pending `can_use_tool` requests, and waiters for
//! the control requests we send.
use super::locate;
use super::*;
use crate::error::{AppError, AppResult};
use base64::Engine;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{oneshot, watch};

const STDERR_LINES: usize = 200;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_GRACE: Duration = Duration::from_secs(2);
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Extra knobs for internal runs (review); the frontend only sets `StartOptions`.
#[derive(Debug, Clone, Default)]
pub(super) struct Spawn {
    pub opts: StartOptions,
    /// Answer permission prompts over stdio (`--permission-prompt-tool stdio`).
    pub prompt_tool: bool,
    pub extra_args: Vec<String>,
    /// Close stdin once a `result` arrives (one-shot runs), so the process exits.
    pub close_after_result: bool,
    /// Run this binary instead of the claude found on PATH (an AI connection's command).
    pub program: Option<std::path::PathBuf>,
    /// Extra environment (an AI connection's variables and API key).
    pub env: Vec<(String, String)>,
}

/// Command line flags for a run (without the binary).
pub(super) fn build_args(spec: &Spawn) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if spec.prompt_tool {
        a.extend(["--permission-prompt-tool".into(), "stdio".into()]);
    }
    let o = &spec.opts;
    if let Some(m) = o.model.as_deref().filter(|s| !s.is_empty()) {
        a.extend(["--model".into(), m.into()]);
    }
    if let Some(m) = o.permission_mode.as_deref().filter(|s| !s.is_empty()) {
        a.extend(["--permission-mode".into(), m.into()]);
    }
    if let Some(r) = o.resume.as_deref().filter(|s| !s.is_empty()) {
        a.extend(["--resume".into(), r.into()]);
        if o.fork {
            a.push("--fork-session".into());
        }
    }
    a.extend(spec.extra_args.iter().cloned());
    a
}

fn control_request(request_id: &str, subtype: &str, payload: Option<&Value>) -> Value {
    let mut req = serde_json::Map::new();
    if let Some(Value::Object(p)) = payload {
        req.extend(p.clone());
    }
    req.insert("subtype".into(), subtype.into());
    json!({ "type": "control_request", "request_id": request_id, "request": req })
}

type Waiter = oneshot::Sender<Result<Value, String>>;

/// Per-run protocol state, driven by stdout lines (no process needed — unit tested).
#[derive(Default)]
pub(super) struct RunState {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub busy: bool,
    /// Outstanding `can_use_tool` requests: request_id → request body.
    pub pending_permissions: HashMap<String, Value>,
    waiters: HashMap<String, Waiter>,
    close_after_result: bool,
}

/// What the reader loop should do with one stdout line.
#[derive(Debug, Default)]
pub(super) struct Handled {
    /// Parsed object to forward as `claude-msg`.
    pub msg: Option<Value>,
    /// Line to write back to stdin (error reply to control requests we can't serve).
    pub reply: Option<Value>,
    pub close_stdin: bool,
}

impl RunState {
    pub fn handle_line(&mut self, line: &str) -> Handled {
        let line = line.trim();
        if line.is_empty() {
            return Handled::default();
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return Handled::default();
        };
        let mut out = Handled::default();
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if let Some(id) = s(&msg, "session_id") {
                    self.session_id = Some(id);
                }
                if msg.get("subtype").and_then(Value::as_str) == Some("init") {
                    self.model = s(&msg, "model").or(self.model.take());
                    self.permission_mode = s(&msg, "permissionMode").or(self.permission_mode.take());
                } else if let Some(m) = s(&msg, "permissionMode") {
                    self.permission_mode = Some(m);
                }
            }
            "result" => {
                self.busy = false;
                if let Some(id) = s(&msg, "session_id") {
                    self.session_id = Some(id);
                }
                // Anything still waiting for an answer is moot once the turn ended.
                self.pending_permissions.clear();
                out.close_stdin = self.close_after_result;
            }
            "control_response" => {
                let resp = msg.get("response").cloned().unwrap_or(Value::Null);
                if let Some(w) = s(&resp, "request_id").and_then(|id| self.waiters.remove(&id)) {
                    let r = if resp.get("subtype").and_then(Value::as_str) == Some("error") {
                        Err(s(&resp, "error").unwrap_or_else(|| "control request failed".into()))
                    } else {
                        Ok(resp.get("response").cloned().unwrap_or(Value::Null))
                    };
                    let _ = w.send(r);
                }
            }
            "control_request" => {
                let id = s(&msg, "request_id").unwrap_or_default();
                let req = msg.get("request").cloned().unwrap_or(Value::Null);
                match req.get("subtype").and_then(Value::as_str) {
                    Some("can_use_tool") => {
                        self.pending_permissions.insert(id, req);
                    }
                    other => {
                        out.reply = Some(json!({
                            "type": "control_response",
                            "response": {
                                "subtype": "error",
                                "request_id": id,
                                "error": format!("DevCat does not handle control request {:?}", other.unwrap_or("")),
                            }
                        }));
                    }
                }
            }
            "control_cancel_request" => {
                if let Some(id) = s(&msg, "request_id") {
                    self.pending_permissions.remove(&id);
                }
            }
            _ => {
                if let Some(id) = s(&msg, "session_id").filter(|s| !s.is_empty()) {
                    self.session_id = Some(id);
                }
            }
        }
        out.msg = Some(msg);
        out
    }

    fn add_waiter(&mut self, id: String) -> oneshot::Receiver<Result<Value, String>> {
        let (tx, rx) = oneshot::channel();
        self.waiters.insert(id, tx);
        rx
    }

    /// Fail every waiter (process gone).
    fn fail_waiters(&mut self, why: &str) {
        for (_, w) in self.waiters.drain() {
            let _ = w.send(Err(why.to_string()));
        }
        self.pending_permissions.clear();
        self.busy = false;
    }
}

struct Run {
    cwd: String,
    state: Mutex<RunState>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    stderr: Mutex<VecDeque<String>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<bool>,
    stopping: AtomicBool,
    pid: Option<u32>,
    /// Label of the window that started the run (stopped when it closes).
    owner: Mutex<Option<String>>,
}

static RUNS: Lazy<Mutex<HashMap<String, Arc<Run>>>> = Lazy::new(Default::default);

fn get_run(run_id: &str) -> AppResult<Arc<Run>> {
    RUNS.lock().get(run_id).cloned().ok_or_else(|| AppError::msg(format!("Claude run {run_id} is not running")))
}

impl Run {
    async fn write(&self, v: &Value) -> AppResult<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| AppError::msg("Claude run input is closed"))?;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn close_stdin(&self) {
        if let Some(mut s) = self.stdin.lock().await.take() {
            let _ = s.shutdown().await;
        }
    }

    /// Send a control request and wait for its control_response.
    async fn control(&self, subtype: &str, payload: Option<&Value>, timeout: Duration) -> AppResult<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.state.lock().add_waiter(id.clone());
        if let Err(e) = self.write(&control_request(&id, subtype, payload)).await {
            self.state.lock().waiters.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(AppError::msg(e)),
            Ok(Err(_)) => Err(AppError::msg("Claude exited before answering")),
            Err(_) => {
                self.state.lock().waiters.remove(&id);
                Err(AppError::msg(format!("Claude did not answer the {subtype} request in time")))
            }
        }
    }

    fn stderr_tail(&self, n: usize) -> Option<String> {
        let buf = self.stderr.lock();
        let lines: Vec<&str> = buf.iter().skip(buf.len().saturating_sub(n)).map(String::as_str).collect();
        let s = lines.join("\n").trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

fn push_stderr(run: &Run, line: String) {
    let mut buf = run.stderr.lock();
    if buf.len() == STDERR_LINES {
        buf.pop_front();
    }
    buf.push_back(line);
}

/// Where run events go: the Tauri app in production, a channel in tests.
pub(crate) use crate::events::Sink;

/// Spawn a run, wire up its reader / exit tasks and send `initialize`. Returns the run id.
pub(super) async fn spawn_run(sink: Sink, cwd: &str, spec: Spawn) -> AppResult<String> {
    if !Path::new(cwd).is_dir() {
        return Err(AppError::msg(format!("Not a directory: {cwd}")));
    }
    let bin = match &spec.program {
        Some(p) => p.clone(),
        None => locate::claude_path().ok_or_else(|| AppError::msg("claude CLI not found (install Claude Code or set CLAUDE_PATH)"))?,
    };
    let mut cmd = locate::command(&bin).await;
    cmd.args(build_args(&spec))
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("Failed to start claude: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (kill_tx, kill_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = watch::channel(false);

    let run_id = uuid::Uuid::new_v4().to_string();
    let run = Arc::new(Run {
        cwd: cwd.to_string(),
        state: Mutex::new(RunState {
            model: spec.opts.model.clone(),
            permission_mode: spec.opts.permission_mode.clone(),
            close_after_result: spec.close_after_result,
            ..Default::default()
        }),
        stdin: tokio::sync::Mutex::new(child.stdin.take()),
        stderr: Mutex::new(VecDeque::new()),
        kill: Mutex::new(Some(kill_tx)),
        exited: exit_rx,
        stopping: AtomicBool::new(false),
        pid: child.id(),
        owner: Mutex::new(None),
    });
    RUNS.lock().insert(run_id.clone(), run.clone());

    // stderr → ring buffer
    let r = run.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            push_stderr(&r, l);
        }
    });

    // stdout → state machine → claude-msg (read_until: no line length limit)
    let (r, a, id) = (run.clone(), sink.clone(), run_id.clone());
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(256 * 1024, stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let h = r.state.lock().handle_line(&line);
            if let Some(reply) = &h.reply {
                let _ = r.write(reply).await;
            }
            if let Some(msg) = h.msg {
                a("claude-msg", json!({ "runId": id, "msg": msg }));
            } else if !line.trim().is_empty() {
                push_stderr(&r, line.trim().to_string());
            }
            if h.close_stdin {
                r.close_stdin().await;
            }
        }
    });

    // exit watcher
    let (r, id) = (run.clone(), run_id.clone());
    tokio::spawn(async move {
        let status = tokio::select! {
            s = child.wait() => s,
            _ = kill_rx => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), stdout_task).await;
        let _ = tokio::time::timeout(Duration::from_millis(500), stderr_task).await;
        r.state.lock().fail_waiters("Claude exited");
        let code = status.as_ref().ok().and_then(|s| s.code());
        let error = match (&status, code) {
            (Err(e), _) => Some(e.to_string()),
            (_, Some(0)) => None,
            _ if r.stopping.load(Ordering::SeqCst) => None,
            _ => r.stderr_tail(20).or_else(|| Some(format!("claude exited with {}", code.map_or("a signal".into(), |c| format!("code {c}"))))),
        };
        RUNS.lock().remove(&id);
        let _ = exit_tx.send(true);
        sink("claude-exit", json!({ "runId": id, "code": code, "error": error }));
    });

    // initialize handshake; its response also feeds the claude_info cache
    let r = run.clone();
    tokio::spawn(async move {
        if let Ok(v) = r.control("initialize", None, Duration::from_secs(60)).await {
            cache_account(&v);
        }
    });
    Ok(run_id)
}

// ---------------------------------------------------------------- claude_info

static ACCOUNT: Mutex<Option<(Option<Value>, Option<Value>)>> = Mutex::new(None);

fn cache_account(init: &Value) {
    let account = init.get("account").cloned().filter(|v| !v.is_null());
    let models = init.get("models").cloned().filter(|v| !v.is_null());
    if account.is_some() || models.is_some() {
        *ACCOUNT.lock() = Some((account, models));
    }
}

/// Short-lived process: initialize handshake only (no model call), then kill it.
async fn handshake(bin: &Path) -> AppResult<Value> {
    let mut cmd = locate::command(bin).await;
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--settings",
        r#"{"disableAllHooks":true}"#,
    ])
    .current_dir(std::env::temp_dir())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let req = control_request("devcat-info", "initialize", None);
    stdin.write_all(format!("{req}\n").as_bytes()).await?;
    stdin.flush().await?;
    let read = async {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf).await? == 0 {
                return Err(AppError::msg("claude exited during initialize"));
            }
            let Ok(v) = serde_json::from_slice::<Value>(&buf) else { continue };
            let resp = &v["response"];
            if v["type"] == "control_response" && resp["request_id"] == "devcat-info" {
                return match resp["subtype"].as_str() {
                    Some("error") => Err(AppError::msg(resp["error"].as_str().unwrap_or("initialize failed").to_string())),
                    _ => Ok(resp["response"].clone()),
                };
            }
        }
    };
    let res = tokio::time::timeout(Duration::from_secs(20), read).await;
    let _ = child.start_kill();
    res.map_err(|_| AppError::msg("claude initialize timed out"))?
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_info() -> AppResult<ClaudeInfo> {
    let Some(bin) = locate::claude_path() else {
        return Ok(ClaudeInfo { path: None, version: None, account: None, models: None });
    };
    let version = {
        let mut cmd = locate::command(&bin).await;
        cmd.arg("--version").stdin(Stdio::null()).kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(o)) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).split_whitespace().next().map(str::to_string)
            }
            _ => None,
        }
    };
    let cached = ACCOUNT.lock().clone();
    let (account, models) = match cached {
        Some(c) => c,
        None => match handshake(&bin).await {
            Ok(v) => {
                cache_account(&v);
                ACCOUNT.lock().clone().unwrap_or((None, None))
            }
            Err(_) => (None, None),
        },
    };
    Ok(ClaudeInfo { path: Some(bin.to_string_lossy().into_owned()), version, account, models })
}

// ---------------------------------------------------------------- runs

/// Start a session in `cwd`; returns a run id. Messages arrive as `claude-msg` events.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_start(window: crate::events::Owner, cwd: String, opts: StartOptions) -> AppResult<String> {
    let id = spawn_run(crate::events::sink(), &cwd, Spawn { opts, prompt_tool: true, ..Default::default() }).await?;
    set_owner(&id, window.label());
    Ok(id)
}

/// Tie a run to the window that started it.
pub(super) fn set_owner(run_id: &str, label: &str) {
    if let Some(run) = RUNS.lock().get(run_id) {
        *run.owner.lock() = Some(label.to_string());
    }
}

/// Stop every run started by a window (it was closed).
pub async fn stop_owned_by(label: &str) {
    let runs: Vec<Arc<Run>> = RUNS.lock().values().filter(|r| r.owner.lock().as_deref() == Some(label)).cloned().collect();
    futures::future::join_all(runs.into_iter().map(stop_run)).await;
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub(super) fn user_message(text: &str, images: &[(String, String)]) -> Value {
    let mut content: Vec<Value> = images
        .iter()
        .map(|(mt, data)| json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } }))
        .collect();
    if !text.is_empty() || content.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    json!({ "type": "user", "message": { "role": "user", "content": content } })
}

async fn load_image(path: &str) -> AppResult<(String, String)> {
    let p = Path::new(path);
    let mt = image_media_type(p).ok_or_else(|| AppError::msg(format!("Unsupported image type: {path} (png, jpeg, gif, webp)")))?;
    let size = tokio::fs::metadata(p).await?.len();
    if size > MAX_IMAGE_BYTES {
        return Err(AppError::msg(format!("Image too large ({} KB, max 5 MB): {path}", size / 1024)));
    }
    let bytes = tokio::fs::read(p).await?;
    Ok((mt.to_string(), base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Write a user message to a run and mark it busy.
pub(super) async fn write_to(run_id: &str, user_msg: &Value) -> AppResult<()> {
    let run = get_run(run_id)?;
    run.write(user_msg).await?;
    run.state.lock().busy = true;
    Ok(())
}

/// Send a user message (text; optional image paths are attached as base64 image blocks).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_send(run_id: String, text: String, images: Option<Vec<String>>) -> AppResult<()> {
    let mut blocks = Vec::new();
    for p in images.unwrap_or_default() {
        blocks.push(load_image(&p).await?);
    }
    write_to(&run_id, &user_message(&text, &blocks)).await
}

/// Answer a can_use_tool request. `decision` = {behavior:"allow", updatedInput?, updatedPermissions?} | {behavior:"deny", message}.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_permission(run_id: String, request_id: String, decision: Value) -> AppResult<()> {
    let run = get_run(&run_id)?;
    let request = run
        .state
        .lock()
        .pending_permissions
        .remove(&request_id)
        .ok_or_else(|| AppError::msg(format!("No pending permission request {request_id}")))?;
    let mut decision = match decision {
        Value::Object(m) => m,
        _ => return Err(AppError::msg("decision must be an object")),
    };
    match decision.get("behavior").and_then(Value::as_str) {
        // The CLI requires updatedInput on allow; default to the tool's original input.
        Some("allow") => {
            if decision.get("updatedInput").is_none_or(Value::is_null) {
                decision.insert("updatedInput".into(), request.get("input").cloned().unwrap_or_else(|| json!({})));
            }
        }
        Some("deny") => {
            if decision.get("message").is_none_or(Value::is_null) {
                decision.insert("message".into(), "Denied by user".into());
            }
        }
        _ => return Err(AppError::msg("decision.behavior must be \"allow\" or \"deny\"")),
    }
    let reply = json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": decision }
    });
    if let Err(e) = run.write(&reply).await {
        run.state.lock().pending_permissions.insert(request_id, request);
        return Err(e);
    }
    Ok(())
}

/// Send a control request: subtype "interrupt" | "set_permission_mode" ({mode}) | "set_model" ({model}).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_control(run_id: String, subtype: String, payload: Option<Value>) -> AppResult<Value> {
    let run = get_run(&run_id)?;
    let res = run.control(&subtype, payload.as_ref(), CONTROL_TIMEOUT).await?;
    let field = |k: &str| payload.as_ref().and_then(|p| p.get(k)).and_then(Value::as_str).map(str::to_string);
    let mut st = run.state.lock();
    match subtype.as_str() {
        "set_permission_mode" => st.permission_mode = field("mode").or(st.permission_mode.take()),
        "set_model" => st.model = field("model").or(st.model.take()),
        _ => {}
    }
    Ok(res)
}

/// Close stdin, give claude `STOP_GRACE` to exit, then kill it.
async fn stop_run(run: Arc<Run>) {
    run.stopping.store(true, Ordering::SeqCst);
    run.close_stdin().await;
    let mut exited = run.exited.clone();
    if tokio::time::timeout(STOP_GRACE, exited.wait_for(|e| *e)).await.is_err() {
        if let Some(k) = run.kill.lock().take() {
            let _ = k.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), exited.wait_for(|e| *e)).await;
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_stop(run_id: String) -> AppResult<()> {
    let Some(run) = RUNS.lock().get(&run_id).cloned() else {
        return Ok(()); // already gone
    };
    stop_run(run).await;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub session_id: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub busy: bool,
    /// Outstanding can_use_tool requests (request_id → request body), for UI recovery.
    pub pending_permissions: HashMap<String, Value>,
}

/// Snapshot of a live run (session id from system/init, current model / mode, busy).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_run_info(run_id: String) -> AppResult<RunInfo> {
    let run = get_run(&run_id)?;
    let st = run.state.lock();
    Ok(RunInfo {
        session_id: st.session_id.clone(),
        cwd: run.cwd.clone(),
        model: st.model.clone(),
        permission_mode: st.permission_mode.clone(),
        busy: st.busy,
        pending_permissions: st.pending_permissions.clone(),
    })
}

/// Kill every run synchronously (app exit — the async runtime may not get another tick).
pub fn shutdown_all() {
    super::pty::shutdown_all();
    let runs: Vec<Arc<Run>> = RUNS.lock().drain().map(|(_, r)| r).collect();
    for run in runs {
        run.stopping.store(true, Ordering::SeqCst);
        if let Some(k) = run.kill.lock().take() {
            let _ = k.send(());
        }
        let Some(pid) = run.pid else { continue };
        // SIGTERM lets claude tear down its own children (MCP servers, shells).
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .status();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn args_default() {
        let a = build_args(&Spawn { prompt_tool: true, ..Default::default() });
        assert_eq!(
            a.join(" "),
            "-p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio"
        );
    }

    #[test]
    fn args_with_options() {
        let opts = StartOptions {
            model: Some("haiku".into()),
            permission_mode: Some("acceptEdits".into()),
            resume: Some("abc".into()),
            fork: true,
        };
        let a = build_args(&Spawn { opts, prompt_tool: true, extra_args: vec!["--max-turns".into(), "3".into()], ..Default::default() });
        let s = a.join(" ");
        assert!(s.ends_with("--model haiku --permission-mode acceptEdits --resume abc --fork-session --max-turns 3"), "{s}");
    }

    #[test]
    fn fork_requires_resume_and_empty_values_skipped() {
        let opts = StartOptions { model: Some(String::new()), fork: true, ..Default::default() };
        let a = build_args(&Spawn { opts, ..Default::default() });
        assert!(!a.iter().any(|x| x == "--fork-session" || x == "--model" || x == "--permission-prompt-tool"));
    }

    #[test]
    fn control_request_shape() {
        let v = control_request("r1", "set_permission_mode", Some(&json!({ "mode": "plan" })));
        assert_eq!(v, json!({ "type": "control_request", "request_id": "r1", "request": { "subtype": "set_permission_mode", "mode": "plan" } }));
        let v = control_request("r2", "interrupt", None);
        assert_eq!(v["request"], json!({ "subtype": "interrupt" }));
    }

    #[test]
    fn user_message_shape() {
        let v = user_message("hi", &[("image/png".into(), "AAAA".into())]);
        assert_eq!(v["type"], "user");
        let c = v["message"]["content"].as_array().unwrap();
        assert_eq!(c[0]["source"]["media_type"], "image/png");
        assert_eq!(c[1], json!({ "type": "text", "text": "hi" }));
        assert_eq!(image_media_type(Path::new("/a/b.JPG")), Some("image/jpeg"));
        assert_eq!(image_media_type(Path::new("/a/b.txt")), None);
    }

    #[test]
    fn state_tracks_session_and_permissions() {
        let mut st = RunState::default();
        assert!(st.handle_line("not json").msg.is_none());
        assert!(st.handle_line("   ").msg.is_none());
        let h = st.handle_line(r#"{"type":"system","subtype":"init","session_id":"S1","model":"claude-haiku","permissionMode":"manual"}"#);
        assert!(h.msg.is_some());
        assert_eq!(st.session_id.as_deref(), Some("S1"));
        assert_eq!(st.model.as_deref(), Some("claude-haiku"));
        assert_eq!(st.permission_mode.as_deref(), Some("manual"));

        let h = st.handle_line(
            r#"{"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"x"}}}"#,
        );
        assert!(h.reply.is_none());
        assert!(st.pending_permissions.contains_key("p1"));
        st.handle_line(r#"{"type":"control_cancel_request","request_id":"p1"}"#);
        assert!(st.pending_permissions.is_empty());

        st.handle_line(r#"{"type":"control_request","request_id":"p2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}"#);
        st.busy = true;
        let h = st.handle_line(r#"{"type":"result","subtype":"success","session_id":"S1"}"#);
        assert!(!st.busy && st.pending_permissions.is_empty() && !h.close_stdin);
    }

    #[test]
    fn unknown_control_requests_get_error_reply() {
        let mut st = RunState::default();
        let h = st.handle_line(r#"{"type":"control_request","request_id":"h1","request":{"subtype":"hook_callback"}}"#);
        let r = h.reply.unwrap();
        assert_eq!(r["response"]["subtype"], "error");
        assert_eq!(r["response"]["request_id"], "h1");
    }

    #[test]
    fn control_responses_resolve_waiters() {
        let mut st = RunState::default();
        let mut ok = st.add_waiter("a".into());
        let mut bad = st.add_waiter("b".into());
        let mut gone = st.add_waiter("c".into());
        st.handle_line(r#"{"type":"control_response","response":{"subtype":"success","request_id":"a","response":{"x":1}}}"#);
        st.handle_line(r#"{"type":"control_response","response":{"subtype":"error","request_id":"b","error":"nope"}}"#);
        assert_eq!(ok.try_recv().unwrap(), Ok(json!({ "x": 1 })));
        assert_eq!(bad.try_recv().unwrap(), Err("nope".to_string()));
        st.fail_waiters("exit");
        assert_eq!(gone.try_recv().unwrap(), Err("exit".to_string()));
    }

    type Rx = tokio::sync::mpsc::UnboundedReceiver<(&'static str, Value)>;

    pub(in crate::claude) fn channel_sink() -> (Sink, Rx) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(move |e, v| drop(tx.send((e, v)))), rx)
    }

    /// Next event matching `pred` (others are printed compactly and skipped).
    pub(in crate::claude) async fn wait_for(rx: &mut Rx, secs: u64, pred: impl Fn(&str, &Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let (e, v) = tokio::time::timeout_at(deadline, rx.recv()).await.expect("timed out waiting for event").expect("sink closed");
            let m = &v["msg"];
            if m["type"] != "stream_event" {
                println!("{e}: {} {}", m["type"].as_str().unwrap_or(""), m["subtype"].as_str().unwrap_or(""));
            }
            if pred(e, &v) {
                return v;
            }
        }
    }

    fn is_msg(e: &str, v: &Value, ty: &str) -> bool {
        e == "claude-msg" && v["msg"]["type"] == ty
    }

    /// Real run (costs a few cents): start / control / permission / interrupt / stop.
    #[tokio::test]
    #[ignore]
    async fn real_chat_run() {
        let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("devcat-chat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_str().unwrap().to_string();
        let (sink, mut rx) = channel_sink();
        let opts = StartOptions { model: Some("haiku".into()), permission_mode: Some("manual".into()), ..Default::default() };
        let spec = Spawn { opts, prompt_tool: true, extra_args: vec!["--max-budget-usd".into(), "0.10".into()], ..Default::default() };
        let id = spawn_run(sink, &cwd, spec).await.unwrap();

        // control round trips (no model call)
        let r = claude_control(id.clone(), "set_permission_mode".into(), Some(json!({ "mode": "plan" }))).await;
        println!("set_permission_mode plan → {r:?}");
        r.unwrap();
        claude_control(id.clone(), "set_permission_mode".into(), Some(json!({ "mode": "manual" }))).await.unwrap();
        assert_eq!(claude_run_info(id.clone()).await.unwrap().permission_mode.as_deref(), Some("manual"));
        assert!(claude_control(id.clone(), "bogus_subtype".into(), None).await.is_err());

        claude_send(id.clone(), "Use the Write tool to create hello.txt containing exactly: hi. Then reply with just: done".into(), None)
            .await
            .unwrap();
        let req = wait_for(&mut rx, 90, |e, v| is_msg(e, v, "control_request")).await;
        let rid = req["msg"]["request_id"].as_str().unwrap().to_string();
        assert_eq!(req["msg"]["request"]["tool_name"], "Write");
        let info = claude_run_info(id.clone()).await.unwrap();
        assert!(info.busy && info.pending_permissions.contains_key(&rid) && info.session_id.is_some());
        assert!(claude_permission(id.clone(), "nope".into(), json!({ "behavior": "allow" })).await.is_err());
        claude_permission(id.clone(), rid, json!({ "behavior": "allow" })).await.unwrap();
        let res = wait_for(&mut rx, 90, |e, v| is_msg(e, v, "result")).await;
        println!("result 1: {} cost {}", res["msg"]["result"], res["msg"]["total_cost_usd"]);
        assert!(std::fs::read_to_string(dir.join("hello.txt")).unwrap().contains("hi"));
        assert!(!claude_run_info(id.clone()).await.unwrap().busy);

        // interrupt a turn mid-stream
        claude_send(id.clone(), "Count from 1 to 300, one number per line, no tools.".into(), None).await.unwrap();
        wait_for(&mut rx, 60, |e, v| is_msg(e, v, "stream_event") && v["msg"]["event"]["type"] == "content_block_delta").await;
        let r = claude_control(id.clone(), "interrupt".into(), None).await;
        println!("interrupt → {r:?}");
        r.unwrap();
        let res = wait_for(&mut rx, 60, |e, v| is_msg(e, v, "result")).await;
        println!("result 2: subtype {} cost {}", res["msg"]["subtype"], res["msg"]["total_cost_usd"]);

        let session = claude_run_info(id.clone()).await.unwrap().session_id.unwrap();
        claude_stop(id.clone()).await.unwrap();
        let exit = wait_for(&mut rx, 10, |e, _| e == "claude-exit").await;
        println!("exit: {exit}");
        assert!(exit["error"].is_null());
        assert!(claude_run_info(id).await.is_err());

        let sessions = super::super::sessions::claude_sessions(cwd.clone()).await.unwrap();
        println!("sessions: {:?}", sessions.iter().map(|s| (&s.id, &s.title, s.message_count, s.cost_usd)).collect::<Vec<_>>());
        assert!(sessions.iter().any(|s| s.id == session));
        let t = super::super::sessions::claude_transcript(cwd, session).await.unwrap();
        assert!(t.len() >= 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// claude_info: binary, version, account via the free initialize handshake.
    #[tokio::test]
    #[ignore]
    async fn real_info() {
        let i = claude_info().await.unwrap();
        println!("path {:?} version {:?} account {}", i.path, i.version, i.account.clone().unwrap_or_default());
        assert!(i.path.is_some() && i.version.is_some() && i.account.is_some() && i.models.is_some());
    }

    #[test]
    fn one_shot_closes_after_result() {
        let mut st = RunState { close_after_result: true, ..Default::default() };
        assert!(!st.handle_line(r#"{"type":"assistant","session_id":"S"}"#).close_stdin);
        assert_eq!(st.session_id.as_deref(), Some("S"));
        assert!(st.handle_line(r#"{"type":"result","subtype":"success"}"#).close_stdin);
    }
}
