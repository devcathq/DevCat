//! The local channel between `devcat mcp` and the DevCat that's open (the app, or `devcat serve`).
//!
//! `devcat mcp` is a process each agent (Claude Code, Codex, ...) starts for itself, but VPNs,
//! tunnels and port forwards, scheduled jobs and ticket runs live in the open DevCat. So while
//! DevCat is open, `devcat mcp` hands each tool call to it and DevCat runs it: the same tools
//! (`mcp_server::tools_for`), the same scope and AI access (`Scope::mcp` set), with networks
//! connected where they belong. With DevCat closed, `devcat mcp` runs the call itself, as before
//! (it never starts networks), and tools that need DevCat say so.
//!
//! DevCat listens on 127.0.0.1 (a random port) and writes `<data>/agent-bridge.json` with the
//! port and a random token, readable only by the user. A call is one JSON line carrying the
//! token, answered by one JSON line. Only the user's own processes can read the token, and they
//! can read the data folder and keychain `devcat mcp` uses anyway, so the channel gives nothing
//! they don't have; a web page can't read the file, so it can't call it. `devcat mcp` still logs
//! every call in the MCP activity log, wherever it ran.
use crate::agent_tools::Scope;
use devcat_harness::tools::{ToolCtx, ToolOutput};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const FILE: &str = "agent-bridge.json";
/// Calls and answers change shape: a `devcat mcp` and a DevCat of different versions don't
/// talk (the call runs in `devcat mcp` instead).
const VERSION: u32 = 1;
/// Longest call or answer line (a tool's input; answers are truncated before this).
const MAX_LINE: u64 = 16 * 1024 * 1024;
/// How long reading a call may take once connected.
const READ_CALL: Duration = Duration::from_secs(10);
/// How often DevCat writes the file again if it's gone (another DevCat that took over quit).
const REWRITE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Info {
    v: u32,
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Call {
    v: u32,
    token: String,
    tool: String,
    input: Value,
    project: Option<String>,
    folder: Option<PathBuf>,
    /// The agent, as memories record it.
    client: String,
    /// Where relative paths start (the agent's folder).
    cwd: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    content: String,
    is_error: bool,
    /// DevCat doesn't have this tool (or speaks another version): `devcat mcp` runs it itself.
    #[serde(default)]
    unknown: bool,
}

/// What this DevCat serves: the file and its token (to remove the file only if it's still ours).
static SERVING: OnceCell<(PathBuf, String)> = OnceCell::new();
/// Where `devcat mcp` looks for the open DevCat.
static CLIENT_DIR: OnceCell<PathBuf> = OnceCell::new();

fn token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Equal without telling, by how long it takes, how much of `a` was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------- DevCat's side

/// Start serving tool calls for `devcat mcp` (the app and `devcat serve`, from `init_backend`).
pub fn start(data_dir: PathBuf) {
    crate::rt::spawn(async move {
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("agent bridge: can't listen: {e}");
                return;
            }
        };
        let Ok(addr) = listener.local_addr() else { return };
        let info = Info { v: VERSION, port: addr.port(), token: token(), pid: std::process::id() };
        let path = data_dir.join(FILE);
        if let Err(e) = write(&path, &info) {
            eprintln!("agent bridge: can't write {}: {e}", path.display());
            return;
        }
        let _ = SERVING.set((path.clone(), info.token.clone()));
        let token = std::sync::Arc::new(info.token.clone());
        let mut tick = tokio::time::interval(REWRITE);
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { continue };
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    let token = token.clone();
                    tokio::spawn(async move { serve(stream, &token).await });
                }
                _ = tick.tick() => {
                    // another DevCat took over and quit since: be the one again
                    if !path.exists() {
                        let _ = write(&path, &info);
                    }
                }
            }
        }
    });
}

/// Remove the file when this DevCat quits (unless another one has written it since).
pub fn stop() {
    let Some((path, token)) = SERVING.get() else { return };
    if read(path).is_some_and(|i| same(&i.token, token)) {
        let _ = std::fs::remove_file(path);
    }
}

fn write(path: &Path, info: &Info) -> crate::error::AppResult<()> {
    crate::network::store::write_private(path, &serde_json::to_vec(info)?)
}

fn read(path: &Path) -> Option<Info> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

async fn serve(stream: TcpStream, token: &str) {
    let (r, mut w) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(r.take(MAX_LINE));
    match tokio::time::timeout(READ_CALL, reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => {}
        _ => return,
    }
    let Ok(call) = serde_json::from_str::<Call>(&line) else { return };
    if !same(&call.token, token) {
        return;
    }
    let reply = if call.v != VERSION { Reply { content: String::new(), is_error: false, unknown: true } } else { run(call).await };
    let mut out = serde_json::to_string(&reply).unwrap_or_default();
    out.push('\n');
    let _ = w.write_all(out.as_bytes()).await;
    let _ = w.flush().await;
}

async fn run(call: Call) -> Reply {
    let scope = Scope { project: call.project, folder: call.folder, mcp: true, unattended: false };
    let tool = crate::mcp_server::tools_for(&scope, &call.client).into_iter().find(|t| t.spec().name == call.tool);
    let Some(tool) = tool else { return Reply { content: String::new(), is_error: false, unknown: true } };
    let ctx = ToolCtx::at(call.cwd);
    let limit = tool.timeout(&call.input);
    let out = match tokio::time::timeout(limit, tool.run(call.input, &ctx)).await {
        Ok(o) => o,
        Err(_) => ToolOutput::err(format!("Timed out after {} s.", limit.as_secs())),
    };
    Reply { content: devcat_harness::tools::truncate(&out.content), is_error: out.is_error, unknown: false }
}

// ---------------------------------------------------------------- `devcat mcp`'s side

/// `devcat mcp` looks for the open DevCat in `data_dir`.
pub fn use_from(data_dir: PathBuf) {
    let _ = CLIENT_DIR.set(data_dir);
}

/// Run the tool `name` in the open DevCat. None when no DevCat is open (or it doesn't know the
/// tool), so the caller runs it itself; an error when DevCat took the call and then failed (it
/// may have done part of it, so it isn't run twice).
pub async fn call(scope: &Scope, client: &str, name: &str, input: &Value, cwd: &Path, limit: Duration) -> Option<ToolOutput> {
    call_at(CLIENT_DIR.get()?, scope, client, name, input, cwd, limit).await
}

async fn call_at(dir: &Path, scope: &Scope, client: &str, name: &str, input: &Value, cwd: &Path, limit: Duration) -> Option<ToolOutput> {
    let info = read(&dir.join(FILE)).filter(|i| i.v == VERSION)?;
    let stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", info.port))).await.ok()?.ok()?;
    let call = Call {
        v: VERSION,
        token: info.token,
        tool: name.to_string(),
        input: input.clone(),
        project: scope.project.clone(),
        folder: scope.folder.clone(),
        client: client.to_string(),
        cwd: cwd.to_path_buf(),
    };
    let mut line = serde_json::to_string(&call).ok()?;
    line.push('\n');
    let (r, mut w) = stream.into_split();
    // nothing was sent yet: DevCat may simply be gone, the call can run here
    w.write_all(line.as_bytes()).await.ok()?;
    let _ = w.flush().await;
    let mut answer = String::new();
    let mut reader = BufReader::new(r.take(MAX_LINE));
    // DevCat applies the tool's own limit; a little more for the way there and back
    let read = tokio::time::timeout(limit + Duration::from_secs(15), reader.read_line(&mut answer)).await;
    let failed = |why: &str| Some(ToolOutput::err(format!("DevCat took the call but {why}; it may have done part of it. Check before trying again.")));
    match read {
        Ok(Ok(n)) if n > 0 => {}
        Ok(_) => return failed("closed the connection without answering (did it quit?)"),
        Err(_) => return failed(&format!("didn't answer within {} s", (limit + Duration::from_secs(15)).as_secs())),
    }
    match serde_json::from_str::<Reply>(&answer) {
        Ok(r) if r.unknown => None,
        Ok(r) => Some(ToolOutput { content: r.content, is_error: r.is_error, ..ToolOutput::ok("") }),
        Err(_) => failed("answered something this `devcat mcp` doesn't understand"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        let t = token();
        assert_eq!(t.len(), 64);
        assert!(same(&t, &t.clone()));
        assert!(!same(&t, &token()));
        assert!(!same(&t, &t[..63]));
    }

    #[tokio::test]
    async fn calls_go_to_the_open_devcat_and_need_the_token() {
        crate::secrets::memory_only();
        crate::memory::tests::data();
        let data = std::env::temp_dir().join(format!("devcat-bridge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        // a DevCat on a port of our own, so the test doesn't meet a real one
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let info = Info { v: VERSION, port: listener.local_addr().unwrap().port(), token: token(), pid: 1 };
        write(&data.join(FILE), &info).unwrap();
        let tok = info.token.clone();
        tokio::spawn(async move {
            loop {
                let (s, _) = listener.accept().await.unwrap();
                let tok = tok.clone();
                tokio::spawn(async move { serve(s, &tok).await });
            }
        });
        let scope = Scope { mcp: true, ..Scope::default() };
        let call = |name: &'static str, input: Value, secs: u64| {
            let (data, scope) = (data.clone(), scope.clone());
            async move { call_at(&data, &scope, "claude-code", name, &input, &data, Duration::from_secs(secs)).await }
        };
        // DevCat has memory tools: it runs the call and answers
        let out = call("memory_search", serde_json::json!({ "query": "nothing-matches-this-zz" }), 10).await;
        assert!(out.is_some_and(|o| !o.is_error), "memory_search runs in DevCat");
        // a tool DevCat doesn't have: run it here
        assert!(call("no_such_tool", serde_json::json!({}), 5).await.is_none());
        // a wrong token gets no answer
        let bad = Info { token: token(), ..info.clone() };
        write(&data.join(FILE), &bad).unwrap();
        let out = call("memory_search", serde_json::json!({}), 1).await;
        assert!(out.is_some_and(|o| o.is_error), "the stranger is refused");
        // no file: no DevCat
        std::fs::remove_file(data.join(FILE)).unwrap();
        assert!(call("memory_search", serde_json::json!({}), 1).await.is_none());
    }
}
