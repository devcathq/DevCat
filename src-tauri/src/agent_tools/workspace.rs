//! DevCat's own things beyond one connection: adding and testing connections, the query history
//! and saved queries, scheduled DevCat Agent jobs, and ticket boards.
//!
//! - Agents add connections but don't edit or delete them, and can't raise what agents may do
//!   with one: a connection an agent adds has AI access Read (the user raises it in DevCat), and
//!   it may be made read-only. Passwords go to the keychain like the app's.
//! - Jobs run in the app (`agent_jobs.rs`); an agent can't make one that runs without asking
//!   (the Bypass mode), and only in its own folder (or below).
//! - Ticket boards follow the folder rule of connections; DevCat, not the agent, talks to the
//!   tracker, and `tickets_fix` hands tickets to the board's own agent run.
//! - `devcat_open_file` shows the user a file in DevCat's window (`opened::show`), as if they'd
//!   opened it with DevCat; its content doesn't go back to the agent. `db_open_query` opens SQL
//!   (text, a .sql file or a saved query) in a query tab and may run it there: a read on a
//!   read-only session, a write only where the agent may change data (`may_change`).
use super::*;
use crate::agent_jobs::{self, Job, Schedule};
use crate::history::SavedQuery;
use crate::tickets;
use devcat_harness::Policy;

/// History entries `db_history` returns unless asked for more; and the most.
const DEFAULT_HISTORY: usize = 30;
const MAX_HISTORY: usize = 300;

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>, v: &View) {
    out.push(Arc::new(ConnTest(s.clone())));
    if v.connections.iter().any(|c| is_db(c.kind)) {
        out.push(Arc::new(History(s.clone())));
        out.push(Arc::new(SaveQuery(s.clone())));
    }
    if s.unattended {
        return;
    }
    // nobody watches an unattended run, so there's no one to show a file to
    out.push(Arc::new(OpenFile));
    if v.connections.iter().any(|c| is_db(c.kind)) {
        out.push(Arc::new(OpenQuery(s.clone())));
    }
    out.push(Arc::new(ConnAdd(s.clone())));
    out.push(Arc::new(Jobs(s.clone())));
    out.push(Arc::new(JobSave(s.clone())));
    out.push(Arc::new(JobRun(s.clone())));
    // only where the user let coding agents hand work to a DevCat Agent connection
    if !crate::ai::delegates().is_empty() {
        out.push(Arc::new(Delegate(s.clone())));
    }
    if !boards(s).is_empty() {
        out.push(Arc::new(Tickets(s.clone())));
        out.push(Arc::new(TicketsFix(s.clone())));
    }
}

/// Connection kinds agents may add (SSH sessions stay the user's: see AGENTS.md). Not JDBC
/// either: its Java path and driver jars are programs DevCat runs (`db/jdbc.rs`).
const KINDS: &[&str] = &["postgres", "mysql", "sqlite", "mssql", "mongodb", "redis", "kubernetes", "rabbitmq", "docker"];

fn conn_props() -> Value {
    json!({
        "name": { "type": "string" },
        "kind": { "type": "string", "enum": KINDS },
        "host": { "type": "string" },
        "port": { "type": "integer" },
        "user": { "type": "string" },
        "password": { "type": "string" },
        "database": { "type": "string", "description": "Default database (Redis: db number, RabbitMQ: vhost)" },
        "file": { "type": "string", "description": "SQLite file, or kubeconfig for Kubernetes" },
        "url": { "type": "string", "description": "A connection string instead (postgres://…, mongodb+srv://…)" },
        "ssl_mode": { "type": "string", "enum": ["disable", "prefer", "require", "verify-full"] },
        "context": { "type": "string", "description": "Kubernetes / Docker context" },
        "network": { "type": "string", "description": "The network profile (VPN / tunnel) it goes through: name or id" },
        "options": { "type": "object", "description": "Driver options, as strings (RabbitMQ managementPort, useTls…)" }
    })
}

/// A connection from the model's fields (unsaved; id empty).
fn config_from(scope: &Scope, v: &View, input: &Value, ctx: &ToolCtx) -> Result<ConnectionConfig, ToolOutput> {
    let kind = arg(input, "kind").unwrap_or_default();
    if !KINDS.contains(&kind.as_str()) {
        return Err(ToolOutput::err(format!("Give the `kind`: one of {}.", KINDS.join(", "))));
    }
    let mut options: serde_json::Map<String, Value> = serde_json::Map::new();
    if let Some(m) = input.get("options").and_then(Value::as_object) {
        for (k, val) in m {
            options.insert(k.clone(), json!(val.as_str().map(String::from).unwrap_or_else(|| val.to_string())));
        }
    }
    if let Some(cx) = arg(input, "context") {
        options.insert("context".into(), json!(cx));
    }
    if let Some(n) = arg(input, "network") {
        let p = super::network::find_for_connection(scope, &n)?;
        options.insert("networkId".into(), json!(p));
    }
    let project = match arg(input, "project") {
        Some(p) => project_named(v, &p)?,
        None => v.project.as_ref().map(|p| p.id.clone()),
    };
    let cfg = json!({
        "id": "",
        "name": arg(input, "name").unwrap_or_default(),
        "kind": kind,
        "host": arg(input, "host").unwrap_or_default(),
        "port": input.get("port").and_then(Value::as_u64),
        "user": arg(input, "user").unwrap_or_default(),
        "password": input.get("password").and_then(Value::as_str),
        "database": arg(input, "database"),
        "filePath": arg(input, "file").map(|f| local_path(ctx, &f)),
        "url": arg(input, "url"),
        "sslMode": arg(input, "ssl_mode"),
        "projectId": project,
        "options": options,
        "readOnly": arg_bool(input, "read_only").unwrap_or(false),
    });
    let cfg: ConnectionConfig = serde_json::from_value(cfg).map_err(|e| ToolOutput::err(format!("Bad connection settings: {e}")))?;
    // a kubeconfig can run a program to log in: not one the agent names (see the function)
    if cfg.kind == ConnectionKind::Kubernetes {
        if let Some(file) = cfg.file_path.as_deref().filter(|f| !f.trim().is_empty()) {
            let temp = std::env::temp_dir();
            if let Some(why) = crate::infra::k8s::agent_kubeconfig_problem(file, &[ctx.cwd.as_path(), temp.as_path()]) {
                return Err(ToolOutput::err(why));
            }
        }
    }
    Ok(cfg)
}

// ---------- devcat_connection_add

struct ConnAdd(Arc<Scope>);

#[async_trait]
impl Tool for ConnAdd {
    fn spec(&self) -> ToolSpec {
        let mut props = conn_props();
        let m = props.as_object_mut().unwrap();
        m.insert("project".into(), json!({ "type": "string", "description": "DevCat project (default: this folder's)" }));
        m.insert("read_only".into(), json!({ "type": "boolean", "description": "Nothing can change data through it" }));
        m.insert("test".into(), json!({ "type": "boolean", "description": "Connect first and save only if that works (default true)" }));
        ToolSpec {
            name: "devcat_connection_add".into(),
            description: "Add a connection to DevCat (a database, Redis, MongoDB, Kubernetes cluster, Docker engine or RabbitMQ broker), e.g. from a project's .env or docker-compose file when the user asks to set DevCat up. DevCat connects first and saves it only if that works (`test: false` saves anyway); the password goes to the OS keychain. It goes in this folder's DevCat project unless `project` says otherwise, behind a VPN / tunnel with `network`. Agents get read access to it (AI access Read); the user decides in DevCat whether they may change data. Then use it with the other tools by its name.".into(),
            input_schema: json!({ "type": "object", "properties": props, "required": ["name", "kind"] }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::EditOutside
    }
    fn summary(&self, input: &Value) -> String {
        format!("add {} ({})", arg(input, "name").unwrap_or_default(), arg(input, "kind").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(90)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let v = view(&self.0);
        let mut cfg = match config_from(&self.0, &v, &input, ctx) {
            Ok(c) => c,
            Err(e) => return e,
        };
        // agents read what they add; whether they may change data is the user's call (not the
        // project's "Allow writes from MCP", which is for connections saved before the setting)
        cfg.mcp_access = Some(McpAccess::Read);
        if cfg.name.trim().is_empty() {
            return ToolOutput::err("Give the connection a `name`.");
        }
        let st = St::get();
        if st.store.load_all().unwrap_or_default().iter().any(|c| c.name.eq_ignore_ascii_case(cfg.name.trim()) && c.project_id == cfg.project_id) {
            return ToolOutput::err(format!("There's already a connection called \"{}\" there; pick another name.", cfg.name));
        }
        let mut tested = None;
        if arg_bool(&input, "test") != Some(false) {
            match crate::commands::connections::test_connection(cfg.clone()).await {
                Ok(version) => tested = Some(version),
                Err(e) => return ToolOutput::err(format!("Couldn't connect, so it isn't saved: {e}. Fix the settings, or pass `test: false` to save it anyway.")),
            }
        }
        match crate::commands::connections::save_connection(st, cfg).await {
            Ok(saved) => {
                // every window reloads its connections
                crate::events::emit("devcat://window-sync", json!({ "kind": "connections", "from": "agent" }));
                let mut text = format!("Added {}.", describe(&saved));
                if let Some(v) = tested.filter(|v| !v.is_empty()) {
                    text.push_str(&format!(" Connected: {v}."));
                }
                ToolOutput::ok(text)
            }
            Err(e) => ToolOutput::err(format!("Couldn't save the connection: {e}")),
        }
    }
}

// ---------- devcat_connection_test

struct ConnTest(Arc<Scope>);

#[async_trait]
impl Tool for ConnTest {
    fn spec(&self) -> ToolSpec {
        let mut props = conn_props();
        props.as_object_mut().unwrap().insert("connection".into(), json!({ "type": "string", "description": "An existing connection (name or id); else describe one with the other fields" }));
        ToolSpec {
            name: "devcat_connection_test".into(),
            description: "Check that DevCat can reach a server: an existing DevCat connection (`connection`), or settings you'd add (the fields of `devcat_connection_add`). Connects through its VPN / tunnel and reports the server's version, or what went wrong.".into(),
            input_schema: json!({ "type": "object", "properties": props }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        arg(input, "connection").or_else(|| arg(input, "host")).or_else(|| arg(input, "name")).unwrap_or_default()
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(90)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let cfg = if arg(&input, "connection").is_some() {
            let c = match find(&self.0, &input).and_then(|c| super::network::usable(&self.0, &c).map(|_| c)) {
                Ok(c) => c,
                Err(e) => return e,
            };
            // with its password, from the keychain
            match St::get().store.get(&c.id) {
                Ok(full) => full,
                Err(e) => return ToolOutput::err(e.to_string()),
            }
        } else if self.0.unattended {
            // a run on outside text (a ticket) doesn't reach servers it describes itself
            return ToolOutput::err("This run can only test DevCat's existing connections: give `connection`.");
        } else {
            match config_from(&self.0, &view(&self.0), &input, ctx) {
                Ok(c) => c,
                Err(e) => return e,
            }
        };
        let name = if cfg.name.is_empty() { cfg.host.clone() } else { cfg.name.clone() };
        match crate::commands::connections::test_connection(cfg).await {
            Ok(v) if v.is_empty() => ToolOutput::ok(format!("{name}: connected.")),
            Ok(v) => ToolOutput::ok(format!("{name}: connected ({v}).")),
            Err(e) => ToolOutput::err(format!("{name}: couldn't connect: {e}")),
        }
    }
}

// ---------- db_history

struct History(Arc<Scope>);

#[async_trait]
impl Tool for History {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_history".into(),
            description: "The queries the user ran in DevCat (newest first, with connection, time, duration and whether it failed) and their saved queries (`saved: true`). `connection` limits it to one, `search` to queries containing the text. Useful to see how the user usually queries a database, or to find a query they mention.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "search": { "type": "string" },
                    "saved": { "type": "boolean", "description": "The saved queries instead of the history" },
                    "limit": { "type": "integer", "description": "Default 30" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        let what = if arg_bool(input, "saved") == Some(true) { "saved queries" } else { "history" };
        format!("{what}{}", arg(input, "connection").map(|c| format!(" · {c}")).unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let v = view(&self.0);
        let only = match arg(&input, "connection") {
            Some(_) => match find(&self.0, &input) {
                Ok(c) => Some(c.id),
                Err(e) => return e,
            },
            None => None,
        };
        // only what's about connections this agent sees
        let name_of = |id: &str| v.connections.iter().find(|c| c.id == id).map(|c| c.name.clone());
        let limit = input.get("limit").and_then(Value::as_u64).map(|n| (n as usize).clamp(1, MAX_HISTORY)).unwrap_or(DEFAULT_HISTORY);
        let search = arg(&input, "search");
        if arg_bool(&input, "saved") == Some(true) {
            let all = match crate::commands::history::saved_queries_list(St::get()).await {
                Ok(a) => a,
                Err(e) => return ToolOutput::err(e.to_string()),
            };
            let lines: Vec<String> = all
                .into_iter()
                .filter(|q| q.connection_id.as_deref().is_none_or(|id| name_of(id).is_some()))
                .filter(|q| only.as_ref().is_none_or(|o| q.connection_id.as_ref() == Some(o)))
                .filter(|q| search.as_ref().is_none_or(|s| q.query.to_lowercase().contains(&s.to_lowercase()) || q.name.to_lowercase().contains(&s.to_lowercase())))
                .take(limit)
                .map(|q| format!("- {} ({}{}) id {}\n{}", q.name, q.connection_id.as_deref().and_then(name_of).unwrap_or_else(|| "any connection".into()), q.database.map(|d| format!(", {d}")).unwrap_or_default(), q.id, q.query))
                .collect();
            return ToolOutput::ok(if lines.is_empty() { "No saved queries.".into() } else { lines.join("\n") });
        }
        match crate::commands::history::history_list(St::get(), only, search, Some(MAX_HISTORY * 2)).await {
            Ok(all) => {
                let lines: Vec<String> = all
                    .into_iter()
                    .filter_map(|h| {
                        let conn = name_of(&h.connection_id)?;
                        let fail = h.error.as_deref().map(|e| format!(" FAILED: {e}")).unwrap_or_default();
                        Some(format!("- {} · {conn}{} · {} ms{fail}\n{}", h.executed_at, h.database.map(|d| format!(" ({d})")).unwrap_or_default(), h.elapsed_ms, h.query.trim()))
                    })
                    .take(limit)
                    .collect();
                ToolOutput::ok(if lines.is_empty() { "No queries in the history.".into() } else { lines.join("\n") })
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

// ---------- db_saved_query_save

struct SaveQuery(Arc<Scope>);

#[async_trait]
impl Tool for SaveQuery {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_saved_query_save".into(),
            description: "Save a query in DevCat's saved queries (the user opens it from the Saved queries panel), for a connection or any. `id` (from `db_history` with saved: true) replaces that one. Save what the user asks for, not scratch work.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "query": { "type": "string" },
                    "connection": { "type": "string", "description": "Connection name or id (any when left out)" },
                    "database": { "type": "string" },
                    "id": { "type": "string" }
                },
                "required": ["name", "query"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::EditOutside
    }
    fn summary(&self, input: &Value) -> String {
        format!("save query {}", arg(input, "name").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let (Some(name), Some(query)) = (arg(&input, "name"), arg(&input, "query")) else { return ToolOutput::err("Missing `name` or `query`.") };
        let connection_id = match arg(&input, "connection") {
            Some(_) => match find(&self.0, &input) {
                Ok(c) => Some(c.id),
                Err(e) => return e,
            },
            None => None,
        };
        let st = St::get();
        let id = arg(&input, "id").unwrap_or_default();
        if !id.is_empty() {
            let v = view(&self.0);
            let mine = crate::commands::history::saved_queries_list(st).await.unwrap_or_default().into_iter().find(|q| q.id == id);
            match mine {
                Some(q) if q.connection_id.as_deref().is_none_or(|c| v.connections.iter().any(|x| x.id == c)) => {}
                _ => return ToolOutput::err(format!("There's no saved query with id {id} here; leave `id` out to add one.")),
            }
        }
        let q = SavedQuery { id, name, connection_id, database: arg(&input, "database"), query, updated_at: String::new() };
        match crate::commands::history::saved_query_save(st, q).await {
            Ok(saved) => ToolOutput::ok(format!("Saved the query {} (id {}).", saved.name, saved.id)),
            Err(e) => ToolOutput::err(format!("Couldn't save the query: {e}")),
        }
    }
}

// ---------- agent_delegate

/// Longest answer `agent_delegate` hands back: the point is to spend the caller's tokens on the
/// outcome, not the work.
const MAX_DELEGATE_ANSWER: usize = 4000;
/// Part of the model's window a task may take up on its own: the rest is for the DevCat Agent's
/// instructions, the files it reads and its work.
const TASK_SHARE: f64 = 0.25;

/// Hands a task to a DevCat Agent connection the user allowed (`AgentSettings::delegate`: a
/// cheaper or local model), in the caller's folder, and waits for it: the caller gets a short
/// answer and the files it changed. The chat stays in DevCat for the user to read; it gets no
/// DevCat tools (`connections: false`), so it can't hand work on in turn. Nobody watches it, so
/// it never waits for the user: what would ask (commands, web pages, edits outside the folder) is
/// refused at once (`agent_jobs::is_delegated`), and questions get "decide yourself".
struct Delegate(Arc<Scope>);

/// The allowed connection the model means, or the one there is.
fn delegate_named(want: Option<&str>) -> Result<crate::ai::AiProvider, ToolOutput> {
    let all = crate::ai::delegates();
    let names = || all.iter().map(|p| format!("\"{}\"", p.name)).collect::<Vec<_>>().join(", ");
    if all.is_empty() {
        return Err(ToolOutput::err("No DevCat Agent connection takes delegated work: the user turns on \"Coding agents may hand it work\" for one in DevCat (AI connections)."));
    }
    match want {
        Some(w) => all.iter().find(|p| p.id == w).or_else(|| all.iter().find(|p| p.name.eq_ignore_ascii_case(w))).cloned().ok_or_else(|| ToolOutput::err(format!("\"{w}\" doesn't take delegated work. These do: {}.", names()))),
        None if all.len() == 1 => Ok(all[0].clone()),
        None => Err(ToolOutput::err(format!("Say which `agent`: {}.", names()))),
    }
}

/// Whether the task alone fits in its share of the window (about 4 characters a token).
fn task_fits(task: &str, window: u64) -> Result<(), String> {
    let tokens = task.chars().count() as u64 / 4;
    let room = (window as f64 * TASK_SHARE) as u64;
    if tokens > room {
        return Err(format!("The task is about {tokens} tokens, and this model's window is {window}: keep it under {room}. Split it, or name the files for it to read instead of pasting them."));
    }
    Ok(())
}

/// At most `max` characters of `text`, saying when there was more.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}\n[… cut: the full answer is in the chat in DevCat]", &text[..i]),
        None => text.to_string(),
    }
}

#[async_trait]
impl Tool for Delegate {
    fn spec(&self) -> ToolSpec {
        let names: Vec<String> = crate::ai::delegates().into_iter().map(|p| p.name).collect();
        ToolSpec {
            name: "agent_delegate".into(),
            description: format!(
                "Hand a self-contained task to another model through DevCat (a cheaper or local one the user allowed: {}), to save your own tokens: it works in your folder with file tools (read, glob, grep, edit; no commands, web pages or edits outside the folder, since nobody approves them) and you get back a short summary and the files it changed, never its whole work. Good for mechanical edits, drafting, renames, boilerplate, summarizing files. Its context is smaller than yours: give one focused task, name the files and what done means, don't paste big files (it reads them). Check what it changed before you rely on it. Takes minutes; needs DevCat open.",
                if names.is_empty() { "none yet".into() } else { names.join(", ") }
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "What to do, with the files and what done means" },
                    "agent": { "type": "string", "description": "Which allowed DevCat Agent connection (name or id); needed when there are several" },
                    "folder": { "type": "string", "description": "Where it works: your folder (default) or a folder in it" },
                    "mode": { "type": "string", "enum": ["auto-edit", "plan"], "description": "auto-edit (default): it edits files in the folder; plan: it only reads and answers" }
                },
                "required": ["task"]
            }),
        }
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        if arg(input, "mode").as_deref() == Some("plan") { Kind::Query } else { Kind::EditOutside }
    }
    fn summary(&self, input: &Value) -> String {
        let task = arg(input, "task").unwrap_or_default();
        format!("{}: {}", arg(input, "agent").unwrap_or_else(|| "delegate".into()), task.chars().take(80).collect::<String>())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(60 * 60)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        // chats run in the app: not in `devcat mcp` with DevCat closed, nor `devcat serve` without the scheduler
        if let Err(e) = jobs_here() {
            return ToolOutput::err(e.content.replace("Scheduled jobs live", "Delegated work runs"));
        }
        let Some(task) = arg(&input, "task") else { return ToolOutput::err("Missing `task`: what it should do.") };
        let p = match delegate_named(arg(&input, "agent").as_deref()) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let root = job_folder(&self.0, ctx);
        let folder = match arg(&input, "folder") {
            Some(f) => local_path(ctx, &f),
            None => root.clone(),
        };
        if !within(&folder, &root) {
            return ToolOutput::err(format!("It works in your folder ({root}) or a folder in it, not {folder}."));
        }
        if !Path::new(&folder).is_dir() {
            return ToolOutput::err(format!("There's no folder {folder}."));
        }
        let policy = match arg(&input, "mode").as_deref().unwrap_or("auto-edit") {
            "auto-edit" => Policy::AutoEdit,
            "plan" => Policy::Plan,
            m => return ToolOutput::err(format!("Unknown mode {m}: auto-edit or plan.")),
        };
        let window = crate::ai::provider_context_window(&p.id).await.unwrap_or(128_000);
        if let Err(why) = task_fits(&task, window) {
            return ToolOutput::err(why);
        }
        let opened = match crate::agent::open(folder.clone(), p.id.clone(), None, "delegate", crate::agent::SCHEDULE, None, None, false).await {
            Ok(o) => o,
            Err(e) => return ToolOutput::err(format!("Couldn't start {}: {e}", p.name)),
        };
        let sid = opened.session_id;
        let title = format!("Delegated · {}", task.split_whitespace().take(8).collect::<Vec<_>>().join(" "));
        let _ = crate::agent::agent_rename(sid.clone(), title.clone()).await;
        agent_jobs::delegated(&sid, Some(title));
        let prompt = format!(
            "{task}\n\n(Another coding agent handed you this task and reads only your final message. Work in this folder. When done, end with a short summary, at most 150 words: what you changed (file paths), what you checked, and anything left undone or uncertain. Don't paste file contents or diffs.)"
        );
        let started = std::time::Instant::now();
        let result = crate::agent::run_turn(&sid, &prompt, policy, Some(window), false).await;
        let took = started.elapsed().as_secs();
        agent_jobs::delegated(&sid, None);
        let answer = crate::agent::last_answer(&sid).unwrap_or_default();
        let changed: Vec<String> = crate::agent::changed_files(&sid)
            .into_iter()
            .map(|f| Path::new(&f).strip_prefix(&folder).map(|r| r.to_string_lossy().into_owned()).unwrap_or(f))
            .collect();
        crate::agent::close_if_idle(&sid);
        let (end, error) = match result {
            Ok(r) => r,
            Err(e) => ("error".into(), Some(e.to_string())),
        };
        let head = match (end.as_str(), &error) {
            ("answered", _) => format!("{} finished in {took} s.", p.name),
            ("stepLimit", _) => format!("{} stopped at its step limit after {took} s without finishing.", p.name),
            ("stopped", _) => format!("{} was stopped.", p.name),
            (_, Some(e)) => format!("{} failed: {e}", p.name),
            _ => format!("{} didn't finish ({end}).", p.name),
        };
        let files = if changed.is_empty() { "Files changed: none.".to_string() } else { format!("Files changed: {}.", changed.join(", ")) };
        let body = if answer.is_empty() { "(No answer.)".to_string() } else { clip(&answer, MAX_DELEGATE_ANSWER) };
        let out = format!("{head} {files}\n\n{body}");
        if end == "answered" { ToolOutput::ok(out) } else { ToolOutput::err(out) }
    }
}

// ---------- scheduled jobs

/// Jobs are managed in the app; elsewhere (`devcat serve`, `devcat mcp` with DevCat closed) the
/// tools say so.
fn jobs_here() -> Result<(), ToolOutput> {
    if !agent_jobs::ready() {
        return Err(ToolOutput::err("Scheduled jobs live in the DevCat app, and it isn't open: the user opens DevCat and asks again."));
    }
    Ok(())
}

/// The folder jobs are for: the agent's (over MCP, the folder it works in).
fn job_folder(scope: &Scope, ctx: &ToolCtx) -> String {
    scope.folder.clone().unwrap_or_else(|| ctx.cwd.clone()).to_string_lossy().into_owned()
}

/// Whether `dir` is `root` or inside it.
fn within(dir: &str, root: &str) -> bool {
    let (d, r) = (Path::new(dir), Path::new(root));
    !root.is_empty() && d.starts_with(r) && !d.components().any(|c| c == std::path::Component::ParentDir)
}

async fn my_jobs(scope: &Scope, ctx: &ToolCtx) -> Result<Vec<agent_jobs::JobInfo>, ToolOutput> {
    jobs_here()?;
    let root = job_folder(scope, ctx);
    let all = agent_jobs::agent_jobs(None).await.map_err(|e| ToolOutput::err(e.to_string()))?;
    Ok(all.into_iter().filter(|j| within(&j.job.cwd, &root)).collect())
}

fn find_job(jobs: Vec<agent_jobs::JobInfo>, want: &str) -> Result<Job, ToolOutput> {
    let names = jobs.iter().map(|j| j.job.name.clone()).collect::<Vec<_>>().join(", ");
    jobs.into_iter()
        .map(|j| j.job)
        .find(|j| j.id == want || j.name.eq_ignore_ascii_case(want))
        .ok_or_else(|| ToolOutput::err(format!("No scheduled job called \"{want}\" in this folder. Jobs: {}.", if names.is_empty() { "none".into() } else { names })))
}

fn when_ms(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

fn schedule_text(s: &Schedule) -> String {
    match s {
        Schedule::Every { minutes } => format!("every {minutes} min"),
        Schedule::Daily { time, days } if days.is_empty() => format!("daily at {time}"),
        Schedule::Daily { time, days } => format!("at {time} on days {}", days.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(",")),
    }
}

struct Jobs(Arc<Scope>);

#[async_trait]
impl Tool for Jobs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agent_jobs".into(),
            description: "The scheduled DevCat Agent jobs of this folder: a prompt that DevCat runs on a schedule (every N minutes, or daily at a time) as a new DevCat Agent chat. Shows each job's schedule, mode, whether it's on or running, when it runs next and how its last runs ended.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        String::new()
    }
    async fn run(&self, _: Value, ctx: &ToolCtx) -> ToolOutput {
        let jobs = match my_jobs(&self.0, ctx).await {
            Ok(j) => j,
            Err(e) => return e,
        };
        if jobs.is_empty() {
            return ToolOutput::ok("No scheduled jobs in this folder (`agent_job_save` adds one).");
        }
        let lines: Vec<String> = jobs
            .iter()
            .map(|j| {
                let job = &j.job;
                let state = if j.running { "running now".to_string() } else if job.enabled { job.next_ms.map(|n| format!("next {}", when_ms(n))).unwrap_or_else(|| "on".into()) } else { "off".into() };
                let last: Vec<String> = job.runs.iter().rev().take(3).map(|r| format!("{} {}", when_ms(r.started_ms), r.end.clone().or(r.error.clone()).unwrap_or_else(|| "running".into()))).collect();
                format!("- {} ({}, {:?} mode) id {} · {state}\n  prompt: {}\n  last runs: {}", job.name, schedule_text(&job.schedule), job.policy, job.id, job.prompt.lines().next().unwrap_or(""), if last.is_empty() { "none".into() } else { last.join("; ") })
            })
            .collect();
        ToolOutput::ok(lines.join("\n"))
    }
}

struct JobSave(Arc<Scope>);

#[async_trait]
impl Tool for JobSave {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agent_job_save".into(),
            description: "Schedule a DevCat Agent job in this folder, or change one (`job`: its name or id): `prompt` is what the agent does each run, `every_minutes` (5 or more) or `daily_at` (\"HH:MM\", with `days` 1 = Monday … 7 = Sunday) when. `mode` ask (default: it asks the user before commands and edits), auto-edit or plan; a job can't run without asking. `provider` is the DevCat Agent connection (default: the first one). `enabled: false` keeps it off; `delete: true` deletes it.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "An existing job to change (name or id)" },
                    "name": { "type": "string" },
                    "prompt": { "type": "string" },
                    "every_minutes": { "type": "integer" },
                    "daily_at": { "type": "string" },
                    "days": { "type": "array", "items": { "type": "integer" } },
                    "mode": { "type": "string", "enum": ["ask", "auto-edit", "plan"] },
                    "provider": { "type": "string" },
                    "enabled": { "type": "boolean" },
                    "delete": { "type": "boolean" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        match (arg(input, "job"), arg_bool(input, "delete")) {
            (Some(j), Some(true)) => format!("delete job {j}"),
            (Some(j), _) => format!("change job {j}"),
            (None, _) => format!("schedule {}", arg(input, "name").unwrap_or_default()),
        }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        match save_job(&self.0, &input, ctx).await {
            Ok(t) => ToolOutput::ok(t),
            Err(e) => e,
        }
    }
}

async fn save_job(scope: &Scope, input: &Value, ctx: &ToolCtx) -> Result<String, ToolOutput> {
    let fail = |m: String| ToolOutput::err(m);
    let jobs = my_jobs(scope, ctx).await?;
    let mut job = match arg(input, "job") {
        Some(want) => {
            let j = find_job(jobs, &want)?;
            if arg_bool(input, "delete") == Some(true) {
                agent_jobs::agent_job_delete(j.id.clone()).await.map_err(|e| fail(e.to_string()))?;
                return Ok(format!("Deleted the job {}.", j.name));
            }
            j
        }
        None => {
            let provider = match arg(input, "provider") {
                Some(p) => p,
                None => {
                    let all = crate::ai::providers().await.unwrap_or_default();
                    all.into_iter().find(|p| crate::ai::agent_config(&p.id).is_ok()).map(|p| p.id).ok_or_else(|| fail("There's no DevCat Agent connection to run the job with: the user adds one in DevCat (AI connections).".into()))?
                }
            };
            Job {
                id: String::new(),
                name: String::new(),
                cwd: job_folder(scope, ctx),
                provider_id: provider,
                prompt: String::new(),
                policy: Policy::Ask,
                sandbox: false,
                schedule: Schedule::Every { minutes: 60 },
                enabled: true,
                runs: vec![],
                next_ms: None,
            }
        }
    };
    if let Some(n) = arg(input, "name") {
        job.name = n;
    }
    if let Some(p) = arg(input, "prompt") {
        job.prompt = p;
    }
    if let Some(p) = arg(input, "provider") {
        // by name or id
        let all = crate::ai::providers().await.unwrap_or_default();
        job.provider_id = all.into_iter().find(|x| x.id == p || x.name.eq_ignore_ascii_case(&p)).map(|x| x.id).unwrap_or(p);
    }
    if let Some(m) = input.get("every_minutes").and_then(Value::as_u64) {
        job.schedule = Schedule::Every { minutes: m as u32 };
    }
    if let Some(t) = arg(input, "daily_at") {
        let days = input.get("days").and_then(Value::as_array).map(|d| d.iter().filter_map(Value::as_u64).map(|d| d as u8).collect()).unwrap_or_default();
        job.schedule = Schedule::Daily { time: t, days };
    }
    if let Some(m) = arg(input, "mode") {
        job.policy = match m.as_str() {
            "ask" => Policy::Ask,
            "auto-edit" => Policy::AutoEdit,
            "plan" => Policy::Plan,
            _ => return Err(fail(format!("Unknown `mode` \"{m}\": ask, auto-edit or plan (a job that runs without asking is the user's to make, in DevCat)."))),
        };
    }
    if job.policy == Policy::Bypass {
        return Err(fail("This job runs without asking; only the user changes it, in DevCat.".into()));
    }
    if let Some(e) = arg_bool(input, "enabled") {
        job.enabled = e;
    }
    let existed = !job.id.is_empty();
    let saved = agent_jobs::agent_job_save(job).await.map_err(|e| fail(format!("Couldn't save the job: {e}")))?;
    let next = saved.next_ms.map(|n| format!(", next run {}", when_ms(n))).unwrap_or_else(|| ", off".into());
    Ok(format!("{} the job {} ({}{next}) id {}.", if existed { "Changed" } else { "Scheduled" }, saved.name, schedule_text(&saved.schedule), saved.id))
}

struct JobRun(Arc<Scope>);

#[async_trait]
impl Tool for JobRun {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agent_job_run".into(),
            description: "Run one of this folder's scheduled DevCat Agent jobs now (its schedule stays). It runs as a new DevCat Agent chat in DevCat; `agent_jobs` shows how it ended.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "job": { "type": "string", "description": "Job name or id" } },
                "required": ["job"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("run {}", arg(input, "job").unwrap_or_default())
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let Some(want) = arg(&input, "job") else { return ToolOutput::err("Missing `job`.") };
        let job = match my_jobs(&self.0, ctx).await.and_then(|j| find_job(j, &want)) {
            Ok(j) => j,
            Err(e) => return e,
        };
        match agent_jobs::agent_job_run(job.id.clone()).await {
            Ok(()) => ToolOutput::ok(format!("Started {}; it runs as a DevCat Agent chat in DevCat.", job.name)),
            Err(e) => ToolOutput::err(format!("Couldn't start {}: {e}", job.name)),
        }
    }
}

// ---------- tickets

/// The ticket boards an agent working in `scope` sees (none where boards aren't kept).
fn boards(scope: &Scope) -> Vec<tickets::Board> {
    if !tickets::ready() {
        return vec![];
    }
    let v = view(scope);
    let all = tickets::load_boards().unwrap_or_default();
    all.into_iter().filter(|b| in_reach(b.project_id.as_deref(), &v.projects, v.project.as_ref())).collect()
}

fn find_board(scope: &Scope, input: &Value) -> Result<tickets::Board, ToolOutput> {
    let all = boards(scope);
    let Some(want) = arg(input, "board") else {
        return match all.as_slice() {
            [b] => Ok(b.clone()),
            _ => Err(ToolOutput::err(format!("Which `board`? {}.", all.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")))),
        };
    };
    all.iter().find(|b| b.id == want || b.name.eq_ignore_ascii_case(&want)).cloned().ok_or_else(|| ToolOutput::err(format!("No ticket board called \"{want}\". Boards: {}.", all.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", "))))
}

struct Tickets(Arc<Scope>);

#[async_trait]
impl Tool for Tickets {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tickets_list".into(),
            description: "The user's DevCat ticket boards (Jira, Trello, Linear, GitHub) and their tickets. Without `board`: each board with its tracker, repository and columns. With `board`: the tickets in `column` (default the board's \"pick from\" column) with key, title, description, labels, assignees and comments; `ticket` (key or id) for one in full. `history: true`: what DevCat's agent did on the board's tickets lately.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string", "description": "Board name or id" },
                    "column": { "type": "string", "description": "Column name or id" },
                    "ticket": { "type": "string" },
                    "history": { "type": "boolean" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        [arg(input, "board"), arg(input, "column"), arg(input, "ticket")].into_iter().flatten().collect::<Vec<_>>().join(" · ")
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(90)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        if arg(&input, "board").is_none() && arg(&input, "column").is_none() && arg(&input, "ticket").is_none() && arg_bool(&input, "history") != Some(true) {
            let lines: Vec<String> = boards(&self.0)
                .iter()
                .map(|b| {
                    let col = |c: &Option<tickets::Column>| c.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "-".into());
                    format!("- {} ({:?} {}) id {} · repo {} · picks from {} → working {} → done {}", b.name, b.kind, b.project, b.id, if b.repo.is_empty() { "-" } else { &b.repo }, col(&b.pick_from), col(&b.working), col(&b.done_to))
                })
                .collect();
            return ToolOutput::ok(lines.join("\n"));
        }
        let b = match find_board(&self.0, &input) {
            Ok(b) => b,
            Err(e) => return e,
        };
        if arg_bool(&input, "history") == Some(true) {
            return match tickets::tickets_history(b.id.clone()).await {
                Ok(h) if h.is_empty() => ToolOutput::ok("DevCat's agent hasn't worked on this board's tickets yet."),
                Ok(h) => ToolOutput::ok(h.iter().rev().take(30).map(|r| serde_json::to_string(r).unwrap_or_default()).collect::<Vec<_>>().join("\n")),
                Err(e) => ToolOutput::err(e.to_string()),
            };
        }
        let column = match arg(&input, "column") {
            Some(want) => match tickets::tickets_columns(b.clone()).await {
                Ok(cols) => match cols.into_iter().find(|c| c.id == want || c.name.eq_ignore_ascii_case(&want)) {
                    Some(c) => c.id,
                    None => return ToolOutput::err(format!("{} has no column \"{want}\".", b.name)),
                },
                Err(e) => return ToolOutput::err(e.to_string()),
            },
            None => match &b.pick_from {
                Some(c) => c.id.clone(),
                None => return ToolOutput::err("Give the `column`."),
            },
        };
        let list = match tickets::tickets_list(b.id.clone(), column).await {
            Ok(l) => l,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        if let Some(t) = arg(&input, "ticket") {
            return match list.iter().find(|x| x.key.eq_ignore_ascii_case(&t) || x.id == t) {
                Some(x) => ToolOutput::ok(serde_json::to_string_pretty(x).unwrap_or_default()),
                None => ToolOutput::err(format!("No ticket {t} in that column.")),
            };
        }
        if list.is_empty() {
            return ToolOutput::ok("No tickets in that column.");
        }
        ToolOutput::ok(list.iter().map(|t| format!("- {} {} [{}] {}\n  {}", t.key, t.title, t.labels.join(", "), t.url, t.description.lines().next().unwrap_or(""))).collect::<Vec<_>>().join("\n"))
    }
}

struct TicketsFix(Arc<Scope>);

#[async_trait]
impl Tool for TicketsFix {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tickets_fix".into(),
            description: "Hand tickets of a DevCat ticket board to the board's own agent run, as the user's \"Fix\" button does: for each ticket DevCat makes a branch in the board's repository, runs the board's agent on it, commits, comments on the ticket and moves it on. `tickets` are keys or ids from `tickets_list`; `prompt` replaces the board's default instructions for them. It runs in DevCat, one ticket at a time. Only when the user asks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "board": { "type": "string" },
                    "tickets": { "type": "array", "items": { "type": "string" } },
                    "prompt": { "type": "string" }
                },
                "required": ["tickets"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        let t: Vec<&str> = input.get("tickets").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        format!("fix {}", t.join(", "))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(90)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        // `devcat mcp` with DevCat closed: the board's run would live in this short-lived process
        if crate::network::passive() {
            return ToolOutput::err("DevCat isn't open, and tickets are worked in DevCat: the user opens DevCat (or runs `devcat serve`) and asks again.");
        }
        let b = match find_board(&self.0, &input) {
            Ok(b) => b,
            Err(e) => return e,
        };
        let wanted: Vec<String> = input.get("tickets").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if wanted.is_empty() {
            return ToolOutput::err("Missing `tickets`.");
        }
        // keys → ids, from the board's pick-from column
        let listed = match &b.pick_from {
            Some(c) => tickets::tickets_list(b.id.clone(), c.id.clone()).await.unwrap_or_default(),
            None => vec![],
        };
        let ids: Vec<String> = wanted.iter().map(|w| listed.iter().find(|t| t.key.eq_ignore_ascii_case(w) || &t.id == w).map(|t| t.id.clone()).unwrap_or_else(|| w.clone())).collect();
        match tickets::tickets_fix(b.id.clone(), ids, arg(&input, "prompt")).await {
            Ok(()) => ToolOutput::ok(format!("Queued {} on {}; DevCat works on them one at a time (`tickets_list` with history: true shows the outcome).", wanted.join(", "), b.name)),
            Err(e) => ToolOutput::err(format!("Couldn't start: {e}")),
        }
    }
}

// ---------- devcat_open_file

/// Shows the user a file in DevCat's window: PDFs, images, sheets in the viewer, text in the
/// editor. It reads like `read` (never asks): the window changes, nothing else, and the agent gets
/// only where it opened.
struct OpenFile;

#[async_trait]
impl Tool for OpenFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "devcat_open_file".into(),
            description: "Open a file on this computer in the DevCat window for the user to look at: PDFs, images, spreadsheets, CSV, Word and Markdown in DevCat's viewer, code and other text in its editor. Use it when the user asks to open, show or preview a file in DevCat. You don't get the file's content back (read it with your own file tools). Needs the DevCat app open.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file: absolute, or relative to your folder (`~` is the home folder)" }
                },
                "required": ["path"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input.get("path").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let Some(p) = arg(&input, "path") else {
            return ToolOutput::err("Missing `path`: the file to open.");
        };
        let path = file_to_open(ctx, &p);
        // `devcat mcp` with DevCat closed: no window in this process
        if crate::network::passive() {
            return ToolOutput::err("DevCat isn't open, and files open in its window: the user opens DevCat and asks again.");
        }
        #[cfg(feature = "gui")]
        if let Some(app) = crate::events::app() {
            return match crate::opened::show(app, &path) {
                Ok(shown) => ToolOutput::ok(format!("Opened {shown} in DevCat.")),
                Err(e) => ToolOutput::err(e),
            };
        }
        let _ = path;
        ToolOutput::err("DevCat runs without windows here (`devcat serve`), so there's nowhere to show the file.")
    }
}

// ---------- db_open_query

/// How a query an agent opens runs in the window, once, when its tab opens.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoRun {
    /// On the connection's own session (the connection is read-only, or the agent may change data).
    Full,
    /// On a read-only session (`St::read_only_driver`): DevCat judged it a read by its words.
    ReadOnly,
}

/// A query an agent opens in a query tab (`db_open_query`); the window takes it from `opened::take_opened_queries`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedQuery {
    pub conn_id: String,
    pub database: Option<String>,
    pub sql: String,
    pub title: Option<String>,
    /// The saved query it is (its tab saves back to it).
    pub saved_query_id: Option<String>,
    pub run: Option<AutoRun>,
}


/// Biggest .sql file `db_open_query` opens (the query editor holds all of it).
const MAX_SQL_FILE: u64 = 2 * 1024 * 1024;

/// Opens SQL in a query tab of DevCat's window, on a connection, and runs it there if asked: the
/// user sees the editor and the results grid, the agent only where it opened. What runs follows
/// the agent's rights, not the window's: a read runs on a read-only session
/// (`St::read_only_driver`), a write only where `may_change` allows (the DevCat Agent asks first:
/// `Kind::Exec`); otherwise the tab opens and the user presses Run.
struct OpenQuery(Arc<Scope>);

/// The SQL the model means: `query`, else the `file`'s text, else the `saved` query's; with the
/// tab's title and the saved query it came from.
async fn sql_to_open(scope: &Scope, input: &Value, ctx: &ToolCtx) -> Result<(String, Option<String>, Option<SavedQuery>), ToolOutput> {
    if let Some(q) = input.get("query").and_then(Value::as_str).filter(|q| !q.trim().is_empty()) {
        return Ok((q.to_string(), None, None));
    }
    if let Some(f) = arg(input, "file") {
        let path = file_to_open(ctx, &f);
        let meta = std::fs::metadata(&path).map_err(|_| ToolOutput::err(format!("There's no file at {}.", path.display())))?;
        if !meta.is_file() {
            return Err(ToolOutput::err(format!("{} is a folder: give a .sql file in it.", path.display())));
        }
        if meta.len() > MAX_SQL_FILE {
            return Err(ToolOutput::err(format!("{} is over {} MB; the query editor opens smaller scripts.", path.display(), MAX_SQL_FILE / 1024 / 1024)));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ToolOutput::err(format!("Couldn't read {} as text: {e}", path.display())))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        return Ok((text, name, None));
    }
    if let Some(want) = arg(input, "saved") {
        let v = view(scope);
        let all = crate::commands::history::saved_queries_list(St::get()).await.unwrap_or_default();
        // saved queries of connections the agent doesn't see stay hidden (as in `db_history`)
        let mine: Vec<SavedQuery> = all.into_iter().filter(|q| q.connection_id.as_deref().is_none_or(|c| v.connections.iter().any(|x| x.id == c))).collect();
        let hit = mine.iter().find(|q| q.id == want).or_else(|| mine.iter().find(|q| q.name.eq_ignore_ascii_case(&want))).cloned();
        return match hit {
            Some(q) => Ok((q.query.clone(), Some(q.name.clone()), Some(q))),
            None => Err(ToolOutput::err(format!("There's no saved query \"{want}\" here (`db_history` with saved: true lists them)."))),
        };
    }
    Err(ToolOutput::err("Give the SQL: `query` (the text), `file` (a .sql file) or `saved` (a saved query's name or id)."))
}

/// The connection: `connection`, else the saved query's own.
fn open_query_conn(scope: &Scope, input: &Value, saved: Option<&SavedQuery>) -> Result<ConnectionConfig, ToolOutput> {
    let mut input = input.clone();
    if arg(&input, "connection").is_none()
        && let Some(c) = saved.and_then(|q| q.connection_id.clone())
    {
        input["connection"] = json!(c);
    }
    let c = find(scope, &input)?;
    if !is_db(c.kind) {
        return Err(ToolOutput::err(format!("{} is a {} connection; the query editor is for databases, Redis and MongoDB.", c.name, kind_name(c.kind))));
    }
    Ok(c)
}

/// How the opened query runs, and what to tell the agent when it doesn't: a read-only
/// connection's own session refuses writes; a read by its words runs on a read-only session;
/// anything else needs `may_change`.
fn run_mode(scope: &Scope, c: &ConnectionConfig, sql: &str, want: bool) -> (Option<AutoRun>, String) {
    if !want {
        return (None, String::new());
    }
    if c.read_only {
        return (Some(AutoRun::Full), String::new());
    }
    if reads_only(c, sql) {
        return (Some(AutoRun::ReadOnly), String::new());
    }
    match may_change(scope, c) {
        Ok(()) => (Some(AutoRun::Full), String::new()),
        Err(why) => (None, format!(" It wasn't run, since it may change data: {} The user presses Run in the tab.", why.content)),
    }
}

#[async_trait]
impl Tool for OpenQuery {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_open_query".into(),
            description: "Open SQL in DevCat's query editor (a query tab in the DevCat window) on a connection, for the user to see, edit and run, with the results in DevCat's grid. Give the SQL as `query`, a .sql `file`, or a `saved` query (name or id; it opens on its own connection unless `connection` says otherwise). With `run: true` DevCat also runs the whole script in that tab: a read runs on a read-only session; a script that changes data runs only where agents may change that connection's data, else the tab opens and the user presses Run. You don't get the rows back (use `db_query` for that). Use it when the user wants a query or .sql file opened or run in DevCat. Needs the DevCat app open.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id (a saved query's own when left out)" },
                    "query": { "type": "string", "description": "The SQL (MongoDB shell / Redis commands on those connections)" },
                    "file": { "type": "string", "description": "A .sql file instead: absolute, or relative to your folder" },
                    "saved": { "type": "string", "description": "A saved query instead: its name or id" },
                    "database": { "type": "string", "description": "Database to run it in (the connection's default when left out)" },
                    "title": { "type": "string", "description": "The tab's title" },
                    "run": { "type": "boolean", "description": "Also run it in the tab (default false)" }
                }
            }),
        }
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        if !arg_bool(input, "run").unwrap_or(false) {
            return Kind::Read;
        }
        // a file's or saved query's text isn't known here: judged a write, so the DevCat Agent asks
        let query = input.get("query").and_then(Value::as_str).unwrap_or("");
        match find(&self.0, input) {
            Ok(c) if !query.trim().is_empty() && reads_only(&c, query) => Kind::Query,
            _ => Kind::Exec,
        }
    }
    fn summary(&self, input: &Value) -> String {
        let what = arg(input, "file").or_else(|| arg(input, "saved")).unwrap_or_else(|| input.get("query").and_then(Value::as_str).unwrap_or("").trim().to_string());
        let verb = if arg_bool(input, "run").unwrap_or(false) { "run" } else { "open" };
        format!("{verb} on {}: {what}", input.get("connection").and_then(Value::as_str).unwrap_or("its connection"))
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        // `devcat mcp` with DevCat closed: no window in this process
        if crate::network::passive() {
            return ToolOutput::err("DevCat isn't open, and the query editor is in its window: the user opens DevCat and asks again.");
        }
        let (sql, name, saved) = match sql_to_open(&self.0, &input, ctx).await {
            Ok(x) => x,
            Err(e) => return e,
        };
        let c = match open_query_conn(&self.0, &input, saved.as_ref()) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if let Err(e) = network::usable(&self.0, &c) {
            return e;
        }
        let (run, note) = run_mode(&self.0, &c, &sql, arg_bool(&input, "run").unwrap_or(false));
        let q = OpenedQuery {
            conn_id: c.id.clone(),
            database: arg(&input, "database").or_else(|| saved.as_ref().and_then(|q| q.database.clone())),
            sql,
            title: arg(&input, "title").or(name),
            saved_query_id: saved.map(|q| q.id),
            run,
        };
        #[cfg(feature = "gui")]
        if let Some(app) = crate::events::app() {
            return match crate::opened::show_query(app, q) {
                Ok(()) => {
                    let did = if run.is_some() {
                        "Opened it in DevCat's query editor and it's running there; the user sees the results (they don't come back to you)."
                    } else {
                        "Opened it in DevCat's query editor."
                    };
                    ToolOutput::ok(format!("{did} Connection {}.{note}", c.name))
                }
                Err(e) => ToolOutput::err(e),
            };
        }
        let _ = (q, note);
        ToolOutput::err("DevCat runs without windows here (`devcat serve`), so there's no query editor to open.")
    }
}

/// The file the model means: relative to its folder, `~` the home folder.
fn file_to_open(ctx: &ToolCtx, p: &str) -> PathBuf {
    let p = local_path(ctx, p);
    if let Some(rest) = p.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\']))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_queries_run_only_as_far_as_the_agent_may() {
            let conn = |v: Value| -> ConnectionConfig { serde_json::from_value(v).unwrap() };
        let rw = conn(json!({ "id": "1", "name": "pmb", "kind": "postgres", "mcpAccess": "full" }));
        let ro = conn(json!({ "id": "2", "name": "cbt", "kind": "postgres", "readOnly": true }));
        let read = conn(json!({ "id": "3", "name": "dev", "kind": "postgres", "mcpAccess": "read" }));
        let (agent, mcp) = (Scope::default(), Scope { mcp: true, ..Scope::default() });
        let (sel, upd) = ("select * from peserta where tahun = '2026'", "update peserta set paid = true");
        // asked not to run: never runs
        assert_eq!(run_mode(&mcp, &rw, sel, false).0, None);
        // a read by its words: on a read-only session, whatever the access
        assert_eq!(run_mode(&mcp, &read, sel, true).0, Some(AutoRun::ReadOnly));
        assert_eq!(run_mode(&mcp, &rw, sel, true).0, Some(AutoRun::ReadOnly));
        // a read-only connection's own session refuses writes already
        assert_eq!(run_mode(&mcp, &ro, upd, true).0, Some(AutoRun::Full));
        // a write: only where the agent may change data
        assert_eq!(run_mode(&mcp, &rw, upd, true).0, Some(AutoRun::Full));
        assert_eq!(run_mode(&agent, &read, upd, true).0, Some(AutoRun::Full), "the DevCat Agent asked the user first (Kind::Exec)");
        let (run, note) = run_mode(&mcp, &read, upd, true);
        assert_eq!(run, None);
        assert!(note.contains("wasn't run") && note.contains("Full"), "{note}");
    }

    #[test]
    fn opened_queries_reach_the_window_as_it_reads_them() {
        let q = OpenedQuery { conn_id: "c1".into(), database: None, sql: "select 1".into(), title: Some("a.sql".into()), saved_query_id: None, run: Some(AutoRun::ReadOnly) };
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["connId"], "c1");
        assert_eq!(v["savedQueryId"], Value::Null);
        assert_eq!(v["run"], "read-only", "openedFiles.ts reads full / read-only");
        assert_eq!(serde_json::to_value(AutoRun::Full).unwrap(), "full");
    }

    #[test]
    fn delegated_tasks_fit_the_models_window_and_answers_stay_short() {
        assert!(task_fits(&"x".repeat(4 * 8_000), 32_768).is_ok(), "8k tokens fit a quarter of 32k");
        let why = task_fits(&"x".repeat(4 * 9_000), 32_768).unwrap_err();
        assert!(why.contains("9000 tokens") && why.contains("under 8192"), "{why}");
        assert!(task_fits(&"x".repeat(4 * 40_000), 196_608).is_ok());
        assert_eq!(clip("short", 10), "short");
        let long = clip(&"é".repeat(20), 10);
        assert!(long.starts_with(&"é".repeat(10)) && long.contains("[… cut"), "{long}");
    }

    #[test]
    fn files_to_open_start_in_the_agents_folder() {
        let dir = std::env::temp_dir().join("proj");
        let ctx = ToolCtx::at(&dir);
        assert_eq!(file_to_open(&ctx, "docs/manual.pdf"), dir.join("docs/manual.pdf"));
        let abs = std::env::temp_dir().join("elsewhere.pdf");
        assert_eq!(file_to_open(&ctx, &abs.to_string_lossy()), abs);
        let home = dirs::home_dir().unwrap();
        assert_eq!(file_to_open(&ctx, "~/notes/a.md"), home.join("notes/a.md"));
        assert_eq!(file_to_open(&ctx, "~"), home);
        assert_eq!(file_to_open(&ctx, "~other/a.md"), PathBuf::from("~other/a.md"), "another user's home isn't expanded");
    }

    #[test]
    fn jobs_stay_in_the_agents_folder() {
        let root = std::env::temp_dir().join("proj").to_string_lossy().into_owned();
        assert!(within(&root, &root));
        assert!(within(&Path::new(&root).join("api").to_string_lossy(), &root));
        assert!(!within(&std::env::temp_dir().join("other").to_string_lossy(), &root));
        assert!(!within(&Path::new(&root).join("..").join("other").to_string_lossy(), &root));
        assert!(!within(&root, ""));
    }
}
