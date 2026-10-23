//! DevCat Agent in the app: runs `crates/harness` sessions for the AI workspace's chat tabs.
//! Everything a turn does is sent to the window as `agent-event` ({ sessionId, event }); tool calls
//! that need approval wait for `agent_approve` (closing the tab or stopping counts as a no).
//! Paired phones open the same chats (remote/dispatch.rs): the events of a running turn are kept,
//! numbered, so a chat opened in the middle of a turn catches up; `agent-chat` tells them when a
//! chat starts working, needs the user or finishes.
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use devcat_harness::approval::{ApprovalRequest, Decision, Question};
use devcat_harness::mcp::{McpTool, Server};
use devcat_harness::prompt::{Environment, PROJECT_RULES, USER_RULES, home_dir, skills, system_prompt};
use devcat_harness::tools::Registry;
use devcat_harness::{Agent, AgentEvent, Approver, Block, Cancel, Llm, Policy, Session, SessionEvent, Usage};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::oneshot;

static DIR: OnceCell<PathBuf> = OnceCell::new();
static RUNS: Lazy<Mutex<HashMap<String, Arc<Run>>>> = Lazy::new(Default::default);

/// Sessions are kept in `<data>/agent-sessions/<id>.jsonl`.
pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir.join("agent-sessions"));
}

fn dir() -> AppResult<&'static PathBuf> {
    DIR.get().ok_or_else(|| AppError::msg("DevCat Agent is not initialised"))
}

struct Run {
    id: String,
    cwd: String,
    provider_id: String,
    provider_name: String,
    /// Where the chat was opened: "desktop" or "phone:<name>".
    origin: String,
    /// Who opened it: "desktop" or the phone's device id (only they end it).
    owner: String,
    /// The session as of the last finished turn (a running turn holds `turn`).
    log: Mutex<Snapshot>,
    /// When it last started or finished working (ms).
    since: AtomicU64,
    /// The connection's model API (with its key); the chat can switch `model`.
    cfg: devcat_harness::LlmConfig,
    turn: tokio::sync::Mutex<(Agent, Session)>,
    cancel: Mutex<Cancel>,
    approver: Arc<UiApprover>,
    busy: AtomicBool,
    /// What the agent loaded for this folder (shown in the chat).
    context: Mutex<AgentContext>,
    /// MCP server processes (stopped when the chat closes).
    mcp: Mutex<Vec<Arc<Server>>>,
}

/// Rules, skills and MCP servers the agent found for the folder.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentContext {
    pub rules: Vec<String>,
    pub skills: Vec<String>,
    pub mcp: Vec<McpInfo>,
    /// Problems worth telling the user (an MCP server that didn't start, ...).
    pub notices: Vec<String>,
    /// MCP servers are still starting (their tools come when they're ready).
    pub mcp_loading: bool,
    /// DevCat connections the agent can use (names).
    pub connections: Vec<String>,
    /// Hook files that run in this chat ("~/.claude/settings.json (2)").
    pub hooks: Vec<String>,
    /// Hooks the user hasn't allowed for this folder yet (or that changed since): they don't run
    /// until the user allows them on the computer.
    pub hooks_review: Option<HooksReview>,
}

/// Hooks waiting for the user's OK: what they'd run.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HooksReview {
    pub files: Vec<String>,
    /// "PreToolUse (Bash): ./check.sh"
    pub commands: Vec<String>,
    /// What wouldn't run anyway (other events and hook types).
    pub skipped: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    pub name: String,
    pub tools: usize,
}

#[derive(Clone, Default)]
struct Snapshot {
    events: Vec<SessionEvent>,
    title: String,
    usage: Usage,
    model: String,
}

impl Snapshot {
    fn of(agent: &Agent, session: &Session) -> Self {
        Snapshot { events: session.events().to_vec(), title: session.title(), usage: session.usage(), model: agent.model.name() }
    }
}

/// Events of the running turn, numbered (`seq`) so a chat opened meanwhile skips what it got.
#[derive(Default)]
struct Live {
    seq: u64,
    events: Vec<Value>,
}

fn emit(session_id: &str, event: Value) {
    crate::events::emit("agent-event", json!({ "sessionId": session_id, "event": event }));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Asks the window: sends the request as an event and waits for `agent_approve` (or, for a
/// question, `agent_answer`).
struct UiApprover {
    session_id: String,
    pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
    questions: Mutex<HashMap<String, oneshot::Sender<String>>>,
    live: Mutex<Live>,
}

#[async_trait]
impl Approver for UiApprover {
    async fn decide(&self, req: &ApprovalRequest) -> (Decision, String) {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(req.tool_use_id.clone(), tx);
        self.send(json!({ "type": "approval", "request": req }));
        chat_changed(&self.session_id);
        match rx.await {
            Ok(d) => (d, "user".into()),
            // the tab was closed or the turn stopped: fail closed
            Err(_) => (Decision::Deny { reason: Some("Nobody answered.".into()) }, "closed".into()),
        }
    }

    async fn ask(&self, q: &Question) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        self.questions.lock().insert(q.tool_use_id.clone(), tx);
        self.send(json!({ "type": "question", "question": q }));
        chat_changed(&self.session_id);
        rx.await.ok()
    }
}

impl UiApprover {
    fn deny_all(&self) {
        self.pending.lock().clear();
        self.questions.lock().clear();
    }

    /// Send an event of the turn, and keep it for chats opened before the turn ends.
    fn send(&self, event: Value) {
        self.send_with(&mut self.live.lock(), event);
    }

    /// `send` with `live` already locked.
    fn send_with(&self, live: &mut Live, event: Value) {
        live.seq += 1;
        if event["type"] == "turnStart" {
            live.events.clear();
        }
        live.events.push(event.clone());
        crate::events::emit("agent-event", json!({ "sessionId": self.session_id, "event": event, "seq": live.seq }));
    }

    /// What the chat waits for: the command or edit to approve, or the question.
    fn waiting(&self) -> Option<String> {
        let (pending, questions) = (self.pending.lock(), self.questions.lock());
        self.live.lock().events.iter().rev().find_map(|e| match e["type"].as_str() {
            Some("approval") if pending.contains_key(e["request"]["toolUseId"].as_str()?) => {
                let verb = match e["request"]["kind"].as_str() {
                    Some("exec") => "Run",
                    Some("edit") | Some("edit-outside") => "Edit",
                    _ => "Allow",
                };
                Some(format!("{verb}: {}", e["request"]["summary"].as_str()?))
            }
            Some("question") if questions.contains_key(e["question"]["toolUseId"].as_str()?) => e["question"]["question"].as_str().map(String::from),
            _ => None,
        })
    }
}

/// A chat that's open (in a tab or on a phone), for the phone's list.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatInfo {
    pub session_id: String,
    pub cwd: String,
    pub title: String,
    pub provider_id: String,
    pub provider_name: String,
    pub origin: String,
    pub busy: bool,
    /// What it asks the user, while it waits.
    pub waiting: Option<String>,
    pub since: u64,
}

fn info(run: &Run) -> ChatInfo {
    ChatInfo {
        session_id: run.id.clone(),
        cwd: run.cwd.clone(),
        title: run.log.lock().title.clone(),
        provider_id: run.provider_id.clone(),
        provider_name: run.provider_name.clone(),
        origin: run.origin.clone(),
        busy: run.busy.load(Ordering::SeqCst),
        waiting: run.approver.waiting(),
        since: run.since.load(Ordering::SeqCst),
    }
}

/// Open chats that have been used, newest activity first.
pub fn list() -> Vec<ChatInfo> {
    let runs: Vec<Arc<Run>> = RUNS.lock().values().cloned().collect();
    let mut out: Vec<ChatInfo> = runs
        .iter()
        .filter(|r| r.busy.load(Ordering::SeqCst) || r.log.lock().events.iter().any(|e| matches!(e, SessionEvent::User { .. })))
        .map(|r| info(r))
        .collect();
    out.sort_by(|a, b| b.since.cmp(&a.since));
    out
}

/// Who opened a chat ("desktop" / a phone's device id), if it's open.
pub fn owner(session_id: &str) -> Option<String> {
    RUNS.lock().get(session_id).map(|r| r.owner.clone())
}

/// A chat that's running, or saved on disk.
pub fn exists(session_id: &str) -> bool {
    RUNS.lock().contains_key(session_id) || (valid_id(session_id) && dir().is_ok_and(|d| d.join(format!("{session_id}.jsonl")).exists()))
}

/// Tell phones a chat started or stopped working or waiting.
fn chat_changed(session_id: &str) {
    let run = RUNS.lock().get(session_id).cloned();
    if let Some(run) = run {
        crate::events::emit("agent-chat", info(&run));
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOpened {
    pub session_id: String,
    pub title: String,
    pub model: String,
    pub provider_name: String,
    pub busy: bool,
    /// The session so far (the chat view rebuilds itself from it).
    pub events: Vec<SessionEvent>,
    pub usage: Usage,
    pub context: AgentContext,
    /// While a turn runs: its events so far (applied after `events`), and the number of the last
    /// one (later `agent-event`s carry higher ones).
    pub live: Vec<Value>,
    pub seq: u64,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Hooks run commands on this computer, so they run only in folders where the user allowed the
/// hooks as they are now: `<data>/agent-hooks.json` maps a folder to the digest of its hooks.
fn hooks_file() -> AppResult<PathBuf> {
    Ok(dir()?.parent().unwrap_or(dir()?).join("agent-hooks.json"))
}

fn folder_key(cwd: &str) -> String {
    let p = cwd.trim_end_matches(['/', '\\']).replace('\\', "/");
    if cfg!(windows) { p.to_lowercase() } else { p }
}

fn hooks_trusted(cwd: &str, digest: &str) -> bool {
    let Ok(text) = hooks_file().and_then(|f| std::fs::read_to_string(f).map_err(Into::into)) else { return false };
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    !digest.is_empty() && v["trusted"][folder_key(cwd)].as_str() == Some(digest)
}

fn trust_hooks(cwd: &str, digest: &str) -> AppResult<()> {
    let file = hooks_file()?;
    let mut v: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    if !v["trusted"].is_object() {
        v["trusted"] = json!({});
    }
    v["trusted"][folder_key(cwd)] = json!(digest);
    std::fs::write(&file, serde_json::to_string_pretty(&v).unwrap_or_default())?;
    Ok(())
}

/// The hooks for `cwd`: (the ones that run, what the chat shows).
fn hooks_for(cwd: &str) -> (devcat_harness::hooks::Hooks, Vec<String>, Option<HooksReview>) {
    let found = devcat_harness::hooks::load(std::path::Path::new(cwd), home_dir().as_deref());
    if found.hooks.is_empty() {
        return Default::default();
    }
    let files = found.files.iter().map(|(f, n)| format!("{f} ({n})")).collect();
    if hooks_trusted(cwd, &found.digest) {
        (found.hooks, files, None)
    } else {
        (Default::default(), vec![], Some(HooksReview { files, commands: found.commands, skipped: found.skipped }))
    }
}

/// Allow the hooks of a chat's folder as they are now (the chat shows them first). Every open chat
/// in that folder runs them from its next step on.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_trust_hooks(session_id: String) -> AppResult<()> {
    let run = run_of(&session_id)?;
    let found = devcat_harness::hooks::load(std::path::Path::new(&run.cwd), home_dir().as_deref());
    if found.digest.is_empty() {
        return Ok(());
    }
    trust_hooks(&run.cwd, &found.digest)?;
    let key = folder_key(&run.cwd);
    let runs: Vec<Arc<Run>> = RUNS.lock().values().filter(|r| folder_key(&r.cwd) == key).cloned().collect();
    for run in runs {
        let (hooks, files, review) = hooks_for(&run.cwd);
        let ctx = {
            let mut c = run.context.lock();
            c.hooks = files;
            c.hooks_review = review;
            c.clone()
        };
        emit(&run.id, json!({ "type": "context", "context": ctx }));
        // between turns: a running turn keeps what it started with
        crate::rt::spawn(async move {
            run.turn.lock().await.0.hooks = Arc::new(hooks);
        });
    }
    Ok(())
}

/// MCP servers get this long to start; the chat works without them meanwhile.
const MCP_START: std::time::Duration = std::time::Duration::from_secs(25);

async fn build(cwd: &str, provider_id: &str, session: Session, origin: &str, owner: &str) -> AppResult<Arc<Run>> {
    let (provider, cfg) = crate::ai::agent_config(provider_id)?;
    let id = session.id.clone();
    let approver = Arc::new(UiApprover { session_id: id.clone(), pending: Mutex::default(), questions: Mutex::default(), live: Mutex::default() });
    let dir = std::path::Path::new(cwd);
    let home = home_dir();
    let shell = if cfg!(windows) { "PowerShell" } else { "bash" };
    let system = system_prompt(&Environment { cwd: dir, os: std::env::consts::OS, shell, date: &today() });

    // What the agent picks up from the folder, for the chat to show.
    let mut context = AgentContext::default();
    if let Some(h) = &home {
        context.rules.extend(USER_RULES.iter().filter(|r| h.join(r).exists()).map(|r| format!("~/{r}")));
    }
    context.rules.extend(PROJECT_RULES.iter().filter(|r| dir.join(r).exists()).map(|r| r.to_string()));
    context.skills = skills(dir, home.as_deref()).into_iter().map(|s| s.name).collect();
    let configured = !devcat_harness::mcp::configured(dir, home.as_deref()).0.is_empty();
    context.mcp_loading = configured;
    // the user's DevCat connections (databases, Redis, MongoDB, clusters) and notes
    let mut tools = Registry::standard();
    crate::agent_tools::add(&mut tools);
    crate::agent_notes::add(&mut tools);
    context.connections = crate::agent_tools::names();
    let (hooks, hook_files, review) = hooks_for(cwd);
    (context.hooks, context.hooks_review) = (hook_files, review);
    let mut agent = Agent::new(Arc::new(Llm::new(cfg.clone())), tools, Policy::Ask, approver.clone(), system);
    agent.hooks = Arc::new(hooks);
    let log = Snapshot::of(&agent, &session);
    let run = Arc::new(Run {
        id,
        cwd: cwd.to_string(),
        provider_id: provider_id.to_string(),
        provider_name: provider.name,
        origin: origin.to_string(),
        owner: owner.to_string(),
        log: Mutex::new(log),
        since: AtomicU64::new(now_ms()),
        cfg,
        turn: tokio::sync::Mutex::new((agent, session)),
        cancel: Mutex::new(Cancel::default()),
        approver,
        busy: AtomicBool::new(false),
        context: Mutex::new(context),
        mcp: Mutex::default(),
    });
    // MCP servers start in the background: the chat opens right away and gets their tools when
    // they're ready (between turns, never in the middle of one).
    if configured {
        let (run2, dir, home) = (Arc::downgrade(&run), dir.to_path_buf(), home.clone());
        tokio::spawn(async move {
            let (servers, errors) = match tokio::time::timeout(MCP_START, devcat_harness::mcp::connect_all(&dir, home.as_deref())).await {
                Ok(r) => r,
                Err(_) => (vec![], vec![format!("MCP servers took longer than {} s to start; this chat runs without them.", MCP_START.as_secs())]),
            };
            let Some(run) = run2.upgrade() else { return };
            {
                let mut g = run.turn.lock().await;
                for s in &servers {
                    for t in McpTool::all(s) {
                        g.0.tools.add(t);
                    }
                }
            }
            let ctx = {
                let mut c = run.context.lock();
                c.notices.extend(errors);
                c.mcp = servers.iter().map(|s| McpInfo { name: s.name.clone(), tools: s.tools.len() }).collect();
                c.mcp_loading = false;
                c.clone()
            };
            *run.mcp.lock() = servers;
            emit(&run.id, json!({ "type": "context", "context": ctx }));
        });
    }
    Ok(run)
}

/// Open a chat: a new session, or `session_id` to continue one (still running or from disk).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_open(cwd: String, provider_id: String, session_id: Option<String>) -> AppResult<AgentOpened> {
    open(cwd, provider_id, session_id, "desktop", "desktop").await
}

/// `agent_open` for the window or a phone (`origin`: "desktop" / "phone:<name>", `owner`:
/// "desktop" / the phone's device id; both kept from whoever opened the chat first).
pub async fn open(cwd: String, provider_id: String, session_id: Option<String>, origin: &str, owner: &str) -> AppResult<AgentOpened> {
    let existing = session_id.as_ref().and_then(|id| RUNS.lock().get(id).cloned());
    let run = match (existing, session_id) {
        (Some(r), _) => r,
        (None, Some(id)) if valid_id(&id) && dir()?.join(format!("{id}.jsonl")).exists() => {
            let session = Session::open(&dir()?.join(format!("{id}.jsonl"))).await.map_err(|e| AppError::msg(e.to_string()))?;
            build(&cwd, &provider_id, session, origin, owner).await?
        }
        _ => build(&cwd, &provider_id, Session::create(dir()?, &cwd).await.map_err(|e| AppError::msg(e.to_string()))?, origin, owner).await?,
    };
    RUNS.lock().insert(run.id.clone(), run.clone());
    // While a turn runs: the session as it was when the turn started, and the turn's events so
    // far. A turn ends (log updated, `live` cleared, `turnEnd` sent, not busy) all under `live`,
    // so holding it here, `log` and `live` never both hold the same turn. (Locks: `live`, then
    // `turn` without waiting, then `log`.)
    let (snap, live, seq, busy) = {
        let l = run.approver.live.lock();
        let busy = run.busy.load(Ordering::SeqCst);
        if busy {
            (run.log.lock().clone(), l.events.clone(), l.seq, busy)
        } else {
            let snap = match run.turn.try_lock() {
                Ok(g) => Snapshot::of(&g.0, &g.1),
                Err(_) => run.log.lock().clone(),
            };
            (snap, vec![], l.seq, busy)
        }
    };
    Ok(AgentOpened {
        session_id: run.id.clone(),
        title: snap.title,
        model: snap.model,
        provider_name: run.provider_name.clone(),
        busy,
        events: snap.events,
        usage: snap.usage,
        context: run.context.lock().clone(),
        live,
        seq,
    })
}

/// A chat's log for the Trace view: each line as written (with `ms`, when it was written), and the
/// logs of the helpers it started, by file name (`<session>.agents/<call>.jsonl`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTrace {
    pub events: Vec<Value>,
    pub helpers: HashMap<String, Vec<Value>>,
    /// The session file.
    pub path: String,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_trace(session_id: String) -> AppResult<AgentTrace> {
    if !valid_id(&session_id) {
        return Err(AppError::msg("Not a DevCat Agent chat"));
    }
    let path = dir()?.join(format!("{session_id}.jsonl"));
    if !path.exists() {
        return Err(AppError::msg("This chat isn't saved yet: send a message first"));
    }
    let events = trace_lines(&path).await?;
    let mut helpers = HashMap::new();
    if let Ok(mut rd) = tokio::fs::read_dir(path.with_extension("agents")).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            let p = e.path();
            if let (Some(name), Some("jsonl")) = (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|x| x.to_str())) {
                helpers.insert(name.to_string(), trace_lines(&p).await.unwrap_or_default());
            }
        }
    }
    Ok(AgentTrace { events, helpers, path: path.to_string_lossy().into_owned() })
}

/// A session file's lines, without the attached images' data (their size stays).
async fn trace_lines(path: &std::path::Path) -> AppResult<Vec<Value>> {
    let text = tokio::fs::read_to_string(path).await?;
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|mut v| {
            for image in v.get_mut("images").and_then(Value::as_array_mut).into_iter().flatten() {
                if let Some(data) = image.get_mut("data") {
                    *data = json!(format!("[{} characters of base64]", data.as_str().map_or(0, str::len)));
                }
            }
            v
        })
        .collect())
}

/// Rename a chat: a `title` event at the end of its session file (the last one names the chat).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_rename(session_id: String, title: String) -> AppResult<()> {
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
    if title.is_empty() {
        return Err(AppError::msg("The name can't be empty"));
    }
    if !valid_id(&session_id) {
        return Err(AppError::msg("Not a DevCat Agent chat"));
    }
    // An open chat writes it through its own session, so the title it reports stays the new one;
    // during a turn that happens when the turn ends.
    if let Ok(run) = run_of(&session_id) {
        crate::rt::spawn(async move {
            let mut turn = run.turn.lock().await;
            if turn.1.append(SessionEvent::Title { title: title.clone() }).await.is_ok() {
                run.log.lock().title = title;
            }
            drop(turn);
            chat_changed(&run.id);
        });
        return Ok(());
    }
    let path = dir()?.join(format!("{session_id}.jsonl"));
    if !path.exists() {
        return Err(AppError::msg("That chat isn't saved yet: send a message first"));
    }
    let mut line = serde_json::to_string(&SessionEvent::Title { title }).map_err(|e| AppError::msg(e.to_string()))?;
    line.push('\n');
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new().append(true).open(&path).await?;
    f.write_all(line.as_bytes()).await?;
    f.flush().await?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn run_of(session_id: &str) -> AppResult<Arc<Run>> {
    RUNS.lock().get(session_id).cloned().ok_or_else(|| AppError::msg("This chat is closed. Open it again."))
}

/// Images the model is shown directly (other files are attached by path only).
const IMAGE_TYPES: &[(&str, &str)] = &[("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];
const MAX_IMAGE: u64 = 5 * 1024 * 1024;
const MAX_IMAGES: usize = 8;
const MAX_ATTACHMENT: usize = 50 * 1024 * 1024;

/// Save a file dropped or pasted into the chat (the window has its bytes, not a path); returns
/// the path to attach.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_attach(session_id: String, name: String, data: String) -> AppResult<String> {
    use base64::Engine;
    if !valid_id(&session_id) {
        return Err(AppError::msg("Invalid chat"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|e| AppError::msg(format!("Couldn't read the file: {e}")))?;
    if bytes.len() > MAX_ATTACHMENT {
        return Err(AppError::msg("Files over 50 MB can't be attached. Put it in the project folder instead."));
    }
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " ._-()".contains(c) { c } else { '_' }).collect();
    let safe = if safe.trim().is_empty() || safe.starts_with('.') { format!("file{safe}") } else { safe };
    let folder = attachments_dir(&session_id)?;
    tokio::fs::create_dir_all(&folder).await?;
    // don't overwrite an earlier attachment with the same name
    let mut path = folder.join(&safe);
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = folder.join(format!("{n}-{safe}"));
    }
    tokio::fs::write(&path, bytes).await?;
    Ok(path.to_string_lossy().into_owned())
}

/// Where a chat's dropped or pasted files are saved: `<data>/agent-attachments/<id>`.
pub fn attachments_dir(session_id: &str) -> AppResult<PathBuf> {
    if !valid_id(session_id) {
        return Err(AppError::msg("Invalid chat"));
    }
    Ok(dir()?.parent().unwrap_or(dir()?).join("agent-attachments").join(session_id))
}

/// The message as the model gets it: the text, a list of the attached files, and the images.
async fn with_attachments(text: &str, paths: &[String]) -> (String, Vec<Block>) {
    if paths.is_empty() {
        return (text.to_string(), vec![]);
    }
    use base64::Engine;
    let mut images = Vec::new();
    let mut lines = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        let size = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let image = IMAGE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t);
        let mut note = String::new();
        if let Some(media_type) = image {
            if size <= MAX_IMAGE && images.len() < MAX_IMAGES {
                if let Ok(bytes) = tokio::fs::read(path).await {
                    images.push(Block::Image { media_type: media_type.into(), data: base64::engine::general_purpose::STANDARD.encode(bytes) });
                    note = ", shown to you below".into();
                }
            } else {
                note = ", too large to show: inspect it with a script if needed".into();
            }
        }
        lines.push(format!("- {p} ({}{note})", human_size(size)));
    }
    let text = if text.trim().is_empty() { "See the attached files." } else { text };
    (format!("{text}\n\nAttached files:\n{}", lines.join("\n")), images)
}

fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.0} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Answer the model's question (`ask_user`).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_answer(session_id: String, tool_use_id: String, answer: String) -> AppResult<()> {
    let run = run_of(&session_id)?;
    let tx = run.approver.questions.lock().remove(&tool_use_id).ok_or_else(|| AppError::msg("That question was already answered"))?;
    let _ = tx.send(answer);
    chat_changed(&session_id);
    Ok(())
}

/// Send a message; the turn runs in the background and reports through `agent-event`.
/// `model` switches the chat's model from this message on (None = keep it). `attachments` are
/// file paths: images are shown to the model, every file is listed with its path.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_send(session_id: String, text: String, policy: Policy, model: Option<String>, attachments: Option<Vec<String>>, context_window: Option<u64>) -> AppResult<()> {
    let run = run_of(&session_id)?;
    let attachments = attachments.unwrap_or_default();
    if text.trim().is_empty() && attachments.is_empty() {
        return Err(AppError::msg("Type a message first"));
    }
    if run.busy.swap(true, Ordering::SeqCst) {
        return Err(AppError::msg("The agent is still working. Stop it or wait for it to finish."));
    }
    let cancel = Cancel::default();
    *run.cancel.lock() = cancel.clone();
    run.since.store(now_ms(), Ordering::SeqCst);
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let ap = run.approver.clone();
        ap.send(json!({ "type": "turnStart", "text": text, "attachments": attachments }));
        chat_changed(&run.id);
        let (message, images) = with_attachments(&text, &attachments).await;
        let mut g = run.turn.lock().await;
        let (agent, session) = &mut *g;
        agent.policy = policy;
        if let Some(m) = model.filter(|m| !m.trim().is_empty() && *m != agent.model.name()) {
            agent.model = Arc::new(Llm::new(devcat_harness::LlmConfig { model: m, ..run.cfg.clone() }));
        }
        agent.context_window = context_window.filter(|&w| w > 0);
        let mut on = |e: AgentEvent| ap.send(serde_json::to_value(&e).unwrap_or(Value::Null));
        // `/compact [what to keep]`: summarize the conversation now instead of sending a message.
        let compact = text.trim().strip_prefix("/compact").filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
        let res = match compact {
            Some(focus) if attachments.is_empty() => agent.compact(session, Some(focus), &cancel, &mut on).await.map(|_| devcat_harness::TurnEnd::Answered),
            _ => agent.turn_with_images(session, &message, images, &cancel, &mut on).await,
        };
        let (end, error) = match res {
            Ok(end) => (serde_json::to_value(end).unwrap_or(Value::Null), None),
            Err(devcat_harness::Error::Cancelled) => (json!("stopped"), None),
            Err(e) => (json!("error"), Some(e.to_string())),
        };
        let snap = Snapshot::of(agent, session);
        drop(g);
        // All at once for `open`: the turn moves from `live` to `log`, and the chat is done.
        {
            let mut live = ap.live.lock();
            let (usage, title) = (snap.usage.clone(), snap.title.clone());
            *run.log.lock() = snap;
            ap.send_with(&mut live, json!({ "type": "turnEnd", "end": end, "error": error, "usage": usage, "title": title, "elapsedMs": started.elapsed().as_millis() as u64 }));
            live.events.clear();
            run.since.store(now_ms(), Ordering::SeqCst);
            run.busy.store(false, Ordering::SeqCst);
        }
        chat_changed(&run.id);
    });
    Ok(())
}

/// Stop the running turn (pending approvals count as a no).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_stop(session_id: String) -> AppResult<()> {
    let run = run_of(&session_id)?;
    run.cancel.lock().cancel();
    run.approver.deny_all();
    Ok(())
}

/// Answer an approval request.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_approve(session_id: String, tool_use_id: String, decision: Decision) -> AppResult<()> {
    let run = run_of(&session_id)?;
    let tx = run.approver.pending.lock().remove(&tool_use_id).ok_or_else(|| AppError::msg("That request was already answered"))?;
    let _ = tx.send(decision);
    chat_changed(&session_id);
    Ok(())
}

/// A chat's tab closed: forget it and stop a running turn, unless a phone started it (that
/// phone ends it).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_close(session_id: String) -> AppResult<()> {
    if owner(&session_id).is_some_and(|o| o != "desktop") {
        return Ok(());
    }
    close(&session_id)
}

/// End a chat: forget it; a running turn is stopped.
pub fn close(session_id: &str) -> AppResult<()> {
    let run = RUNS.lock().remove(session_id);
    if let Some(run) = run {
        run.cancel.lock().cancel();
        run.approver.deny_all();
        crate::events::emit("agent-chat-gone", json!({ "sessionId": session_id }));
    }
    Ok(())
}

/// A saved chat, for the Sessions list.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionInfo {
    pub id: String,
    pub title: String,
    pub updated_ms: u64,
    /// Messages the user sent.
    pub messages: usize,
}

/// Saved DevCat Agent chats for `cwd`, newest first. Only reads the files.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_sessions(cwd: String) -> AppResult<Vec<AgentSessionInfo>> {
    let dir = dir()?.clone();
    let same = |a: &str| {
        let norm = |p: &str| {
            let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
            if cfg!(windows) { p.to_lowercase() } else { p }
        };
        norm(a) == norm(&cwd)
    };
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return Ok(out) };
    while let Ok(Some(e)) = rd.next_entry().await {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = tokio::fs::read_to_string(&path).await else { continue };
        let mut lines = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok());
        let Some(meta) = lines.next() else { continue };
        if meta["event"] != "meta" || !same(meta["cwd"].as_str().unwrap_or_default()) {
            continue;
        }
        let (mut first, mut title, mut messages) = (None::<String>, None::<String>, 0);
        for v in lines {
            match v["event"].as_str() {
                // messages from hooks (a Stop hook sending the agent on) aren't the user's
                Some("user") if v["hook"].is_null() => {
                    messages += 1;
                    if first.is_none() {
                        let t = v["text"].as_str().unwrap_or_default();
                        let t = t.split("\n\nAttached files:").next().unwrap_or(t).trim();
                        first = Some(if t.is_empty() { "Attached files".into() } else { t.lines().next().unwrap_or(t).chars().take(80).collect() });
                    }
                }
                Some("title") => title = v["title"].as_str().map(String::from),
                _ => {}
            }
        }
        if messages == 0 {
            continue; // opened but never used
        }
        let updated_ms = e.metadata().await.ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
        out.push(AgentSessionInfo { id: meta["id"].as_str().unwrap_or_default().to_string(), title: title.or(first).unwrap_or_default(), updated_ms, messages });
    }
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

/// Stop every running turn (app exit).
pub fn shutdown_all() {
    for run in RUNS.lock().drain().map(|(_, r)| r) {
        run.cancel.lock().cancel();
        run.approver.deny_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_events_are_numbered_kept_per_turn_and_show_what_waits() {
        let ap = UiApprover { session_id: "S".into(), pending: Mutex::default(), questions: Mutex::default(), live: Mutex::default() };
        ap.send(json!({ "type": "turnStart", "text": "hi" }));
        ap.send(json!({ "type": "approval", "request": { "toolUseId": "t1", "kind": "exec", "summary": "cargo test" } }));
        assert_eq!(ap.live.lock().seq, 2);
        assert_eq!(ap.live.lock().events.len(), 2);
        // only while the request is still open
        assert_eq!(ap.waiting(), None);
        let (tx, _rx) = oneshot::channel();
        ap.pending.lock().insert("t1".into(), tx);
        assert_eq!(ap.waiting().as_deref(), Some("Run: cargo test"));
        ap.send(json!({ "type": "question", "question": { "toolUseId": "q1", "question": "Which one?" } }));
        let (tx, _rx2) = oneshot::channel();
        ap.questions.lock().insert("q1".into(), tx);
        assert_eq!(ap.waiting().as_deref(), Some("Which one?"));
        ap.deny_all();
        assert_eq!(ap.waiting(), None);
        // a new turn starts a new list; the numbers keep counting
        ap.send(json!({ "type": "turnStart", "text": "again" }));
        let live = ap.live.lock();
        assert_eq!((live.seq, live.events.len()), (4, 1));
    }
}
