//! DevCat Agent tools over what DevCat manages for the user: connections (databases, Redis,
//! MongoDB, Kubernetes, Docker, RabbitMQ) and adding them, network profiles (VPNs, SSH tunnels,
//! Kubernetes port-forwards), query history and saved queries, scheduled agent jobs and ticket
//! boards; here: listing connections, reading a schema, running a query (SQL, MongoDB shell, Redis
//! commands), dumping a database or exporting rows to a file, dropping a database, and reading
//! Kubernetes objects and pod logs. They use the same code as the app, so a connection marked
//! read-only stays read-only for the agent. The same tools are served over MCP by `devcat mcp`
//! (see `mcp_server.rs`) for Claude Code, Codex, Cursor and Gemini, except what can't be undone
//! (dropping a database, deleting a VPN profile, Kubernetes / Docker / RabbitMQ deletes): the user
//! confirms that by typing its name (`Tool::confirm`), which only the app can ask. With DevCat
//! open, `devcat mcp` runs the rest in the app (`agent_bridge.rs`), where VPNs, port forwards, jobs
//! and ticket runs live. A chat opened from a connection starts with its [`brief`].
//!
//! Which connections an agent sees depends on the folder it works in ([`Scope`]): a project with
//! code folders keeps its connections to those folders; connections in no project, or in a
//! project without folders, are everyone's. SSH sessions are never offered (see AGENTS.md), nor
//! connections whose AI access is off ([`McpAccess`]). Over MCP, changing data needs `Full`.
//! Network profiles have the same AI access (see `network.rs`); ticket boards and notes follow
//! the same folder rule. The DevCat Agent asks the user before anything that changes something.
//!
//! One file per area: `db.rs` (beyond schema and query), `k8s.rs` (beyond get and logs),
//! `docker.rs`, `rabbitmq.rs`, `network.rs` and `workspace.rs` (connections, history, jobs,
//! tickets).
mod db;
mod docker;
mod k8s;
mod network;
mod rabbitmq;
mod workspace;

use crate::db::{ExecOptions, QueryResult, TableRef};
use crate::models::{ConnectionConfig, ConnectionKind, McpAccess, Project};
use crate::state::St;
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Registry, Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Rows a query returns unless the model asks for more; and the most it may ask for.
const DEFAULT_ROWS: usize = 100;
const MAX_ROWS: usize = 1000;
/// Longest cell shown; longer values keep their start.
const MAX_CELL: usize = 300;

/// Whose connections the tools work with. Resolved on every call, so connections and projects
/// changed in the app apply right away.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// This project (an id), whatever the folder.
    pub project: Option<String>,
    /// Else the project whose code folder holds this one.
    pub folder: Option<PathBuf>,
    /// Served over MCP: changing data needs the connection's AI access to be full (the DevCat
    /// Agent asks the user instead).
    pub mcp: bool,
    /// A run nobody watches that works on text from outside (a ticket's agent): it gets the
    /// connection tools, but none that reach ticket boards, schedule jobs, add connections or
    /// add, change or delete network profiles.
    pub unattended: bool,
}

impl Scope {
    pub fn folder(dir: &Path) -> Scope {
        Scope { folder: Some(dir.to_path_buf()), ..Scope::default() }
    }

    /// The project the agent works in, if any.
    pub fn current(&self, projects: &[Project]) -> Option<Project> {
        if let Some(id) = &self.project {
            return projects.iter().find(|p| &p.id == id).cloned();
        }
        crate::projects::for_folder(projects, self.folder.as_deref()?).cloned()
    }
}

/// What a scope sees right now.
pub struct View {
    pub project: Option<Project>,
    pub connections: Vec<ConnectionConfig>,
    projects: Vec<Project>,
}

impl View {
    /// What agents may do with `c` (its own setting, else its project's).
    pub fn access(&self, c: &ConnectionConfig) -> McpAccess {
        access(c, &self.projects)
    }

    /// [`describe`], and over MCP whether agents may change its data.
    pub fn line(&self, c: &ConnectionConfig, mcp: bool) -> String {
        let l = describe(c);
        if mcp && !c.read_only {
            let a = if self.access(c) == McpAccess::Full { "agents may change data" } else { "agents read only" };
            return format!("{l} ({a})");
        }
        l
    }
}

/// What agents may do with `c`, given every project.
pub fn access(c: &ConnectionConfig, projects: &[Project]) -> McpAccess {
    c.agent_access(projects.iter().find(|p| Some(&p.id) == c.project_id.as_ref()))
}

pub fn view(scope: &Scope) -> View {
    let Some(st) = St::try_get() else { return View { project: None, connections: vec![], projects: vec![] } };
    let projects = st.projects.list().unwrap_or_default();
    let project = scope.current(&projects);
    let connections = st.store.load_all().unwrap_or_default().into_iter().filter(|c| visible(c, &projects, project.as_ref())).collect();
    View { project, connections, projects }
}

/// Whether an agent working in `current` sees `c`.
pub fn visible(c: &ConnectionConfig, projects: &[Project], current: Option<&Project>) -> bool {
    if c.kind == ConnectionKind::Ssh || access(c, projects) == McpAccess::Off {
        return false;
    }
    in_reach(c.project_id.as_deref(), projects, current)
}

/// Whether what belongs to project `pid` (a connection, network profile, note or ticket board)
/// is for an agent working in `current`: a project with code folders keeps its things to them;
/// no project, a project without folders or one that's gone, everyone's.
pub(crate) fn in_reach(pid: Option<&str>, projects: &[Project], current: Option<&Project>) -> bool {
    let Some(pid) = pid else { return true };
    match projects.iter().find(|p| p.id == pid) {
        Some(p) if !p.folders.is_empty() => current.is_some_and(|cur| cur.id == pid),
        _ => true,
    }
}

/// The project the model names (by name or id) among those `v`'s agent may put things in:
/// Ok(None) for "none" / "general".
fn project_named(v: &View, want: &str) -> Result<Option<String>, ToolOutput> {
    if ["none", "general", ""].contains(&want.trim().to_ascii_lowercase().as_str()) {
        return Ok(None);
    }
    let open: Vec<&Project> = v.projects.iter().filter(|p| in_reach(Some(&p.id), &v.projects, v.project.as_ref())).collect();
    match open.iter().find(|p| p.id == want || p.name.eq_ignore_ascii_case(want)) {
        Some(p) => Ok(Some(p.id.clone())),
        None => Err(ToolOutput::err(format!(
            "There's no DevCat project called \"{want}\" here. Projects: {}.",
            if open.is_empty() { "none".to_string() } else { open.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ") }
        ))),
    }
}

/// The DevCat tools for `scope`.
pub fn add(tools: &mut Registry, scope: &Scope) {
    for t in for_scope(scope) {
        tools.add_arc(t);
    }
}

/// The tools for what `scope` sees: always the ones that list and add connections and network
/// profiles and manage scheduled jobs; per connection kind the ones that work with it (no
/// Docker tools without a Docker connection, and so on), so agents get no tools they can't use.
pub fn for_scope(scope: &Scope) -> Vec<Arc<dyn Tool>> {
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    if St::try_get().is_none() {
        return out;
    }
    let v = view(scope);
    let s = Arc::new(scope.clone());
    let has = |k: ConnectionKind| v.connections.iter().any(|c| c.kind == k);
    out.push(Arc::new(Connections(s.clone())));
    workspace::add(&mut out, &s, &v);
    network::add(&mut out, &s);
    if v.connections.iter().any(|c| is_db(c.kind)) {
        out.push(Arc::new(DbSchema(s.clone())));
        out.push(Arc::new(DbQuery(s.clone())));
        db::add(&mut out, &s);
    }
    if v.connections.iter().any(|c| has_schema(c.kind)) {
        out.push(Arc::new(DbDump(s.clone())));
        out.push(Arc::new(DbExport(s.clone())));
    }
    // Over MCP DevCat can't ask the user to type the database's name, so dropping one is for the
    // DevCat Agent only (see `DbDrop`).
    if !scope.mcp && v.connections.iter().any(|c| can_drop(c.kind) && !c.read_only) {
        out.push(Arc::new(DbDrop(s.clone())));
    }
    if has(ConnectionKind::Kubernetes) {
        out.push(Arc::new(K8sGet(s.clone())));
        out.push(Arc::new(K8sLogs(s.clone())));
        k8s::add(&mut out, &s);
    }
    if has(ConnectionKind::Docker) {
        docker::add(&mut out, &s);
    }
    if has(ConnectionKind::Rabbitmq) {
        rabbitmq::add(&mut out, &s);
    }
    out
}

// ---------- shared by the tool files

/// A trimmed, non-empty string argument.
fn arg(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn arg_bool(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(Value::as_bool)
}

/// The connection the model means, of `kind`.
fn conn_of(scope: &Scope, input: &Value, kind: ConnectionKind) -> Result<ConnectionConfig, ToolOutput> {
    let c = find(scope, input)?;
    if c.kind != kind {
        return Err(ToolOutput::err(format!("{} is a {} connection, not {}.", c.name, kind_name(c.kind), kind_name(kind))));
    }
    network::usable(scope, &c)?;
    Ok(c)
}

/// Whether the agent may change something through `c`: never on a read-only connection, and
/// over MCP only with the connection's AI access at Full (the DevCat Agent asks the user instead).
fn may_change(scope: &Scope, c: &ConnectionConfig) -> Result<(), ToolOutput> {
    if c.read_only {
        return Err(ToolOutput::err(format!("{} is a read-only connection: DevCat doesn't change anything through it.", c.name)));
    }
    if scope.mcp && view(scope).access(c) != McpAccess::Full {
        return Err(ToolOutput::err(format!(
            "DevCat only lets agents read {} over MCP. To allow changes, the user sets the connection's AI / MCP access to \"Full\" in DevCat (Edit connection → Advanced).",
            c.name
        )));
    }
    Ok(())
}

/// A path the model gave, for the app: relative ones start in the agent's folder (over MCP the
/// tool may run in DevCat, whose own folder is elsewhere); `~` stays for the callee to expand.
fn local_path(ctx: &ToolCtx, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() || p.starts_with('~') || ctx.cwd.as_os_str().is_empty() {
        return p.to_string();
    }
    ctx.cwd.join(path).to_string_lossy().into_owned()
}

/// A pretty JSON value, for tools that show an object as it is.
fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Keys whose values agents don't get to see (environment variables, Helm values, ...).
fn secret_name(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["password", "passwd", "secret", "token", "apikey", "api_key", "api-key", "private", "credential", "auth"].iter().any(|w| k.contains(w))
}

/// Replaces the values of secret-looking keys anywhere in `v` (and `NAME=value` strings with a
/// secret-looking NAME) with a placeholder.
fn redact_secrets(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                if secret_name(k) && (val.is_string() || val.is_number()) {
                    *val = Value::String("<hidden>".into());
                } else {
                    redact_secrets(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact_secrets),
        Value::String(s) => {
            if let Some((name, _)) = s.split_once('=')
                && !name.contains(char::is_whitespace)
                && secret_name(name)
            {
                *s = format!("{name}=<hidden>");
            }
        }
        _ => {}
    }
}

/// Names of the connections `scope` sees (for the chat's "Using" line).
pub fn names(scope: &Scope) -> Vec<String> {
    view(scope).connections.into_iter().map(|c| c.name).collect()
}

/// One line per connection: name, kind, default database, read-only, id.
pub fn describe(c: &ConnectionConfig) -> String {
    let mut l = format!("- {} ({}", c.name, kind_name(c.kind));
    if let Some(db) = c.database.as_deref().filter(|d| !d.is_empty()) {
        l.push_str(&format!(", database {db}"));
    }
    if c.read_only {
        l.push_str(", read-only");
    }
    if let Some(n) = network::via(c) {
        l.push_str(&format!(", through network profile {n}"));
    }
    l.push_str(&format!(") id {}", c.id));
    l
}

/// Whether `db_schema` reads it (SQL databases and MongoDB).
pub fn has_schema(k: ConnectionKind) -> bool {
    is_db(k) && k != ConnectionKind::Redis
}

fn is_db(k: ConnectionKind) -> bool {
    !matches!(k, ConnectionKind::Kubernetes | ConnectionKind::Rabbitmq | ConnectionKind::Docker | ConnectionKind::Ssh)
}

pub fn kind_name(k: ConnectionKind) -> &'static str {
    match k {
        ConnectionKind::Postgres => "postgres",
        ConnectionKind::Mysql => "mysql",
        ConnectionKind::Sqlite => "sqlite",
        ConnectionKind::Mssql => "mssql",
        ConnectionKind::Mongodb => "mongodb",
        ConnectionKind::Redis => "redis",
        ConnectionKind::Kubernetes => "kubernetes",
        ConnectionKind::Rabbitmq => "rabbitmq",
        ConnectionKind::Docker => "docker",
        ConnectionKind::Ssh => "ssh",
    }
}

/// The connection the model means: by id, else by name (any case), among those `scope` sees.
fn find(scope: &Scope, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    let Some(want) = input.get("connection").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(ToolOutput::err("Missing `connection`: the name or id of a DevCat connection (see `devcat_connections`)."));
    };
    let all = view(scope).connections;
    if let Some(c) = all.iter().find(|c| c.id == want) {
        return Ok(c.clone());
    }
    let named: Vec<_> = all.iter().filter(|c| c.name.eq_ignore_ascii_case(want)).collect();
    match named.as_slice() {
        [c] => Ok((*c).clone()),
        [] => Err(ToolOutput::err(format!("No DevCat connection is called \"{want}\". Connections: {}.", all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")))),
        _ => Err(ToolOutput::err(format!("Several connections are called \"{want}\"; use the id: {}.", named.iter().map(|c| format!("{} ({})", c.id, kind_name(c.kind))).collect::<Vec<_>>().join(", ")))),
    }
}

fn conn_summary(input: &Value) -> String {
    input.get("connection").and_then(Value::as_str).unwrap_or("?").to_string()
}

// ---------- devcat_connections

struct Connections(Arc<Scope>);

#[async_trait]
impl Tool for Connections {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "devcat_connections".into(),
            description: "List the user's DevCat connections (databases, Redis, MongoDB, Kubernetes clusters, Docker engines, RabbitMQ brokers): name, kind, default database, the VPN / tunnel it goes through, whether it's read-only and what agents may do with it. Use the name with the `db_*` (db_schema, db_query, db_dump, db_export…), `k8s_*`, `docker_*` and `rmq_*` tools.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        String::new()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        let v = view(&self.0);
        if v.connections.is_empty() {
            return ToolOutput::ok("The user has no DevCat connections for this folder (`devcat_connection_save` adds one).");
        }
        let mut lines: Vec<String> = v.connections.iter().map(|c| v.line(c, self.0.mcp)).collect();
        if let Some(p) = &v.project {
            lines.insert(0, format!("Project {} (its connections, then the ones every folder gets):", p.name));
        }
        ToolOutput::ok(lines.join("\n"))
    }
}

// ---------- db_schema

struct DbSchema(Arc<Scope>);

#[async_trait]
impl Tool for DbSchema {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_schema".into(),
            description: "Read the structure of a database connection: without `table`, every table with its columns (types, primary keys) and the views; with `table`, that table's columns, indexes and foreign keys. For SQL databases and MongoDB.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string", "description": "Database, if not the connection's default" },
                    "schema": { "type": "string", "description": "Schema (Postgres, SQL Server), if not the default" },
                    "table": { "type": "string", "description": "One table to describe in detail" }
                },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        let t = input.get("table").and_then(Value::as_str).map(|t| format!(" · {t}")).unwrap_or_default();
        format!("{}{t}", conn_summary(input))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(90)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match find(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if !has_schema(c.kind) {
            return ToolOutput::err(format!("{} is a {} connection; `db_schema` reads SQL databases and MongoDB.", c.name, kind_name(c.kind)));
        }
        if let Err(e) = network::usable(&self.0, &c) {
            return e;
        }
        let arg = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let (database, schema) = (arg("database").or(c.database.clone().filter(|d| !d.is_empty())), arg("schema"));
        let driver = match St::get().driver(&c.id).await {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        if let Some(name) = arg("table") {
            return match driver.table_structure(&TableRef { database, schema, name: name.clone() }).await {
                Ok(t) => {
                    let mut out = vec![format!("Table {name}")];
                    if let Some(cm) = t.comment.filter(|s| !s.is_empty()) {
                        out.push(format!("Comment: {cm}"));
                    }
                    out.push("Columns:".into());
                    for col in &t.columns {
                        let mut l = format!("- {} {}", col.name, col.data_type);
                        if col.is_primary_key {
                            l.push_str(" PRIMARY KEY");
                        }
                        if !col.nullable {
                            l.push_str(" NOT NULL");
                        }
                        if let Some(d) = &col.default {
                            l.push_str(&format!(" DEFAULT {d}"));
                        }
                        if let Some(cm) = col.comment.as_deref().filter(|s| !s.is_empty()) {
                            l.push_str(&format!(" -- {cm}"));
                        }
                        out.push(l);
                    }
                    if !t.indexes.is_empty() {
                        out.push(format!("Indexes: {}", serde_json::to_string(&t.indexes).unwrap_or_default()));
                    }
                    if !t.foreign_keys.is_empty() {
                        out.push(format!("Foreign keys: {}", serde_json::to_string(&t.foreign_keys).unwrap_or_default()));
                    }
                    ToolOutput::ok(out.join("\n"))
                }
                Err(e) => ToolOutput::err(e.to_string()),
            };
        }
        match tables_text(&driver, database.as_deref(), schema.as_deref()).await {
            Ok(Some(text)) => ToolOutput::ok(text),
            Ok(None) => ToolOutput::ok("No tables found (check `database` / `schema`)."),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

/// Every table with its columns, and the views, one per line; None when there are none.
async fn tables_text(driver: &crate::db::DynDriver, database: Option<&str>, schema: Option<&str>) -> crate::error::AppResult<Option<String>> {
    let s = driver.schema_snapshot(database, schema).await?;
    let mut out = Vec::new();
    for t in &s.tables {
        let name = t.table.as_ref().map(qualified).unwrap_or_default();
        let cols: Vec<String> = t.columns.iter().map(|c| format!("{} {}{}", c.name, c.data_type, if c.is_primary_key { " PK" } else { "" })).collect();
        out.push(format!("{name}({})", cols.join(", ")));
    }
    for (v, _) in &s.views {
        out.push(format!("view {}", qualified(v)));
    }
    Ok((!out.is_empty()).then(|| format!("{} tables, {} views:\n{}", s.tables.len(), s.views.len(), out.join("\n"))))
}

/// A failed connect. A connection behind a VPN / tunnel connects it first, in the process that
/// owns networks: `devcat mcp` doesn't start them itself (the app does, see `agent_bridge.rs`).
fn connect_error(scope: &Scope, c: &ConnectionConfig, e: &str) -> String {
    let mut msg = format!("Couldn't connect to {}: {e}", c.name);
    if scope.mcp && crate::network::passive() && network::via(c).is_some() {
        msg.push_str(" (This connection goes through a VPN or tunnel profile, and DevCat isn't open to connect it: the user opens DevCat, or connects it in DevCat's Network panel.)");
    }
    msg
}

fn qualified(r: &TableRef) -> String {
    match &r.schema {
        Some(s) if !s.is_empty() => format!("{s}.{}", r.name),
        _ => r.name.clone(),
    }
}

// ---------- db_query

struct DbQuery(Arc<Scope>);

#[async_trait]
impl Tool for DbQuery {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_query".into(),
            description: "Run a query on one of the user's DevCat connections and get the rows back: SQL for Postgres, MySQL, SQLite and SQL Server; mongo shell syntax for MongoDB (`db.users.find({...}).limit(10)`, `db.orders.aggregate([...])`); one command per line for Redis (`SCAN 0 MATCH user:* COUNT 100`, `HGETALL user:1`). Prefer reading: SELECT with a LIMIT, only the columns you need. Statements that change data ask the user first, and read-only connections refuse them.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "query": { "type": "string" },
                    "database": { "type": "string", "description": "Database (or Redis db number), if not the connection's default" },
                    "max_rows": { "type": "integer", "description": "Rows to return per statement (default 100, at most 1000)" }
                },
                "required": ["connection", "query"]
            }),
        }
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        let query = input.get("query").and_then(Value::as_str).unwrap_or("");
        match find(&self.0, input) {
            Ok(c) if reads_only(&c, query) => Kind::Query,
            _ => Kind::Exec,
        }
    }
    fn summary(&self, input: &Value) -> String {
        let q = input.get("query").and_then(Value::as_str).unwrap_or("").trim();
        format!("{}: {q}", conn_summary(input))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(120)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match find(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if !is_db(c.kind) {
            return ToolOutput::err(format!("{} is a {} connection; `db_query` is for databases, Redis and MongoDB.", c.name, kind_name(c.kind)));
        }
        if let Err(e) = network::usable(&self.0, &c) {
            return e;
        }
        let query = input.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if query.is_empty() {
            return ToolOutput::err("Missing `query`.");
        }
        if self.0.mcp && !reads_only(&c, query) && view(&self.0).access(&c) != McpAccess::Full {
            return ToolOutput::err(format!(
                "DevCat only lets agents read {} over MCP, and this query may change data. To allow changes, the user sets the connection's AI / MCP access to \"Full\" in DevCat (Edit connection → Advanced); a read-only connection can't be.",
                c.name
            ));
        }
        let max_rows = input.get("max_rows").and_then(Value::as_u64).map(|n| (n as usize).clamp(1, MAX_ROWS)).unwrap_or(DEFAULT_ROWS);
        let database = input.get("database").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        // A query judged a read by its words ran without asking (`Kind::Query`), or over MCP
        // without full access: run it on a read-only session, so a write dressed up as a read fails.
        let driver = if reads_only(&c, query) { St::get().read_only_driver(&c.id).await } else { St::get().driver(&c.id).await };
        let driver = match driver {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        let opts = ExecOptions { database, schema: None, max_rows: Some(max_rows) };
        match driver.execute(query, &opts).await {
            Ok(results) => {
                let failed = results.iter().any(|r| r.message.as_deref().is_some_and(|m| m.starts_with("(error)")));
                let text = results.iter().map(format_result).collect::<Vec<_>>().join("\n\n");
                let text = if text.trim().is_empty() { "(no results)".to_string() } else { text };
                if failed { ToolOutput::err(text) } else { ToolOutput::ok(text) }
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

/// Whether the query only reads. The driver refuses writes on a read-only connection, so there
/// every query is a read.
fn reads_only(c: &ConnectionConfig, query: &str) -> bool {
    c.read_only || crate::remote::readonly::allowed(Some(c.kind), query)
}

/// One statement's result as text: a header line and rows separated by ` | `.
fn format_result(r: &QueryResult) -> String {
    let mut out = Vec::new();
    if !r.columns.is_empty() {
        out.push(r.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" | "));
        for row in &r.rows {
            out.push(row.iter().map(cell).collect::<Vec<_>>().join(" | "));
        }
        let n = r.rows.len();
        out.push(format!("({n} row{}{}, {} ms)", if n == 1 { "" } else { "s" }, if r.truncated { ", more not shown: add a LIMIT / filter or raise max_rows" } else { "" }, r.elapsed_ms));
    } else if let Some(n) = r.rows_affected {
        out.push(format!("{n} row{} affected ({} ms)", if n == 1 { "" } else { "s" }, r.elapsed_ms));
    }
    if let Some(m) = r.message.as_deref().filter(|m| !m.is_empty()) {
        out.push(m.to_string());
    }
    out.join("\n")
}

fn cell(v: &Value) -> String {
    let s = match v {
        Value::Null => "NULL".to_string(),
        Value::String(s) => s.replace('\n', "\\n"),
        other => other.to_string(),
    };
    if s.chars().count() > MAX_CELL { format!("{}…", s.chars().take(MAX_CELL).collect::<String>()) } else { s }
}

// ---------- db_dump, db_export

fn str_arg(input: &Value, k: &str) -> Option<String> {
    input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// `name` made safe for a file name.
fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() { "export".into() } else { s }
}

/// Where a dump or export goes: `path` (`~/` for the home folder, relative to the chat's folder),
/// else `default` in the Downloads folder.
fn out_path(input: &Value, cwd: &Path, default: &str) -> PathBuf {
    let home = dirs::home_dir();
    match str_arg(input, "path") {
        Some(p) => match (p.strip_prefix("~/").or(p.strip_prefix("~\\")), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ if p == "~" && home.is_some() => home.unwrap(),
            _ => cwd.join(p),
        },
        None => dirs::download_dir().or(home).unwrap_or_else(|| cwd.to_path_buf()).join(default),
    }
}

/// Writing a file in the chat's folder is an edit; anywhere else (Downloads) it asks.
fn file_kind(path: &Path, ctx: &ToolCtx) -> Kind {
    if path.starts_with(&ctx.cwd) { Kind::Edit } else { Kind::EditOutside }
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// The connection a dump, export, import, restore or drop works on, if it's a database with
/// tables (and the agent may use the network it goes through).
fn db_conn(scope: &Scope, input: &Value, what: &str) -> Result<ConnectionConfig, ToolOutput> {
    let c = find(scope, input)?;
    if !has_schema(c.kind) {
        return Err(ToolOutput::err(format!("{} is a {} connection; `{what}` works on SQL databases and MongoDB.", c.name, kind_name(c.kind))));
    }
    network::usable(scope, &c)?;
    Ok(c)
}

/// The database a call means: its `database`, else the connection's default.
fn database_of(c: &ConnectionConfig, input: &Value) -> Option<String> {
    str_arg(input, "database").or(c.database.clone().filter(|d| !d.trim().is_empty()))
}

struct DbDump(Arc<Scope>);

impl DbDump {
    fn path(&self, input: &Value, cwd: &Path) -> PathBuf {
        let c = find(&self.0, input).ok();
        let name = c.as_ref().map(|c| database_of(c, input).unwrap_or_else(|| c.name.clone())).unwrap_or_else(|| "database".into());
        // MongoDB: a folder of `<collection>.jsonl` files
        let ext = if c.is_some_and(|c| c.kind == ConnectionKind::Mongodb) { "" } else { ".sql" };
        out_path(input, cwd, &format!("{}-{}{ext}", slug(&name), stamp()))
    }
}

#[async_trait]
impl Tool for DbDump {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_dump".into(),
            description: "Dump (back up) a database of one of the user's connections to a file on this computer: a .sql script with CREATE TABLE statements and INSERTs for SQL databases, a folder of `<collection>.jsonl` files for MongoDB. The data goes to the file, not to you. Without `path` it goes to the user's Downloads folder. `tables` dumps only those; `structure_only` leaves the rows out. DevCat's Restore (Data transfer) reads the file back. Suggest a dump before anything that deletes data.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string", "description": "Database, if not the connection's default" },
                    "schema": { "type": "string", "description": "Schema (Postgres, SQL Server), if not the default" },
                    "tables": { "type": "array", "items": { "type": "string" }, "description": "Only these tables (default: all)" },
                    "structure_only": { "type": "boolean", "description": "Tables and views without their rows" },
                    "path": { "type": "string", "description": "File to write (a folder for MongoDB); must not exist yet" }
                },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        file_kind(&self.path(input, &ctx.cwd), ctx)
    }
    fn summary(&self, input: &Value) -> String {
        let db = str_arg(input, "database").map(|d| format!(" · {d}")).unwrap_or_default();
        let to = str_arg(input, "path").unwrap_or_else(|| "Downloads".into());
        format!("{}{db} → {to}", conn_summary(input))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(60 * 60)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input, "db_dump") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let path = self.path(&input, &ctx.cwd);
        if path.exists() {
            return ToolOutput::err(format!("{} already exists: pick another `path`.", path.display()));
        }
        let driver = match St::get().driver(&c.id).await {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        let mut options = json!({ "structureOnly": input.get("structure_only").and_then(Value::as_bool).unwrap_or(false) });
        if let Some(t) = input.get("tables").and_then(Value::as_array).filter(|t| !t.is_empty()) {
            options["tables"] = Value::Array(t.clone());
        }
        let database = database_of(&c, &input);
        match crate::db::transfer::backup_database(&driver, database.as_deref(), str_arg(&input, "schema").as_deref(), &path, &options, &|_| {}).await {
            Ok(summary) => ToolOutput::ok(format!("Dumped {} to {}: {summary}", database.as_deref().unwrap_or(&c.name), path.display())),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

/// File formats `db_export` writes.
const EXPORT_FORMATS: [&str; 6] = ["csv", "tsv", "json", "jsonl", "sql", "xlsx"];

struct DbExport(Arc<Scope>);

impl DbExport {
    fn format(input: &Value) -> String {
        str_arg(input, "format").map(|f| f.to_ascii_lowercase()).unwrap_or_else(|| "csv".into())
    }
    fn path(input: &Value, cwd: &Path) -> PathBuf {
        let name = str_arg(input, "table").unwrap_or_else(|| "query".into());
        out_path(input, cwd, &format!("{}-{}.{}", slug(&name), stamp(), Self::format(input)))
    }
}

#[async_trait]
impl Tool for DbExport {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_export".into(),
            description: "Export a table, or the rows of a reading query, from one of the user's connections to a file on this computer: csv, tsv, json, jsonl, sql (INSERT statements) or xlsx. The rows go to the file, not to you. Give `table` or `query` (SQL, or mongo shell syntax for MongoDB); the query must only read. Without `path` the file goes to the user's Downloads folder.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "table": { "type": "string", "description": "Table (or collection) to export" },
                    "query": { "type": "string", "description": "Instead of a table: a query whose rows to export" },
                    "database": { "type": "string", "description": "Database, if not the connection's default" },
                    "schema": { "type": "string", "description": "Schema of the table (Postgres, SQL Server), if not the default" },
                    "filter": { "type": "string", "description": "With `table`: a WHERE expression (a filter document for MongoDB)" },
                    "format": { "type": "string", "enum": EXPORT_FORMATS, "description": "Default csv" },
                    "path": { "type": "string", "description": "File to write; must not exist yet" }
                },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        file_kind(&Self::path(input, &ctx.cwd), ctx)
    }
    fn summary(&self, input: &Value) -> String {
        let what = str_arg(input, "table").or_else(|| str_arg(input, "query")).unwrap_or_default();
        let to = str_arg(input, "path").unwrap_or_else(|| format!("Downloads ({})", Self::format(input)));
        format!("{}: {what} → {to}", conn_summary(input))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(60 * 60)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input, "db_export") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let format = Self::format(&input);
        if !EXPORT_FORMATS.contains(&format.as_str()) {
            return ToolOutput::err(format!("Unknown `format` {format}: one of {}.", EXPORT_FORMATS.join(", ")));
        }
        let database = database_of(&c, &input);
        let source = match (str_arg(&input, "table"), str_arg(&input, "query")) {
            (Some(name), None) => crate::db::transfer::ExportSource::Table(TableRef { database: database.clone(), schema: str_arg(&input, "schema"), name }),
            (None, Some(sql)) => {
                if !reads_only(&c, &sql) {
                    return ToolOutput::err("`db_export` runs only queries that read: export the table, or give a SELECT (a find / aggregate for MongoDB).");
                }
                crate::db::transfer::ExportSource::Query { sql, database: database.clone() }
            }
            _ => return ToolOutput::err("Give either `table` or `query`."),
        };
        let path = Self::path(&input, &ctx.cwd);
        if path.exists() {
            return ToolOutput::err(format!("{} already exists: pick another `path`.", path.display()));
        }
        // An export only reads (its query and `filter` included): a read-only session makes sure.
        let driver = match St::get().read_only_driver(&c.id).await {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        let mut options = json!({});
        if let Some(f) = str_arg(&input, "filter") {
            options["filter"] = json!(f);
        }
        match crate::db::transfer::export_data(&driver, source, &format, &path, &options, &|_| {}).await {
            Ok(n) => ToolOutput::ok(format!("Exported {n} row{} to {}", if n == 1 { "" } else { "s" }, path.display())),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

// ---------- db_drop_database

/// Kinds whose databases `db_drop_database` drops (SQLite is a file, Redis has numbered ones).
fn can_drop(k: ConnectionKind) -> bool {
    matches!(k, ConnectionKind::Postgres | ConnectionKind::Mysql | ConnectionKind::Mssql | ConnectionKind::Mongodb)
}

/// The server's own databases: never dropped from a chat.
fn system_database(k: ConnectionKind, name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let list: &[&str] = match k {
        ConnectionKind::Postgres => &["postgres", "template0", "template1"],
        ConnectionKind::Mysql => &["mysql", "information_schema", "performance_schema", "sys"],
        ConnectionKind::Mssql => &["master", "model", "msdb", "tempdb"],
        ConnectionKind::Mongodb => &["admin", "local", "config"],
        _ => &[],
    };
    list.contains(&n.as_str())
}

/// The statement that drops `name`, and the database to run it in (not `name` itself, where the
/// server can't drop the database a session is in).
fn drop_statement(k: ConnectionKind, name: &str) -> (String, Option<&'static str>) {
    match k {
        ConnectionKind::Postgres => (format!("DROP DATABASE \"{}\"", name.replace('"', "\"\"")), Some("postgres")),
        ConnectionKind::Mysql => (format!("DROP DATABASE `{}`", name.replace('`', "``")), None),
        ConnectionKind::Mssql => (format!("DROP DATABASE [{}]", name.replace(']', "]]")), Some("master")),
        _ => ("db.dropDatabase()".into(), None),
    }
}

/// Drop a database for good. DevCat's own sessions on the connection close first (the server
/// won't drop a database that's in use); other people's sessions still stop it.
async fn drop_database(c: &ConnectionConfig, name: &str) -> crate::error::AppResult<()> {
    use crate::error::AppError;
    let st = St::get();
    let existing = st.driver(&c.id).await?.list_databases().await?;
    if !existing.iter().any(|d| d == name) {
        return Err(AppError::msg(format!("{} has no database \"{name}\". Its databases: {}.", c.name, existing.join(", "))));
    }
    if let Some(d) = st.sessions.write().await.remove(&c.id) {
        d.close().await;
    }
    st.close_read_only(&c.id).await;
    let (sql, run_in) = drop_statement(c.kind, name);
    let mut cfg = st.store.get(&c.id)?;
    if let Some(db) = run_in {
        cfg.database = Some(db.into());
    }
    crate::network::ensure_for(&cfg).await?;
    let driver = crate::db::connect(&cfg).await?;
    let database = if c.kind == ConnectionKind::Mongodb { Some(name.to_string()) } else { run_in.map(String::from) };
    let res = driver.execute(&sql, &ExecOptions { database, schema: None, max_rows: Some(1) }).await;
    driver.close().await;
    let results = res?;
    if let Some(m) = results.iter().filter_map(|r| r.message.as_deref()).find(|m| m.starts_with("(error)")) {
        return Err(AppError::msg(m.to_string()));
    }
    crate::events::emit("connection-changed", json!({ "id": c.id }));
    Ok(())
}

/// Drops a database. It can't be undone, so the user confirms it twice: they allow the call,
/// then type the database's name ([`Tool::confirm`]), and no permission mode, hook or "always
/// allow" skips that. Read-only connections and the server's own databases refuse it.
struct DbDrop(Arc<Scope>);

#[async_trait]
impl Tool for DbDrop {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_drop_database".into(),
            description: "Drop (delete) a whole database on one of the user's Postgres, MySQL, SQL Server or MongoDB connections, with all its tables and data. It can't be undone: only call it when the user asked to drop that database, say which one first, and offer a `db_dump` before it. DevCat then asks the user to confirm twice, the second time by typing the database's name. Read-only connections and the server's own databases (postgres, mysql, master, admin, ...) can't be dropped.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string", "description": "The database to drop, by its exact name" }
                },
                "required": ["connection", "database"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: drop database {}", conn_summary(input), str_arg(input, "database").unwrap_or_default())
    }
    fn confirm(&self, input: &Value) -> Option<String> {
        // nothing to type for a call that names no database: it's refused when it runs
        Some(str_arg(input, "database").unwrap_or_else(|| "(no database)".into()))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(120)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        if self.0.mcp {
            return ToolOutput::err("Databases are dropped from the DevCat Agent in DevCat only, where the user confirms it.");
        }
        let c = match find(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(name) = str_arg(&input, "database") else { return ToolOutput::err("Missing `database`: the exact name of the database to drop.") };
        if !can_drop(c.kind) {
            return ToolOutput::err(format!("{} is a {} connection; `db_drop_database` drops Postgres, MySQL, SQL Server and MongoDB databases.", c.name, kind_name(c.kind)));
        }
        if c.read_only {
            return ToolOutput::err(format!("{} is read-only in DevCat, so nothing is dropped through it.", c.name));
        }
        if system_database(c.kind, &name) {
            return ToolOutput::err(format!("\"{name}\" is one of the server's own databases; DevCat doesn't drop it."));
        }
        match drop_database(&c, &name).await {
            Ok(()) => ToolOutput::ok(format!("Dropped database \"{name}\" on {}.", c.name)),
            Err(e) => ToolOutput::err(format!("Couldn't drop \"{name}\": {e}")),
        }
    }
}

// ---------- the connection a chat is about

/// Longest schema listing put in a chat's instructions; `db_schema` reads the rest.
const MAX_BRIEF_SCHEMA: usize = 16_000;
/// How long reading the connection may take while the chat opens.
const BRIEF_TIMEOUT: Duration = Duration::from_secs(15);

/// For a chat opened from a connection (`agent_open` with a connection): what it is and, read
/// right now, what's in it (a database's tables, a cluster's namespaces), so the user can ask
/// straight away. Only structure, never rows.
pub async fn brief(scope: &Scope, c: &ConnectionConfig) -> String {
    let mut out = vec![
        "# The connection this chat is about".to_string(),
        format!("The user opened this chat from their DevCat connection \"{}\":", c.name),
        describe(c),
        "Their questions are about it unless they say otherwise: use it with the connection tools without asking which connection they mean.".into(),
    ];
    let read = async {
        if c.kind == ConnectionKind::Kubernetes {
            let ns = crate::infra::k8s::k8s_list(St::get(), c.id.clone(), "namespaces".into(), None, None).await.map_err(|e| e.to_string())?;
            let names: Vec<&str> = ns.iter().filter_map(|n| n["metadata"]["name"].as_str()).collect();
            return Ok(format!("Namespaces: {}.", names.join(", ")));
        }
        let driver = St::get().driver(&c.id).await.map_err(|e| connect_error(scope, c, &e.to_string()))?;
        let mut lines = Vec::new();
        if let Ok(v) = driver.server_version().await {
            lines.push(format!("Server: {v}"));
        }
        if let Ok(dbs) = driver.list_databases().await
            && !dbs.is_empty()
        {
            lines.push(format!("Databases: {}", dbs.join(", ")));
        }
        if has_schema(c.kind) {
            let db = c.database.as_deref().filter(|d| !d.is_empty());
            match tables_text(&driver, db, None).await {
                Ok(Some(t)) if t.len() > MAX_BRIEF_SCHEMA => {
                    let cut: String = t.chars().take(MAX_BRIEF_SCHEMA).collect();
                    lines.push(format!("{}:\n{}\n… more not shown: `db_schema` lists them all.", db.unwrap_or("Default database"), cut.rsplit_once('\n').map_or(cut.as_str(), |(a, _)| a)));
                }
                Ok(Some(t)) => lines.push(format!("{}: {t}", db.unwrap_or("Default database"))),
                Ok(None) => lines.push("Its default database has no tables yet.".into()),
                Err(e) => lines.push(format!("(Couldn't read the tables: {e})")),
            }
        }
        Ok::<_, String>(lines.join("\n"))
    };
    match tokio::time::timeout(BRIEF_TIMEOUT, read).await {
        Ok(Ok(s)) if !s.is_empty() => out.push(format!("As it was when the chat opened (read it again with the tools when it matters):\n{s}")),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => out.push(format!("(DevCat couldn't read it when the chat opened: {e})")),
        Err(_) => out.push(format!("(DevCat couldn't read it within {} s when the chat opened; the tools may still reach it.)", BRIEF_TIMEOUT.as_secs())),
    }
    if has_schema(c.kind) {
        out.push("Besides reading and querying, you can dump it to a file (`db_dump`), export tables or query results (`db_export`)".to_string()
            + if can_drop(c.kind) && !c.read_only { " and drop a database (`db_drop_database`; the user confirms it twice, and you offer a dump first)." } else { "." });
    }
    out.join("\n")
}

// ---------- Kubernetes

fn k8s_conn(scope: &Scope, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    conn_of(scope, input, ConnectionKind::Kubernetes)
}

struct K8sGet(Arc<Scope>);

#[async_trait]
impl Tool for K8sGet {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "k8s_get".into(),
            description: "Read Kubernetes objects from one of the user's cluster connections (read only). Without `name`, lists objects of `kind` (pods, deployments, services, configmaps, events, nodes, ... or `apiVersion/Kind`) with their status; with `name`, returns that object as JSON. Secret values are never shown.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Cluster connection name or id" },
                    "kind": { "type": "string", "description": "e.g. pods, deployments, services, events" },
                    "namespace": { "type": "string", "description": "Namespace; empty for all namespaces when listing" },
                    "name": { "type": "string" },
                    "label_selector": { "type": "string", "description": "e.g. app=api" }
                },
                "required": ["connection", "kind"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let ns = s("namespace");
        format!("{}: {} {}{}", conn_summary(input), s("kind"), s("name"), if ns.is_empty() { String::new() } else { format!(" -n {ns}") }).trim_end().to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match k8s_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let Some(kind) = s("kind") else { return ToolOutput::err("Missing `kind`.") };
        let state = St::get();
        if let Some(name) = s("name") {
            return match crate::infra::k8s::k8s_get(state, c.id.clone(), kind, s("namespace"), name).await {
                Ok(mut v) => {
                    redact_secret(&mut v);
                    ToolOutput::ok(serde_json::to_string_pretty(&v).unwrap_or_default())
                }
                Err(e) => ToolOutput::err(e.to_string()),
            };
        }
        match crate::infra::k8s::k8s_list(state, c.id.clone(), kind, s("namespace"), s("label_selector")).await {
            Ok(items) if items.is_empty() => ToolOutput::ok("No objects found."),
            Ok(items) => {
                let lines: Vec<String> = items.iter().take(500).map(k8s_line).collect();
                let more = if items.len() > 500 { format!("\n… and {} more (narrow it with namespace / label_selector)", items.len() - 500) } else { String::new() };
                ToolOutput::ok(format!("{} objects:\n{}{more}", items.len(), lines.join("\n")))
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

/// One listed object: namespace/name, its status in a few words, and when it was created.
fn k8s_line(o: &Value) -> String {
    let meta = &o["metadata"];
    let name = meta["name"].as_str().unwrap_or("?");
    let ns = meta["namespace"].as_str().map(|n| format!("{n}/")).unwrap_or_default();
    let st = &o["status"];
    let mut status = Vec::new();
    if let Some(p) = st["phase"].as_str() {
        status.push(p.to_string());
    }
    if let (Some(r), Some(d)) = (st["readyReplicas"].as_u64().or(Some(0)).filter(|_| st.get("replicas").is_some()), st["replicas"].as_u64()) {
        status.push(format!("{r}/{d} ready"));
    }
    if let Some(cs) = st["containerStatuses"].as_array() {
        let restarts: u64 = cs.iter().filter_map(|c| c["restartCount"].as_u64()).sum();
        let waiting: Vec<&str> = cs.iter().filter_map(|c| c["state"]["waiting"]["reason"].as_str()).collect();
        let ready = cs.iter().filter(|c| c["ready"].as_bool() == Some(true)).count();
        status.push(format!("{ready}/{} containers ready", cs.len()));
        if restarts > 0 {
            status.push(format!("{restarts} restarts"));
        }
        if !waiting.is_empty() {
            status.push(waiting.join(","));
        }
    }
    if let (Some(reason), Some(msg)) = (o["reason"].as_str(), o["message"].as_str()) {
        status.push(format!("{reason}: {msg}")); // events
    }
    let created = meta["creationTimestamp"].as_str().unwrap_or("");
    format!("{ns}{name}  {}  {created}", status.join(", ")).trim_end().to_string()
}

/// Secret data stays in the cluster: only the keys and sizes are shown.
fn redact_secret(v: &mut Value) {
    if v["kind"] != "Secret" {
        return;
    }
    for field in ["data", "stringData"] {
        if let Some(m) = v.get_mut(field).and_then(Value::as_object_mut) {
            for val in m.values_mut() {
                let len = val.as_str().map(str::len).unwrap_or(0);
                *val = Value::String(format!("<hidden, {len} characters>"));
            }
        }
    }
    if let Some(ann) = v.pointer_mut("/metadata/annotations").and_then(Value::as_object_mut) {
        ann.remove("kubectl.kubernetes.io/last-applied-configuration");
    }
}

struct K8sLogs(Arc<Scope>);

#[async_trait]
impl Tool for K8sLogs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "k8s_logs".into(),
            description: "Read the recent logs of a pod in one of the user's cluster connections (the last `tail_lines` lines, default 200). `previous: true` reads the container's previous run (after a crash).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Cluster connection name or id" },
                    "namespace": { "type": "string" },
                    "pod": { "type": "string" },
                    "container": { "type": "string", "description": "Needed when the pod has several containers" },
                    "tail_lines": { "type": "integer" },
                    "previous": { "type": "boolean" }
                },
                "required": ["connection", "namespace", "pod"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        format!("{}: {}/{}", conn_summary(input), s("namespace"), s("pod"))
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        use k8s_openapi::api::core::v1::Pod;
        use kube::api::{Api, LogParams};
        let c = match k8s_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let (Some(ns), Some(pod)) = (s("namespace"), s("pod")) else { return ToolOutput::err("Missing `namespace` or `pod`.") };
        let conn = match crate::infra::k8s::conn(&St::get(), &c.id).await {
            Ok(x) => x,
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        let lp = LogParams {
            container: s("container"),
            tail_lines: Some(input.get("tail_lines").and_then(Value::as_i64).unwrap_or(200).clamp(1, 5000)),
            previous: input.get("previous").and_then(Value::as_bool).unwrap_or(false),
            ..LogParams::default()
        };
        match Api::<Pod>::namespaced(conn.client.clone(), &ns).logs(&pod, &lp).await {
            Ok(text) if text.trim().is_empty() => ToolOutput::ok("(no log lines)"),
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_with_folders_keep_their_connections_to_them() {
        let conn = |id: &str, kind: &str, project: Option<&str>| -> ConnectionConfig {
            serde_json::from_value(json!({ "id": id, "name": id, "kind": kind, "projectId": project })).unwrap()
        };
        let projects = vec![
            Project { id: "shop".into(), name: "Shop".into(), folders: vec!["/src/shop".into()], ..Default::default() },
            Project { id: "misc".into(), name: "Misc".into(), ..Default::default() },
        ];
        let all = [conn("shop-db", "postgres", Some("shop")), conn("misc-db", "mysql", Some("misc")), conn("cache", "redis", None), conn("box", "ssh", None), conn("lost", "sqlite", Some("gone"))];
        let seen = |current: Option<&Project>| all.iter().filter(|c| visible(c, &projects, current)).map(|c| c.id.as_str()).collect::<Vec<_>>();
        assert_eq!(seen(Some(&projects[0])), ["shop-db", "misc-db", "cache", "lost"], "in the shop folder: its own and the shared ones");
        assert_eq!(seen(None), ["misc-db", "cache", "lost"], "elsewhere: a project without folders is shared; SSH never");
        assert_eq!(seen(Some(&projects[1])), ["misc-db", "cache", "lost"]);
        let scope = Scope::folder(Path::new("/src/shop/api"));
        assert_eq!(scope.current(&projects).map(|p| p.id), Some("shop".into()));
        assert_eq!(Scope::folder(Path::new("/src/shopping")).current(&projects).map(|p| p.id), None, "a folder, not a name prefix");
        assert_eq!(Scope { project: Some("misc".into()), ..Scope::default() }.current(&projects).map(|p| p.id), Some("misc".into()));
    }

    #[test]
    fn ai_access_is_the_connections_own_else_its_projects_and_read_only_caps_it() {
        let conn = |v: Value| -> ConnectionConfig { serde_json::from_value(v).unwrap() };
        let projects = vec![
            Project { id: "rw".into(), name: "Rw".into(), mcp_writes: true, ..Default::default() },
            Project { id: "ro".into(), name: "Ro".into(), ..Default::default() },
        ];
        let a = |v: Value| access(&conn(v), &projects);
        // saved before the setting: the project's "Allow writes from MCP"
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "projectId": "rw" })), McpAccess::Full);
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "projectId": "ro" })), McpAccess::Read);
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres" })), McpAccess::Read);
        // its own setting wins, either way
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "projectId": "rw", "mcpAccess": "read" })), McpAccess::Read);
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "projectId": "ro", "mcpAccess": "full" })), McpAccess::Full);
        // a read-only connection never gets full
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "readOnly": true, "mcpAccess": "full" })), McpAccess::Read);
        assert_eq!(a(json!({ "id": "1", "name": "a", "kind": "postgres", "readOnly": true, "projectId": "rw" })), McpAccess::Read);
        // off: no agent sees it
        let off = conn(json!({ "id": "1", "name": "a", "kind": "postgres", "mcpAccess": "off" }));
        assert!(!visible(&off, &projects, None));
        assert!(visible(&conn(json!({ "id": "1", "name": "a", "kind": "postgres", "mcpAccess": "read" })), &projects, None));
    }

    #[test]
    fn query_results_read_as_a_table() {
        let r = QueryResult {
            statement: "select".into(),
            columns: vec![crate::db::ColumnInfo { name: "id".into(), type_name: "int".into() }, crate::db::ColumnInfo { name: "note".into(), type_name: "text".into() }],
            rows: vec![vec![json!(1), json!("a\nb")], vec![json!(2), Value::Null]],
            rows_affected: None,
            elapsed_ms: 3,
            truncated: true,
            message: None,
        };
        assert_eq!(format_result(&r), "id | note\n1 | a\\nb\n2 | NULL\n(2 rows, more not shown: add a LIMIT / filter or raise max_rows, 3 ms)");
    }

    #[test]
    fn secrets_keep_their_keys_only() {
        let mut v = json!({ "kind": "Secret", "data": { "password": "c2VjcmV0" }, "metadata": { "name": "db" } });
        redact_secret(&mut v);
        assert_eq!(v["data"]["password"], "<hidden, 8 characters>");
        let mut cm = json!({ "kind": "ConfigMap", "data": { "a": "b" } });
        redact_secret(&mut cm);
        assert_eq!(cm["data"]["a"], "b");
    }

    #[test]
    fn dropping_quotes_the_name_and_runs_outside_the_database() {
        assert_eq!(drop_statement(ConnectionKind::Postgres, "a\"b"), ("DROP DATABASE \"a\"\"b\"".into(), Some("postgres")));
        assert_eq!(drop_statement(ConnectionKind::Mysql, "a`b"), ("DROP DATABASE `a``b`".into(), None));
        assert_eq!(drop_statement(ConnectionKind::Mssql, "a]b"), ("DROP DATABASE [a]]b]".into(), Some("master")));
        assert_eq!(drop_statement(ConnectionKind::Mongodb, "shop").0, "db.dropDatabase()");
        assert!(system_database(ConnectionKind::Postgres, "Template1"));
        assert!(system_database(ConnectionKind::Mongodb, "admin"));
        assert!(!system_database(ConnectionKind::Mysql, "shop"));
        assert!(!can_drop(ConnectionKind::Sqlite) && !can_drop(ConnectionKind::Redis));
        let input = json!({ "connection": "x", "database": "shop" });
        assert_eq!(DbDrop(Arc::new(Scope::default())).confirm(&input).as_deref(), Some("shop"), "the user types the database's name");
    }

    #[test]
    fn dumps_and_exports_go_to_downloads_unless_given_a_path() {
        let cwd = Path::new("/work/api");
        assert_eq!(out_path(&json!({ "path": "out/db.sql" }), cwd, "x.sql"), Path::new("/work/api/out/db.sql"));
        assert_eq!(out_path(&json!({ "path": "/tmp/db.sql" }), cwd, "x.sql"), Path::new("/tmp/db.sql"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(out_path(&json!({ "path": "~/db.sql" }), cwd, "x.sql"), home.join("db.sql"));
        }
        let default = out_path(&json!({}), cwd, "x.sql");
        assert!(default.ends_with("x.sql") && !default.starts_with(cwd));
        let ctx = ToolCtx::at(cwd);
        assert_eq!(file_kind(Path::new("/work/api/out/db.sql"), &ctx), Kind::Edit);
        assert_eq!(file_kind(&default, &ctx), Kind::EditOutside);
        assert_eq!(slug("public.orders 2026"), "public.orders-2026");
        assert_eq!(slug("../.."), "export");
    }

    #[test]
    fn listed_pods_show_their_state() {
        let pod = json!({ "metadata": { "name": "api-1", "namespace": "prod", "creationTimestamp": "2026-10-21T00:00:00Z" },
            "status": { "phase": "Running", "containerStatuses": [{ "ready": false, "restartCount": 4, "state": { "waiting": { "reason": "CrashLoopBackOff" } } }] } });
        assert_eq!(k8s_line(&pod), "prod/api-1  Running, 0/1 containers ready, 4 restarts, CrashLoopBackOff  2026-10-21T00:00:00Z");
    }
}
