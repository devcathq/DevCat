//! Claude Code hooks → agent status. Every terminal running Claude Code gets
//! `--settings {hooks…}` whose hook command POSTs the hook input to a tiny HTTP listener on
//! 127.0.0.1 (random port, per-launch secret in the URL). From those we know when an agent
//! starts working, finishes, or waits for the user, and report it as `agent-status` events
//! (desktop, phones) and push notifications.
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub pty_id: String,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    /// "working" | "idle" | "waiting"
    pub status: String,
    /// When `status` last changed (ms).
    pub since: i64,
    /// Waiting: what Claude asks for ("Claude needs your permission to use Bash").
    pub message: Option<String>,
    /// Idle after a turn: how long that turn ran (ms).
    pub last_turn_ms: Option<i64>,
    #[serde(skip)]
    turn_started: Option<i64>,
}

/// What the hooks listener learned from one hook call, for notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Finished { ran_ms: i64 },
    NeedsInput { message: String },
    Other,
}

struct Server {
    port: u16,
    token: String,
}

static SERVER: OnceCell<Server> = OnceCell::new();
static AGENTS: Lazy<Mutex<HashMap<String, AgentStatus>>> = Lazy::new(Default::default);

pub struct HookLaunch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Hook command: send the hook's JSON input (stdin) to DevCat; never fail the agent.
const HOOK_CMD: &str = r#"curl -s -m 3 -X POST -H 'content-type: application/json' --data-binary @- "$DEVCAT_HOOK_URL" >/dev/null 2>&1 || true"#;

fn settings_json() -> String {
    let cmd = serde_json::json!([{ "type": "command", "command": HOOK_CMD, "timeout": 5 }]);
    let all = serde_json::json!([{ "matcher": "", "hooks": cmd }]);
    let plain = serde_json::json!([{ "hooks": cmd }]);
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": plain,
            "Stop": plain,
            "Notification": all,
            "PostToolUse": all,
        }
    })
    .to_string()
}

/// Extra `claude` arguments and environment for the terminal `pty_id`, once the listener runs.
/// Unix only: the hook command needs a POSIX shell and curl.
pub fn launch(pty_id: &str) -> Option<HookLaunch> {
    if !cfg!(unix) {
        return None;
    }
    let s = SERVER.get()?;
    Some(HookLaunch {
        args: vec!["--settings".into(), settings_json()],
        env: vec![("DEVCAT_HOOK_URL".into(), format!("http://127.0.0.1:{}/hook/{}/{}", s.port, pty_id, s.token))],
    })
}

/// The terminal closed.
pub fn forget(pty_id: &str) {
    if AGENTS.lock().remove(pty_id).is_some() {
        super::emit("agent-gone", serde_json::json!({ "ptyId": pty_id }));
    }
}

pub fn all() -> Vec<AgentStatus> {
    AGENTS.lock().values().cloned().collect()
}

pub fn get(pty_id: &str) -> Option<AgentStatus> {
    AGENTS.lock().get(pty_id).cloned()
}

/// Apply one hook call to the agent's status (pure: tested without a server).
pub fn apply(pty_id: &str, hook: &Value, now: i64) -> (AgentStatus, Change) {
    let mut agents = AGENTS.lock();
    let a = agents.entry(pty_id.to_string()).or_insert_with(|| AgentStatus {
        pty_id: pty_id.to_string(),
        cwd: None,
        session_id: None,
        status: "idle".into(),
        since: now,
        message: None,
        last_turn_ms: None,
        turn_started: None,
    });
    let str_of = |k: &str| hook.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(c) = str_of("cwd") {
        a.cwd = Some(c);
    }
    if let Some(s) = str_of("session_id") {
        a.session_id = Some(s);
    }
    let set = |a: &mut AgentStatus, status: &str| {
        if a.status != status {
            a.status = status.to_string();
            a.since = now;
        }
    };
    let change = match str_of("hook_event_name").as_deref() {
        Some("UserPromptSubmit") => {
            a.turn_started = Some(now);
            a.message = None;
            set(a, "working");
            Change::Other
        }
        Some("PostToolUse") => {
            // A tool ran: any permission question was answered.
            a.message = None;
            if a.turn_started.is_none() {
                a.turn_started = Some(now);
            }
            set(a, "working");
            Change::Other
        }
        Some("Stop") => {
            let ran = a.turn_started.take().map(|t| now - t).unwrap_or(0);
            a.last_turn_ms = Some(ran);
            a.message = None;
            set(a, "idle");
            Change::Finished { ran_ms: ran }
        }
        Some("Notification") => {
            let message = str_of("message").unwrap_or_else(|| "Claude needs your attention".into());
            // After a turn, Claude also says it's waiting for input; "finished" already covered that.
            let idle_reminder = str_of("notification_type").as_deref() == Some("idle_prompt") || message.to_lowercase().contains("waiting for your input");
            if idle_reminder && a.status == "idle" {
                Change::Other
            } else {
                a.message = Some(message.clone());
                set(a, "waiting");
                Change::NeedsInput { message }
            }
        }
        _ => Change::Other,
    };
    (a.clone(), change)
}

/// Start the listener (once).
pub async fn start() -> std::io::Result<()> {
    if SERVER.get().is_some() {
        return Ok(());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = hex::encode(crate::remote::store::random_bytes::<16>());
    let _ = SERVER.set(Server { port, token });
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(handle(sock));
        }
    });
    Ok(())
}

async fn handle(mut sock: tokio::net::TcpStream) {
    let Some((path, body)) = read_request(&mut sock).await else { return };
    let _ = sock.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await;
    let Some(server) = SERVER.get() else { return };
    // /hook/<pty id>/<token>
    let mut parts = path.trim_start_matches('/').split('/');
    let (Some("hook"), Some(pty_id), Some(token)) = (parts.next(), parts.next(), parts.next()) else { return };
    if !crate::remote::constant_time_eq(token.as_bytes(), server.token.as_bytes()) {
        return;
    }
    let Ok(hook) = serde_json::from_slice::<Value>(&body) else { return };
    let (status, change) = apply(pty_id, &hook, chrono::Utc::now().timestamp_millis());
    super::emit("agent-status", serde_json::to_value(&status).unwrap_or_default());
    super::on_change(&status, change).await;
}

/// Minimal HTTP/1.1 request reader: (path, body). Bodies over 1 MB are refused.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    let read = async {
        let (head_end, len) = loop {
            let n = sock.read(&mut tmp).await.ok().filter(|n| *n > 0)?;
            buf.extend_from_slice(&tmp[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0usize);
                break (i + 4, len);
            }
            if buf.len() > 64 * 1024 {
                return None;
            }
        };
        if len > 1024 * 1024 {
            return None;
        }
        while buf.len() < head_end + len {
            let n = sock.read(&mut tmp).await.ok().filter(|n| *n > 0)?;
            buf.extend_from_slice(&tmp[..n]);
        }
        let first = String::from_utf8_lossy(&buf[..head_end]).lines().next()?.to_string();
        let mut it = first.split_whitespace();
        let (method, path) = (it.next()?, it.next()?.to_string());
        (method == "POST").then(|| (path, buf[head_end..head_end + len].to_vec()))
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), read).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turn_lifecycle() {
        let id = "t-lifecycle";
        let (s, c) = apply(id, &json!({ "hook_event_name": "UserPromptSubmit", "cwd": "/p", "session_id": "S" }), 1000);
        assert_eq!((s.status.as_str(), s.cwd.as_deref(), c), ("working", Some("/p"), Change::Other));
        let (s, c) = apply(id, &json!({ "hook_event_name": "Notification", "message": "Claude needs your permission to use Bash" }), 2000);
        assert_eq!(s.status, "waiting");
        assert_eq!(c, Change::NeedsInput { message: "Claude needs your permission to use Bash".into() });
        let (s, _) = apply(id, &json!({ "hook_event_name": "PostToolUse" }), 3000);
        assert_eq!((s.status.as_str(), s.message), ("working", None));
        let (s, c) = apply(id, &json!({ "hook_event_name": "Stop" }), 61_000);
        assert_eq!((s.status.as_str(), s.last_turn_ms, c), ("idle", Some(60_000), Change::Finished { ran_ms: 60_000 }));
        // The "waiting for your input" reminder after a finished turn is not news.
        let (s, c) = apply(id, &json!({ "hook_event_name": "Notification", "message": "Claude is waiting for your input" }), 120_000);
        assert_eq!((s.status.as_str(), c), ("idle", Change::Other));
        assert!(get(id).is_some());
        AGENTS.lock().remove(id);
    }

    #[test]
    fn settings_have_all_hooks() {
        let v: Value = serde_json::from_str(&settings_json()).unwrap();
        for k in ["UserPromptSubmit", "Stop", "Notification", "PostToolUse"] {
            assert!(v["hooks"][k][0]["hooks"][0]["command"].as_str().unwrap().contains("$DEVCAT_HOOK_URL"), "{k}");
        }
    }

    #[tokio::test]
    async fn listener_accepts_only_the_token() {
        start().await.unwrap();
        let s = SERVER.get().unwrap();
        let post = |path: String, body: &'static str| async move {
            let mut c = tokio::net::TcpStream::connect(("127.0.0.1", SERVER.get().unwrap().port)).await.unwrap();
            let req = format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            c.write_all(req.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ = c.read_to_end(&mut out).await;
            String::from_utf8_lossy(&out).into_owned()
        };
        let body = r#"{"hook_event_name":"UserPromptSubmit"}"#;
        assert!(post(format!("/hook/t-bad/{}", "0".repeat(32)), body).await.starts_with("HTTP/1.1 204"));
        assert!(get("t-bad").is_none());
        post(format!("/hook/t-good/{}", s.token), body).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(get("t-good").map(|a| a.status), Some("working".into()));
        // hooks are offered on Unix only (`launch`)
        match launch("p1") {
            Some(l) => assert!(l.env[0].1.ends_with(&format!("/hook/p1/{}", s.token))),
            None => assert!(!cfg!(unix), "Unix terminals get the hook"),
        }
        AGENTS.lock().remove("t-good");
    }
}
