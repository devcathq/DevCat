//! MCP client (Model Context Protocol): starts local (stdio) servers and connects to remote
//! (Streamable HTTP) ones configured for the project or the user, and offers their tools to the
//! model as `mcp__<server>__<tool>`. Calls go through the normal tool pipeline, so they ask for
//! approval like commands.
//!
//! Configs are read where other agents keep them: `.mcp.json` and `.cursor/mcp.json` in the
//! project, `~/.claude.json` (user and per-project servers) and `~/.cursor/mcp.json`.
use crate::approval::Kind;
use crate::message::ToolSpec;
use crate::tools::{Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::oneshot;

const PROTOCOL: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// One configured server: a local command (stdio) or a remote `url` (Streamable HTTP).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Remote server address; when set, `command` / `args` / `env` are unused.
    pub url: Option<String>,
    /// Headers for a remote server (e.g. an API key).
    pub headers: Vec<(String, String)>,
    /// Where it was configured (for messages).
    pub source: String,
}

/// Servers configured for `cwd`: the project's first, then the user's; one per name.
/// Old-style SSE-only servers come back as errors: not supported.
pub fn configured(cwd: &Path, home: Option<&Path>) -> (Vec<ServerConfig>, Vec<String>) {
    let mut found: Vec<(String, Value, String)> = Vec::new();
    let mut read = |path: PathBuf, label: &str, pick: &dyn Fn(&Value) -> Option<Map<String, Value>>| {
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        for (name, cfg) in pick(&v).unwrap_or_default() {
            found.push((name, cfg, label.to_string()));
        }
    };
    let servers = |v: &Value| v.get("mcpServers").and_then(Value::as_object).cloned();
    read(cwd.join(".mcp.json"), ".mcp.json", &servers);
    read(cwd.join(".cursor/mcp.json"), ".cursor/mcp.json", &servers);
    if let Some(home) = home {
        let key = cwd.to_string_lossy().replace('\\', "/");
        let cwd_raw = cwd.to_string_lossy().to_string();
        read(home.join(".claude.json"), "~/.claude.json (this project)", &|v: &Value| {
            let projects = v.get("projects")?.as_object()?;
            projects.iter().find(|(k, _)| k.replace('\\', "/") == key || **k == cwd_raw).and_then(|(_, p)| servers(p))
        });
        read(home.join(".claude.json"), "~/.claude.json", &servers);
        read(home.join(".cursor/mcp.json"), "~/.cursor/mcp.json", &servers);
    }
    let (mut out, mut errors) = (Vec::<ServerConfig>::new(), Vec::new());
    for (name, cfg, source) in found {
        if out.iter().any(|s| s.name == name) {
            continue;
        }
        if cfg.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let pairs = |key: &str| -> Vec<(String, String)> { cfg.get(key).and_then(Value::as_object).map(|e| e.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), expand(v)))).collect()).unwrap_or_default() };
        if let Some(url) = cfg.get("url").or_else(|| cfg.get("serverUrl")).and_then(Value::as_str) {
            if cfg.get("type").and_then(Value::as_str) == Some("sse") {
                errors.push(format!("MCP server \"{name}\" ({source}) uses the old SSE transport, which DevCat Agent doesn't support. Most servers also offer the newer HTTP address."));
                continue;
            }
            out.push(ServerConfig { name, url: Some(expand(url)), headers: pairs("headers"), source, ..Default::default() });
            continue;
        }
        let Some(command) = cfg.get("command").and_then(Value::as_str) else { continue };
        let args = cfg.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(expand).collect()).unwrap_or_default();
        out.push(ServerConfig { name, command: expand(command), args, env: pairs("env"), source, ..Default::default() });
    }
    (out, errors)
}

/// `${VAR}` / `${VAR:-default}` from the environment.
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let Some(end) = rest[i..].find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let inner = &rest[i + 2..i + end];
        let (var, default) = inner.split_once(":-").unwrap_or((inner, ""));
        out.push_str(&std::env::var(var).unwrap_or_else(|_| default.to_string()));
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

enum Transport {
    Stdio { stdin: tokio::sync::Mutex<ChildStdin>, pending: Pending, _child: Box<Child> },
    Http { http: reqwest::Client, url: String, headers: Vec<(String, String)>, session: Mutex<Option<String>> },
}

/// A connected server. Dropping a local one stops its process.
pub struct Server {
    pub name: String,
    transport: Transport,
    next: AtomicU64,
    pub tools: Vec<ToolSpec>,
}

impl Server {
    /// Start (or connect to) the server, do the MCP handshake and list its tools.
    pub async fn start(cfg: &ServerConfig) -> Result<Arc<Server>, String> {
        let transport = match &cfg.url {
            Some(url) => Transport::Http {
                http: reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?,
                url: url.clone(),
                headers: cfg.headers.clone(),
                session: Mutex::new(None),
            },
            None => Self::spawn(cfg)?,
        };
        let mut server = Server { name: cfg.name.clone(), transport, next: AtomicU64::new(1), tools: vec![] };
        let fail = |e: String| format!("MCP server \"{}\" ({}): {e}", cfg.name, cfg.source);
        server
            .request("initialize", json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "devcat", "version": env!("CARGO_PKG_VERSION") } }), START_TIMEOUT)
            .await
            .map_err(fail)?;
        server.notify("notifications/initialized").await.map_err(fail)?;
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map(|c| json!({ "cursor": c })).unwrap_or(json!({}));
            let r = server.request("tools/list", params, START_TIMEOUT).await.map_err(fail)?;
            for t in r.get("tools").and_then(Value::as_array).into_iter().flatten() {
                let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                let mut schema = t.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" }));
                if schema.get("type").is_none() {
                    schema["type"] = json!("object");
                }
                let description = t.get("description").and_then(Value::as_str).unwrap_or_default();
                server.tools.push(ToolSpec { name: name.to_string(), description: description.to_string(), input_schema: schema });
            }
            cursor = r.get("nextCursor").and_then(Value::as_str).map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(Arc::new(server))
    }

    /// A local server: its process, and a task routing its answers to the waiting requests.
    fn spawn(cfg: &ServerConfig) -> Result<Transport, String> {
        let mut cmd = if cfg!(windows) {
            // npx, uvx and friends are .cmd files on Windows: let cmd find them.
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&cfg.command).args(&cfg.args);
            c
        } else {
            let mut c = tokio::process::Command::new(&cfg.command);
            c.args(&cfg.args);
            c
        };
        cmd.envs(cfg.env.iter().cloned()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("MCP server \"{}\": couldn't start {}: {e}", cfg.name, cfg.command))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Pending = Arc::default();
        let p2 = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                // responses only; server notifications and requests are ignored
                let Some(id) = v.get("id").and_then(Value::as_u64).filter(|_| v.get("method").is_none()) else { continue };
                if let Some(tx) = p2.lock().unwrap().remove(&id) {
                    let r = match v.get("error") {
                        Some(e) => Err(e.get("message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| e.to_string())),
                        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(r);
                }
            }
            // the process ended: fail whatever is still waiting
            p2.lock().unwrap().clear();
        });
        Ok(Transport::Stdio { stdin: tokio::sync::Mutex::new(stdin), pending, _child: Box::new(child) })
    }

    async fn write_line(stdin: &tokio::sync::Mutex<ChildStdin>, msg: Value) -> Result<(), String> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = stdin.lock().await;
        w.write_all(line.as_bytes()).await.map_err(|e| format!("the server stopped ({e})"))?;
        w.flush().await.map_err(|e| e.to_string())
    }

    /// POST one JSON-RPC message to a remote server. The answer is JSON, or an event stream
    /// that carries it (Streamable HTTP).
    async fn post(&self, msg: &Value, timeout: Duration) -> Result<Option<Value>, String> {
        let Transport::Http { http, url, headers, session } = &self.transport else { unreachable!() };
        let mut req = http
            .post(url)
            .timeout(timeout)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .json(msg);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = session.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *session.lock().unwrap() = Some(sid.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let hint = if status.as_u16() == 401 || status.as_u16() == 403 { " (check the server's API key in its config)" } else { "" };
            return Err(format!("HTTP {}{hint}: {}", status.as_u16(), body.chars().take(200).collect::<String>()));
        }
        if msg.get("id").is_none() || status.as_u16() == 202 {
            return Ok(None);
        }
        let id = msg.get("id").cloned();
        let is_sse = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("text/event-stream"));
        if !is_sse {
            return resp.json::<Value>().await.map(Some).map_err(|e| format!("bad answer: {e}"));
        }
        let mut sse = crate::llm::sse::Sse::new(resp);
        while let Some(data) = sse.next().await.map_err(|e| e.to_string())? {
            if let Ok(v) = serde_json::from_str::<Value>(&data)
                && v.get("method").is_none()
                && v.get("id") == id.as_ref()
            {
                return Ok(Some(v));
            }
        }
        Err("the server closed the stream without answering".into())
    }

    async fn notify(&self, method: &str) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        match &self.transport {
            Transport::Stdio { stdin, .. } => Self::write_line(stdin, msg).await,
            Transport::Http { .. } => self.post(&msg, START_TIMEOUT).await.map(|_| ()),
        }
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let answer = |v: Value| match v.get("error") {
            Some(e) => Err(e.get("message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| e.to_string())),
            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        };
        match &self.transport {
            Transport::Http { .. } => match tokio::time::timeout(timeout, self.post(&msg, timeout)).await {
                Ok(Ok(Some(v))) => answer(v),
                Ok(Ok(None)) => Err(format!("no answer to {method}")),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(format!("no answer to {method} within {} s", timeout.as_secs())),
            },
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                Self::write_line(stdin, msg).await?;
                match tokio::time::timeout(timeout, rx).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => Err("the server stopped".into()),
                    Err(_) => {
                        pending.lock().unwrap().remove(&id);
                        Err(format!("no answer to {method} within {} s", timeout.as_secs()))
                    }
                }
            }
        }
    }

    /// Call one of its tools; the content comes back as text.
    pub async fn call(&self, tool: &str, args: Value) -> ToolOutput {
        match self.request("tools/call", json!({ "name": tool, "arguments": args }), CALL_TIMEOUT).await {
            Err(e) => ToolOutput::err(format!("MCP {}: {e}", self.name)),
            Ok(r) => {
                let text = r
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| match c.get("type").and_then(Value::as_str) {
                        Some("text") => c.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                        Some("resource") => c.pointer("/resource/text").and_then(Value::as_str).unwrap_or("[resource]").to_string(),
                        Some(other) => format!("[{other} content]"),
                        None => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = if text.is_empty() { r.get("structuredContent").map(Value::to_string).unwrap_or_else(|| "(no output)".into()) } else { text };
                ToolOutput { content: text, is_error: r.get("isError").and_then(Value::as_bool).unwrap_or(false) }
            }
        }
    }
}

/// Start every configured server (in parallel). Servers that fail are reported, not fatal.
pub async fn connect_all(cwd: &Path, home: Option<&Path>) -> (Vec<Arc<Server>>, Vec<String>) {
    let (configs, mut errors) = configured(cwd, home);
    let started = futures::future::join_all(configs.iter().map(Server::start)).await;
    let mut servers = Vec::new();
    for r in started {
        match r {
            Ok(s) => servers.push(s),
            Err(e) => errors.push(e),
        }
    }
    (servers, errors)
}

/// A tool of an MCP server, as the model sees it.
pub struct McpTool {
    server: Arc<Server>,
    tool: String,
    spec: ToolSpec,
}

impl McpTool {
    /// Every tool of `server`, named `mcp__<server>__<tool>`.
    pub fn all(server: &Arc<Server>) -> Vec<McpTool> {
        let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
        server
            .tools
            .iter()
            .map(|t| {
                let mut name = format!("mcp__{}__{}", clean(&server.name), clean(&t.name));
                name.truncate(64);
                McpTool { server: server.clone(), tool: t.name.clone(), spec: ToolSpec { name, description: format!("[MCP {}] {}", server.name, t.description), input_schema: t.input_schema.clone() } }
            })
            .collect()
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        let args = input.to_string();
        format!("{} {}", self.tool, if args.len() > 120 { format!("{}…", &args[..args.char_indices().nth(120).map(|(i, _)| i).unwrap_or(args.len())]) } else { args })
    }
    fn timeout(&self, _: &Value) -> Duration {
        CALL_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        self.server.call(&self.tool, input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_project_and_user_configs() {
        let proj = std::env::temp_dir().join(format!("devcat-mcp-{}", uuid::Uuid::new_v4()));
        let home = std::env::temp_dir().join(format!("devcat-mcp-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(proj.join(".mcp.json"), r#"{"mcpServers":{"db":{"command":"node","args":["db.js","${DEVCAT_TEST_UNSET:-x}"]},"web":{"type":"http","url":"https://x/mcp","headers":{"Authorization":"Bearer ${DEVCAT_TEST_UNSET:-k}"}},"old":{"type":"sse","url":"https://y/sse"}}}"#).unwrap();
        let key = proj.to_string_lossy().replace('\\', "\\\\");
        std::fs::write(home.join(".claude.json"), format!(r#"{{"mcpServers":{{"db":{{"command":"ignored"}},"fs":{{"command":"npx","args":["-y","fs"],"env":{{"A":"1"}}}}}},"projects":{{"{key}":{{"mcpServers":{{"proj":{{"command":"uvx","args":["p"]}}}}}}}}}}"#)).unwrap();
        let (s, errors) = configured(&proj, Some(&home));
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["db", "web", "proj", "fs"]);
        assert_eq!(s[0].args, vec!["db.js", "x"]);
        assert_eq!((s[1].url.as_deref(), s[1].headers.clone()), (Some("https://x/mcp"), vec![("Authorization".to_string(), "Bearer k".to_string())]));
        assert_eq!(s[3].env, vec![("A".to_string(), "1".to_string())]);
        assert!(errors[0].contains("\"old\"") && errors[0].contains("SSE"));
        let _ = (std::fs::remove_dir_all(proj), std::fs::remove_dir_all(home));
    }
}
