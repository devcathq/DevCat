//! `devcat mcp`: the user's DevCat connections and shared memory as an MCP server (stdio), for
//! Claude Code, Codex, Cursor, Gemini or any other agent. It serves the DevCat Agent's connection
//! tools (`agent_tools.rs`) and memory tools (`memory/tools.rs`) from the app's own data folder and
//! keychain, like `devcat serve`, so it works with the window closed. Sync MCP (`mcp_sync.rs`)
//! writes it into the agents' configs. The agent learns about the memory when it connects (the
//! `initialize` instructions carry a brief of it), so switching agents keeps what the last one
//! learned.
//!
//! Which connections it offers follows the folder (`--project auto`, the default): the project
//! whose code folder the agent works in, plus the connections every folder gets. `--project <id>`
//! picks one project, `--project none` only the shared ones. Connections and projects are read
//! on every call, so changes in the app apply without syncing again. Each connection's AI access
//! (`McpAccess`) decides: off hides it, read-only refuses queries that may change data, full runs
//! them (the driver still refuses writes on a read-only connection). Every tool call goes into the
//! MCP activity log (`mcp_audit.rs`).
use crate::agent_tools::{self, Scope};
use devcat_harness::approval::Kind;
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Protocol versions this server speaks (the newest first); a client asking for another gets the
/// newest.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const HELP: &str = "\
devcat mcp: your DevCat connections and shared memory as an MCP server (stdio)

  --project auto|none|<id>  Whose connections: the project whose code folder the agent works in
                            (auto, the default), only the ones every folder gets (none), or one
                            project (its id, see projects.json)
  --folder <path>           The folder for `auto` (default: the agent's folder, else this one)
  --data-dir <path>         DevCat's data folder (default: the app's)

Use Sync MCP in DevCat (Connections → the plug button) to add it to Claude Code, Codex, Cursor
and Gemini.
";

pub fn main(args: &[String]) -> i32 {
    let mut scope = Scope { mcp: true, ..Scope::default() };
    let (mut project, mut folder, mut data_dir) = (None::<String>, None::<PathBuf>, None::<PathBuf>);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        let r = match a.as_str() {
            "--project" => value("--project").map(|v| project = Some(v)),
            "--folder" => value("--folder").map(|v| folder = given(&v).map(PathBuf::from)),
            "--data-dir" => value("--data-dir").map(|v| data_dir = given(&v).map(PathBuf::from)),
            "-h" | "--help" | "help" => {
                eprint!("{HELP}");
                return 0;
            }
            other => Err(format!("Unknown option {other}")),
        };
        if let Err(e) = r {
            eprintln!("{e}\n\n{HELP}");
            return 2;
        }
    }
    // the folder the agent works in (also with a fixed project: Claude's own memory for it)
    let folder = folder.or_else(agent_folder);
    match project.as_deref().map(str::trim) {
        None | Some("" | "auto") => scope.folder = folder.clone(),
        Some("none") => {}
        Some(id) => scope.project = Some(id.to_string()),
    }
    let Some(dir) = data_dir.or_else(crate::cli::default_data_dir) else {
        eprintln!("devcat mcp: can't find DevCat's data folder: pass --data-dir");
        return 1;
    };
    if std::env::var_os("DEVCAT_TEST_MEMORY_SECRETS").is_some() {
        // tests: a fresh build reading the macOS keychain would wait for the user to allow it
        crate::secrets::memory_only();
    }
    crate::init_process();
    crate::secrets::follow_data_dir(&dir);
    crate::network::init(dir.clone());
    crate::network::set_passive();
    crate::memory::init(dir.clone());
    crate::mcp_audit::init(dir.clone());
    crate::state::init(dir);
    // Claude's newest memories reach this session, even with DevCat's window closed
    crate::memory::claude::auto();
    crate::rt::block_on(serve(Server::new(scope, folder)));
    0
}

/// One agent's connection.
pub struct Server {
    scope: Scope,
    /// The folder the agent works in.
    folder: Option<PathBuf>,
    /// The agent, from its `initialize` (memories record who wrote them).
    client: Mutex<String>,
}

impl Server {
    pub fn new(scope: Scope, folder: Option<PathBuf>) -> Server {
        Server { scope, folder, client: Mutex::new("an agent".into()) }
    }

    /// Memory tools, then the connection tools for this folder.
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = crate::memory::tools::for_scope(&self.scope, &self.client.lock());
        tools.extend(agent_tools::for_scope(&self.scope));
        tools
    }
}

/// The agent's name as memories record it.
fn client_name(info: &Value) -> String {
    let name = info.get("name").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    let known = [("claude", "claude-code"), ("codex", "codex"), ("gemini", "gemini"), ("cursor", "cursor"), ("opencode", "opencode"), ("copilot", "copilot"), ("devcat", "devcat-agent")];
    match known.iter().find(|(k, _)| name.contains(k)) {
        Some((_, n)) => n.to_string(),
        None if name.is_empty() => "an agent".into(),
        None => name,
    }
}

/// A value from a config, unless it's a variable the agent didn't fill in (`${workspaceFolder}`).
fn given(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty() && !v.contains("${")).then(|| v.to_string())
}

/// The folder the agent runs in: what it tells MCP servers, else the working directory.
fn agent_folder() -> Option<PathBuf> {
    ["CLAUDE_PROJECT_DIR", "WORKSPACE_FOLDER_PATHS", "GEMINI_PROJECT_DIR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .filter_map(|v| given(v.split([',', ';']).next().unwrap_or("")))
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
}

/// Reads JSON-RPC lines from stdin until it closes; each request is answered on stdout (tool
/// calls run side by side).
async fn serve(server: Server) {
    let server = Arc::new(server);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(msg) = rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(error(Value::Null, -32700, &format!("Parse error: {e}")));
                continue;
            }
        };
        // batches (older protocol versions) are answered one by one
        for m in msg.as_array().cloned().unwrap_or_else(|| vec![msg]) {
            let (server, tx) = (server.clone(), tx.clone());
            tokio::spawn(async move {
                if let Some(reply) = handle(&server, m).await {
                    let _ = tx.send(reply);
                }
            });
        }
    }
    drop(tx);
    let _ = writer.await;
}

/// The answer to one message (notifications and responses get none).
async fn handle(server: &Server, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str)?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = PROTOCOLS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOLS[0]);
            *server.client.lock() = client_name(params.get("clientInfo").unwrap_or(&Value::Null));
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false } },
                    "serverInfo": { "name": "devcat", "title": "DevCat connections and memory", "version": env!("DEVCAT_VERSION") },
                    "instructions": instructions(server),
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": server.tools().iter().map(|t| tool_json(t.as_ref())).collect::<Vec<_>>() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let input = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            let out = logged(server, name, name, input).await;
            result(id, json!({ "content": [{ "type": "text", "text": out.content }], "isError": out.is_error }))
        }
        "resources/list" => result(id, json!({ "resources": resources(server) })),
        "resources/templates/list" => result(id, json!({ "resourceTemplates": [] })),
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            let Some((tool, input)) = resource_call(server, uri) else {
                // -32002: resource not found
                return Some(error(id, -32002, &format!("No DevCat resource {uri} here (see resources/list).")));
            };
            let out = logged(server, &format!("read {uri}"), tool, input).await;
            if out.is_error {
                return Some(error(id, -32603, &out.content));
            }
            result(id, json!({ "contents": [{ "uri": uri, "mimeType": "text/plain", "text": out.content }] }))
        }
        "prompts/list" => result(id, json!({ "prompts": [] })),
        _ => error(id, -32601, &format!("Method not found: {method}")),
    })
}

/// Run the tool `name` (if this folder has it) and log the call in the MCP activity log as `what`.
async fn logged(server: &Server, what: &str, name: &str, input: Value) -> ToolOutput {
    let (started, at) = (std::time::Instant::now(), crate::mcp_audit::now_ms());
    let tool = server.tools().into_iter().find(|t| t.spec().name == name);
    let detail = tool.as_ref().and_then(|t| crate::mcp_audit::short(&t.summary(&input)));
    let out = match tool {
        Some(t) => call(t, input.clone()).await,
        None => ToolOutput::err(format!("No DevCat tool called \"{name}\" here (no connection of that kind for this folder?).")),
    };
    crate::mcp_audit::log(&crate::mcp_audit::Entry {
        at,
        client: server.client.lock().clone(),
        tool: what.to_string(),
        connection: input.get("connection").and_then(Value::as_str).and_then(crate::mcp_audit::short),
        detail,
        project: agent_tools::view(&server.scope).project.map(|p| p.name),
        ok: !out.is_error,
        error: out.is_error.then(|| crate::mcp_audit::short(out.content.lines().next().unwrap_or(""))).flatten(),
        ms: started.elapsed().as_millis() as u64,
    });
    out
}

/// Context the agent can attach without a tool call: the connections this folder sees, and each
/// database's schema (read when the agent opens it).
fn resources(server: &Server) -> Vec<Value> {
    let v = agent_tools::view(&server.scope);
    if v.connections.is_empty() {
        return vec![];
    }
    let p = v.project.as_ref().map(|p| p.id.as_str()).unwrap_or("none");
    let mut out = vec![json!({
        "uri": format!("devcat://project/{p}/connections"),
        "name": "connections",
        "title": match &v.project { Some(p) => format!("DevCat connections · {}", p.name), None => "DevCat connections".into() },
        "description": "The DevCat connections agents get in this folder: name, kind, database and access",
        "mimeType": "text/plain",
    })];
    for c in v.connections.iter().filter(|c| agent_tools::has_schema(c.kind)) {
        out.push(json!({
            "uri": format!("devcat://conn/{}/schema", c.id),
            "name": format!("{}-schema", c.name),
            "title": format!("{} schema", c.name),
            "description": format!("Tables and columns of {} (connects to it when read)", c.name),
            "mimeType": "text/plain",
        }));
    }
    out
}

/// The tool and input that read the resource `uri`, if this folder has it.
fn resource_call(server: &Server, uri: &str) -> Option<(&'static str, Value)> {
    let rest = uri.strip_prefix("devcat://")?;
    let v = agent_tools::view(&server.scope);
    if let Some(p) = rest.strip_prefix("project/").and_then(|r| r.strip_suffix("/connections")) {
        let here = v.project.as_ref().map(|p| p.id.as_str()).unwrap_or("none");
        return (p == here && !v.connections.is_empty()).then(|| ("devcat_connections", json!({})));
    }
    let id = rest.strip_prefix("conn/")?.strip_suffix("/schema")?;
    v.connections.iter().any(|c| c.id == id && agent_tools::has_schema(c.kind)).then(|| ("db_schema", json!({ "connection": id })))
}

async fn call(t: Arc<dyn Tool>, input: Value) -> ToolOutput {
    let ctx = ToolCtx::at(std::env::current_dir().unwrap_or_default());
    match tokio::time::timeout(t.timeout(&input), t.run(input, &ctx)).await {
        Ok(out) => ToolOutput { content: devcat_harness::tools::truncate(&out.content), ..out },
        Err(_) => ToolOutput::err(format!("Timed out after {} s.", t.timeout(&Value::Null).as_secs())),
    }
}

/// A tool as MCP lists it, with hints for the client's approval prompts.
fn tool_json(t: &dyn Tool) -> Value {
    let spec = t.spec();
    let ctx = ToolCtx::at(PathBuf::new());
    // `db_query` with no query: whether it may change data at all
    let kind = t.kind(&json!({}), &ctx);
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.input_schema,
        "annotations": { "readOnlyHint": matches!(kind, Kind::Read | Kind::Query), "destructiveHint": kind == Kind::Exec, "openWorldHint": false },
    })
}

/// Told to the agent when it connects: the shared memory's brief, and the connections DevCat
/// offers here, so it uses these tools when it needs data instead of asking for credentials.
fn instructions(server: &Server) -> String {
    // Claude Code loads its own memory for the folder: its copies here aren't repeated
    let claude_own = (*server.client.lock() == "claude-code").then(|| server.folder.as_deref().and_then(crate::memory::claude::memory_dir_for)).flatten();
    let brief = crate::memory::brief(crate::memory::place_for(&server.scope).as_ref(), claude_own.as_deref());
    format!("{brief}\n# Connections (DevCat)\n{}", connections(&server.scope))
}

fn connections(scope: &Scope) -> String {
    let v = agent_tools::view(scope);
    let mut s = String::from(
        "DevCat holds the user's database, Redis, MongoDB and Kubernetes connections (with their passwords, VPNs and read-only settings). \
         Whenever a task needs data, a schema or cluster state from one of them, use these tools instead of asking for credentials, \
         reading .env files or running psql / mysql / redis-cli / kubectl yourself: `devcat_connections` lists them, `db_schema` reads \
         tables and columns, `db_query` runs SQL / MongoDB shell / Redis commands, `k8s_get` and `k8s_logs` read clusters. \
         Prefer reads (SELECT with a LIMIT). Changing data works only where the user allowed it.",
    );
    if let Some(p) = &v.project {
        s.push_str(&format!("\n\nThis folder belongs to the DevCat project \"{}\".", p.name));
    }
    if v.connections.is_empty() {
        s.push_str("\n\nThere are no DevCat connections for this folder yet.");
    } else {
        s.push_str("\n\nConnections:\n");
        s.push_str(&v.connections.iter().map(|c| v.line(c, true)).collect::<Vec<_>>().join("\n"));
    }
    s
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfilled_variables_are_no_value() {
        assert_eq!(given("${workspaceFolder}"), None);
        assert_eq!(given("  "), None);
        assert_eq!(given(" /src/shop "), Some("/src/shop".into()));
    }

    #[tokio::test]
    async fn answers_the_handshake_and_unknown_methods() {
        crate::memory::tests::data();
        let server = Server::new(Scope { mcp: true, ..Scope::default() }, None);
        let init = handle(&server, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26", "clientInfo": { "name": "codex-mcp-client" } } })).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "devcat");
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("db_query") && instructions.contains("memory_search"), "{instructions}");
        assert_eq!(*server.client.lock(), "codex");
        let newer = handle(&server, json!({ "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } })).await.unwrap();
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOLS[0]);
        assert!(handle(&server, json!({ "method": "notifications/initialized" })).await.is_none());
        assert_eq!(handle(&server, json!({ "id": 3, "method": "nope" })).await.unwrap()["error"]["code"], -32601);
        let call = handle(&server, json!({ "id": 4, "method": "tools/call", "params": { "name": "db_query", "arguments": {} } })).await.unwrap();
        assert_eq!(call["result"]["isError"], true);
        let tools = handle(&server, json!({ "id": 5, "method": "tools/list" })).await.unwrap();
        let save = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "memory_save").unwrap().clone();
        assert_eq!(save["annotations"]["readOnlyHint"], false, "saving isn't read-only");
        assert_eq!(save["annotations"]["destructiveHint"], false);
    }

    #[tokio::test]
    async fn each_connections_ai_access_is_enforced_and_every_call_logged() {
        crate::secrets::memory_only();
        crate::memory::tests::data();
        crate::mcp_audit::tests::dir();
        let data = std::env::temp_dir().join(format!("devcat-mcp-access-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        let db = |id: &str, extra: Value| {
            let mut c = json!({ "id": id, "name": id, "kind": "sqlite", "filePath": data.join(format!("{id}.db")), "options": { "create": "true" } });
            c.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            c
        };
        let conns = json!([
            db("access-ro", json!({ "mcpAccess": "read" })),
            db("access-rw", json!({ "mcpAccess": "full" })),
            db("access-off", json!({ "mcpAccess": "off" })),
            db("access-locked", json!({ "mcpAccess": "full", "readOnly": true })),
        ]);
        std::fs::write(data.join("connections.json"), conns.to_string()).unwrap();
        crate::state::init(data);
        let server = Server::new(Scope { mcp: true, ..Scope::default() }, None);
        handle(&server, json!({ "id": 0, "method": "initialize", "params": { "clientInfo": { "name": "claude-code" } } })).await.unwrap();
        let call = |name: &str, args: Value| handle(&server, json!({ "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }));
        let text = |v: &Value| v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();

        let refused = call("db_query", json!({ "connection": "access-ro", "query": "CREATE TABLE t (x int)" })).await.unwrap();
        assert_eq!(refused["result"]["isError"], true);
        assert!(text(&refused).contains("only lets agents read"), "{refused}");
        let written = call("db_query", json!({ "connection": "access-rw", "query": "CREATE TABLE t (x int); INSERT INTO t VALUES (7)" })).await.unwrap();
        assert_eq!(written["result"]["isError"], false, "{written}");
        let read = call("db_query", json!({ "connection": "access-rw", "query": "SELECT x FROM t" })).await.unwrap();
        assert!(text(&read).contains("7"), "{read}");
        let hidden = call("db_query", json!({ "connection": "access-off", "query": "SELECT 1" })).await.unwrap();
        assert!(text(&hidden).contains("No DevCat connection is called"), "{hidden}");
        let locked = call("db_query", json!({ "connection": "access-locked", "query": "CREATE TABLE t (x int)" })).await.unwrap();
        assert_eq!(locked["result"]["isError"], true, "a read-only connection stays read-only: {locked}");

        let listed = text(&call("devcat_connections", json!({})).await.unwrap());
        assert!(listed.contains("access-rw (sqlite) id access-rw (agents may change data)") && listed.contains("access-ro (sqlite) id access-ro (agents read only)"), "{listed}");
        assert!(!listed.contains("access-off"), "{listed}");

        // resources: the connections, and each database's schema
        let res = handle(&server, json!({ "id": 2, "method": "resources/list" })).await.unwrap();
        let uris: Vec<&str> = res["result"]["resources"].as_array().unwrap().iter().filter_map(|r| r["uri"].as_str()).collect();
        assert!(uris.contains(&"devcat://project/none/connections") && uris.contains(&"devcat://conn/access-rw/schema"), "{uris:?}");
        assert!(!uris.iter().any(|u| u.contains("access-off")));
        let schema = handle(&server, json!({ "id": 3, "method": "resources/read", "params": { "uri": "devcat://conn/access-rw/schema" } })).await.unwrap();
        assert!(schema["result"]["contents"][0]["text"].as_str().unwrap().contains("t(x"), "{schema}");
        let missing = handle(&server, json!({ "id": 4, "method": "resources/read", "params": { "uri": "devcat://conn/access-off/schema" } })).await.unwrap();
        assert_eq!(missing["error"]["code"], -32002);

        let log: Vec<_> = crate::mcp_audit::tail(2000).into_iter().filter(|e| e.connection.as_deref().is_some_and(|c| c.starts_with("access-"))).collect();
        let ro = log.iter().find(|e| e.connection.as_deref() == Some("access-ro")).unwrap();
        assert_eq!((ro.client.as_str(), ro.tool.as_str(), ro.ok), ("claude-code", "db_query", false));
        assert!(ro.detail.as_deref().unwrap().contains("CREATE TABLE t") && ro.error.as_deref().unwrap().contains("only lets agents read"), "{ro:?}");
        assert!(log.iter().any(|e| e.tool == "read devcat://conn/access-rw/schema" && e.ok));
    }

    #[test]
    fn clients_are_named_like_the_memories_say() {
        assert_eq!(client_name(&json!({ "name": "claude-code" })), "claude-code");
        assert_eq!(client_name(&json!({ "name": "codex-mcp-client" })), "codex");
        assert_eq!(client_name(&json!({ "name": "gemini-cli-mcp-client" })), "gemini");
        assert_eq!(client_name(&json!({ "name": "Zed" })), "zed");
        assert_eq!(client_name(&Value::Null), "an agent");
    }
}
