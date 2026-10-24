//! `devcat mcp`: the user's DevCat connections as an MCP server (stdio), for Claude Code, Codex,
//! Cursor, Gemini or any other agent. It serves the DevCat Agent's connection tools
//! (`agent_tools.rs`) from the app's own data folder and keychain, like `devcat serve`, so it works
//! with the window closed. Sync MCP (`mcp_sync.rs`) writes it into the agents' configs.
//!
//! Which connections it offers follows the folder (`--project auto`, the default): the project
//! whose code folder the agent works in, plus the connections every folder gets. `--project <id>`
//! picks one project, `--project none` only the shared ones. Connections and projects are read
//! on every call, so changes in the app apply without syncing again. Over MCP a query that may
//! change data runs only when the connection's project allows it (`mcp_writes`).
use crate::agent_tools::{self, Scope};
use devcat_harness::approval::Kind;
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Protocol versions this server speaks (the newest first); a client asking for another gets the
/// newest.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const HELP: &str = "\
devcat mcp: your DevCat connections as an MCP server (stdio)

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
    match project.as_deref().map(str::trim) {
        None | Some("" | "auto") => scope.folder = folder.or_else(agent_folder),
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
    crate::network::init(dir.clone());
    crate::network::set_passive();
    crate::state::init(dir);
    crate::rt::block_on(serve(scope));
    0
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
async fn serve(scope: Scope) {
    let scope = Arc::new(scope);
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
            let (scope, tx) = (scope.clone(), tx.clone());
            tokio::spawn(async move {
                if let Some(reply) = handle(&scope, m).await {
                    let _ = tx.send(reply);
                }
            });
        }
    }
    drop(tx);
    let _ = writer.await;
}

/// The answer to one message (notifications and responses get none).
async fn handle(scope: &Scope, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str)?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = PROTOCOLS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOLS[0]);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "devcat", "title": "DevCat connections", "version": env!("DEVCAT_VERSION") },
                    "instructions": instructions(scope),
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": agent_tools::for_scope(scope).iter().map(|t| tool_json(t.as_ref())).collect::<Vec<_>>() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let input = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            let out = match agent_tools::for_scope(scope).into_iter().find(|t| t.spec().name == name) {
                Some(t) => call(t, input).await,
                None => ToolOutput::err(format!("No DevCat tool called \"{name}\" here (no connection of that kind for this folder?).")),
            };
            result(id, json!({ "content": [{ "type": "text", "text": out.content }], "isError": out.is_error }))
        }
        "resources/list" => result(id, json!({ "resources": [] })),
        "prompts/list" => result(id, json!({ "prompts": [] })),
        _ => error(id, -32601, &format!("Method not found: {method}")),
    })
}

async fn call(t: Arc<dyn Tool>, input: Value) -> ToolOutput {
    let ctx = ToolCtx { cwd: std::env::current_dir().unwrap_or_default() };
    match tokio::time::timeout(t.timeout(&input), t.run(input, &ctx)).await {
        Ok(out) => ToolOutput { content: devcat_harness::tools::truncate(&out.content), ..out },
        Err(_) => ToolOutput::err(format!("Timed out after {} s.", t.timeout(&Value::Null).as_secs())),
    }
}

/// A tool as MCP lists it, with hints for the client's approval prompts.
fn tool_json(t: &dyn Tool) -> Value {
    let spec = t.spec();
    let ctx = ToolCtx { cwd: PathBuf::new() };
    // `db_query` with no query: whether it may change data at all
    let writes = t.kind(&json!({}), &ctx) == Kind::Exec;
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.input_schema,
        "annotations": { "readOnlyHint": !writes, "destructiveHint": writes, "openWorldHint": false },
    })
}

/// Told to the agent when it connects: what DevCat offers here, so it uses these tools when it
/// needs data instead of asking for credentials.
fn instructions(scope: &Scope) -> String {
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
        s.push_str(&v.connections.iter().map(agent_tools::describe).collect::<Vec<_>>().join("\n"));
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
        let scope = Scope { mcp: true, ..Scope::default() };
        let init = handle(&scope, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "devcat");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("db_query"));
        let newer = handle(&scope, json!({ "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } })).await.unwrap();
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOLS[0]);
        assert!(handle(&scope, json!({ "method": "notifications/initialized" })).await.is_none());
        assert_eq!(handle(&scope, json!({ "id": 3, "method": "nope" })).await.unwrap()["error"]["code"], -32601);
        let call = handle(&scope, json!({ "id": 4, "method": "tools/call", "params": { "name": "db_query", "arguments": {} } })).await.unwrap();
        assert_eq!(call["result"]["isError"], true);
    }
}
