//! DevCat Agent tools over the user's DevCat connections: list them, read a database schema, run
//! a query (SQL, MongoDB shell, Redis commands) and read Kubernetes objects and pod logs. They use
//! the same drivers as the app, so a connection marked read-only stays read-only for the agent.
use crate::db::{ExecOptions, QueryResult, TableRef};
use crate::models::{ConnectionConfig, ConnectionKind};
use crate::state::St;
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Registry, Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::time::Duration;

/// Rows a query returns unless the model asks for more; and the most it may ask for.
const DEFAULT_ROWS: usize = 100;
const MAX_ROWS: usize = 1000;
/// Longest cell shown; longer values keep their start.
const MAX_CELL: usize = 300;

/// The connection tools, when the user has any connections.
pub fn add(tools: &mut Registry) {
    let conns = connections();
    if conns.is_empty() {
        return;
    }
    tools.add(Connections);
    if conns.iter().any(|c| is_db(c.kind)) {
        tools.add(DbSchema);
        tools.add(DbQuery);
    }
    if conns.iter().any(|c| c.kind == ConnectionKind::Kubernetes) {
        tools.add(K8sGet);
        tools.add(K8sLogs);
    }
}

/// Names of the user's connections (for the chat's "Using" line).
pub fn names() -> Vec<String> {
    connections().into_iter().map(|c| c.name).collect()
}

fn connections() -> Vec<ConnectionConfig> {
    St::try_get().and_then(|s| s.store.load_all().ok()).unwrap_or_default()
}

fn is_db(k: ConnectionKind) -> bool {
    !matches!(k, ConnectionKind::Kubernetes | ConnectionKind::Rabbitmq)
}

fn kind_name(k: ConnectionKind) -> &'static str {
    match k {
        ConnectionKind::Postgres => "postgres",
        ConnectionKind::Mysql => "mysql",
        ConnectionKind::Sqlite => "sqlite",
        ConnectionKind::Mssql => "mssql",
        ConnectionKind::Mongodb => "mongodb",
        ConnectionKind::Redis => "redis",
        ConnectionKind::Kubernetes => "kubernetes",
        ConnectionKind::Rabbitmq => "rabbitmq",
    }
}

/// The connection the model means: by id, else by name (any case).
fn find(input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    let Some(want) = input.get("connection").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(ToolOutput::err("Missing `connection`: the name or id of a DevCat connection (see `devcat_connections`)."));
    };
    let all = connections();
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

struct Connections;

#[async_trait]
impl Tool for Connections {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "devcat_connections".into(),
            description: "List the user's DevCat connections (databases, Redis, MongoDB, Kubernetes clusters): name, kind, default database, and whether it's read-only. Use the name with `db_schema`, `db_query`, `k8s_get` and `k8s_logs`.".into(),
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
        let all = connections();
        if all.is_empty() {
            return ToolOutput::ok("The user has no DevCat connections.");
        }
        let lines: Vec<String> = all
            .iter()
            .map(|c| {
                let mut l = format!("- {} ({}", c.name, kind_name(c.kind));
                if let Some(db) = c.database.as_deref().filter(|d| !d.is_empty()) {
                    l.push_str(&format!(", database {db}"));
                }
                if c.read_only {
                    l.push_str(", read-only");
                }
                l.push_str(&format!(") id {}", c.id));
                l
            })
            .collect();
        ToolOutput::ok(lines.join("\n"))
    }
}

// ---------- db_schema

struct DbSchema;

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
        let c = match find(&input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if !is_db(c.kind) || c.kind == ConnectionKind::Redis {
            return ToolOutput::err(format!("{} is a {} connection; `db_schema` reads SQL databases and MongoDB.", c.name, kind_name(c.kind)));
        }
        let arg = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let (database, schema) = (arg("database").or(c.database.clone().filter(|d| !d.is_empty())), arg("schema"));
        let driver = match St::get().driver(&c.id).await {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(format!("Couldn't connect to {}: {e}", c.name)),
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
        match driver.schema_snapshot(database.as_deref(), schema.as_deref()).await {
            Ok(s) => {
                let mut out = Vec::new();
                for t in &s.tables {
                    let name = t.table.as_ref().map(|r| qualified(r)).unwrap_or_default();
                    let cols: Vec<String> = t.columns.iter().map(|c| format!("{} {}{}", c.name, c.data_type, if c.is_primary_key { " PK" } else { "" })).collect();
                    out.push(format!("{name}({})", cols.join(", ")));
                }
                for (v, _) in &s.views {
                    out.push(format!("view {}", qualified(v)));
                }
                if out.is_empty() {
                    return ToolOutput::ok("No tables found (check `database` / `schema`).");
                }
                ToolOutput::ok(format!("{} tables, {} views:\n{}", s.tables.len(), s.views.len(), out.join("\n")))
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

fn qualified(r: &TableRef) -> String {
    match &r.schema {
        Some(s) if !s.is_empty() => format!("{s}.{}", r.name),
        _ => r.name.clone(),
    }
}

// ---------- db_query

struct DbQuery;

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
        match find(input) {
            // the driver refuses writes on a read-only connection; reads are a query
            Ok(c) if c.read_only || crate::remote::readonly::allowed(Some(c.kind), query) => Kind::Query,
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
        let c = match find(&input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        if !is_db(c.kind) {
            return ToolOutput::err(format!("{} is a {} connection; use `k8s_get` / `k8s_logs` for clusters.", c.name, kind_name(c.kind)));
        }
        let query = input.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if query.is_empty() {
            return ToolOutput::err("Missing `query`.");
        }
        let max_rows = input.get("max_rows").and_then(Value::as_u64).map(|n| (n as usize).clamp(1, MAX_ROWS)).unwrap_or(DEFAULT_ROWS);
        let database = input.get("database").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let driver = match St::get().driver(&c.id).await {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(format!("Couldn't connect to {}: {e}", c.name)),
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

// ---------- Kubernetes

fn k8s_conn(input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    let c = find(input)?;
    if c.kind != ConnectionKind::Kubernetes {
        return Err(ToolOutput::err(format!("{} is a {} connection, not a Kubernetes cluster.", c.name, kind_name(c.kind))));
    }
    Ok(c)
}

struct K8sGet;

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
        let c = match k8s_conn(&input) {
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

struct K8sLogs;

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
        let c = match k8s_conn(&input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let (Some(ns), Some(pod)) = (s("namespace"), s("pod")) else { return ToolOutput::err("Missing `namespace` or `pod`.") };
        let conn = match crate::infra::k8s::conn(&St::get(), &c.id).await {
            Ok(x) => x,
            Err(e) => return ToolOutput::err(format!("Couldn't connect to {}: {e}", c.name)),
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
    fn listed_pods_show_their_state() {
        let pod = json!({ "metadata": { "name": "api-1", "namespace": "prod", "creationTimestamp": "2026-10-21T00:00:00Z" },
            "status": { "phase": "Running", "containerStatuses": [{ "ready": false, "restartCount": 4, "state": { "waiting": { "reason": "CrashLoopBackOff" } } }] } });
        assert_eq!(k8s_line(&pod), "prod/api-1  Running, 0/1 containers ready, 4 restarts, CrashLoopBackOff  2026-10-21T00:00:00Z");
    }
}
