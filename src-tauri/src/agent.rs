//! DevCat Agent in the app: runs `crates/harness` sessions for the AI workspace's chat tabs.
//! Everything a turn does is sent to the window as `agent-event` ({ sessionId, event }); tool calls
//! that need approval wait for `agent_approve` (closing the tab or stopping counts as a no).
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
use std::sync::atomic::{AtomicBool, Ordering};
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
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    pub name: String,
    pub tools: usize,
}

fn emit(session_id: &str, event: Value) {
    crate::events::emit("agent-event", json!({ "sessionId": session_id, "event": event }));
}

/// Asks the window: sends the request as an event and waits for `agent_approve` (or, for a
/// question, `agent_answer`).
struct UiApprover {
    session_id: String,
    pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
    questions: Mutex<HashMap<String, oneshot::Sender<String>>>,
}

#[async_trait]
impl Approver for UiApprover {
    async fn decide(&self, req: &ApprovalRequest) -> (Decision, String) {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(req.tool_use_id.clone(), tx);
        emit(&self.session_id, json!({ "type": "approval", "request": req }));
        match rx.await {
            Ok(d) => (d, "user".into()),
            // the tab was closed or the turn stopped: fail closed
            Err(_) => (Decision::Deny { reason: Some("Nobody answered.".into()) }, "closed".into()),
        }
    }

    async fn ask(&self, q: &Question) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        self.questions.lock().insert(q.tool_use_id.clone(), tx);
        emit(&self.session_id, json!({ "type": "question", "question": q }));
        rx.await.ok()
    }
}

impl UiApprover {
    fn deny_all(&self) {
        self.pending.lock().clear();
        self.questions.lock().clear();
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
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// MCP servers get this long to start; the chat works without them meanwhile.
const MCP_START: std::time::Duration = std::time::Duration::from_secs(25);

async fn build(cwd: &str, provider_id: &str, session: Session) -> AppResult<Arc<Run>> {
    let (_, cfg) = crate::ai::agent_config(provider_id)?;
    let id = session.id.clone();
    let approver = Arc::new(UiApprover { session_id: id.clone(), pending: Mutex::default(), questions: Mutex::default() });
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
    // the user's DevCat connections: databases, Redis, MongoDB, clusters
    let mut tools = Registry::standard();
    crate::agent_tools::add(&mut tools);
    context.connections = crate::agent_tools::names();
    let agent = Agent::new(Arc::new(Llm::new(cfg.clone())), tools, Policy::Ask, approver.clone(), system);
    let run = Arc::new(Run {
        id,
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
    let (provider, cfg) = crate::ai::agent_config(&provider_id)?;
    let existing = session_id.as_ref().and_then(|id| RUNS.lock().get(id).cloned());
    let run = match (existing, session_id) {
        (Some(r), _) => r,
        (None, Some(id)) if valid_id(&id) && dir()?.join(format!("{id}.jsonl")).exists() => {
            let session = Session::open(&dir()?.join(format!("{id}.jsonl"))).await.map_err(|e| AppError::msg(e.to_string()))?;
            build(&cwd, &provider_id, session).await?
        }
        _ => build(&cwd, &provider_id, Session::create(dir()?, &cwd).await.map_err(|e| AppError::msg(e.to_string()))?).await?,
    };
    RUNS.lock().insert(run.id.clone(), run.clone());
    let busy = run.busy.load(Ordering::SeqCst);
    // a running turn holds the lock: send what we can without waiting for it
    let (events, title, usage) = match run.turn.try_lock() {
        Ok(g) => (g.1.events().to_vec(), g.1.title(), g.1.usage()),
        Err(_) => (vec![], String::new(), Usage::default()),
    };
    Ok(AgentOpened { session_id: run.id.clone(), title, model: cfg.model, provider_name: provider.name, busy, events, usage, context: run.context.lock().clone() })
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
    let folder = dir()?.parent().unwrap_or(dir()?).join("agent-attachments").join(&session_id);
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
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        emit(&run.id, json!({ "type": "turnStart", "text": text, "attachments": attachments }));
        let (message, images) = with_attachments(&text, &attachments).await;
        let mut g = run.turn.lock().await;
        let (agent, session) = &mut *g;
        agent.policy = policy;
        if let Some(m) = model.filter(|m| !m.trim().is_empty() && *m != agent.model.name()) {
            agent.model = Arc::new(Llm::new(devcat_harness::LlmConfig { model: m, ..run.cfg.clone() }));
        }
        agent.context_window = context_window.filter(|&w| w > 0);
        let id = run.id.clone();
        let mut on = |e: AgentEvent| emit(&id, serde_json::to_value(&e).unwrap_or(Value::Null));
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
        let usage = session.usage();
        let title = session.title();
        drop(g);
        run.busy.store(false, Ordering::SeqCst);
        emit(&run.id, json!({ "type": "turnEnd", "end": end, "error": error, "usage": usage, "title": title, "elapsedMs": started.elapsed().as_millis() as u64 }));
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
    Ok(())
}

/// Forget a chat (its tab closed); a running turn is stopped.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_close(session_id: String) -> AppResult<()> {
    if let Some(run) = RUNS.lock().remove(&session_id) {
        run.cancel.lock().cancel();
        run.approver.deny_all();
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
                Some("user") => {
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
