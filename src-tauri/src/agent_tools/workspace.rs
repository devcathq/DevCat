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
    out.push(Arc::new(ConnAdd(s.clone())));
    out.push(Arc::new(Jobs(s.clone())));
    out.push(Arc::new(JobSave(s.clone())));
    out.push(Arc::new(JobRun(s.clone())));
    if !boards(s).is_empty() {
        out.push(Arc::new(Tickets(s.clone())));
        out.push(Arc::new(TicketsFix(s.clone())));
    }
}

/// Connection kinds agents may add (SSH sessions stay the user's: see AGENTS.md).
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
    serde_json::from_value(cfg).map_err(|e| ToolOutput::err(format!("Bad connection settings: {e}")))
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
                    let all = crate::ai::ai_providers_list().await.unwrap_or_default();
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
        let all = crate::ai::ai_providers_list().await.unwrap_or_default();
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
            description: "The user's DevCat ticket boards (Jira, Trello, Linear) and their tickets. Without `board`: each board with its tracker, repository and columns. With `board`: the tickets in `column` (default the board's \"pick from\" column) with key, title, description, labels, assignees and comments; `ticket` (key or id) for one in full. `history: true`: what DevCat's agent did on the board's tickets lately.".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
