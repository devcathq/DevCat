//! Port forwards of a saved SSH session (`-L` / `-R` / `-D`), started and stopped from the sidebar.
//! Each session's forwards run in one `ssh -N` of their own (not the shared master connection),
//! with the session's key, jump host and saved password, so stopping them never touches terminals.
//!
//! Events: `ssh-forward` → `ForwardStatus` (on every state change).
use super::remote::{self, Session, Target};
use crate::error::{AppError, AppResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

/// One forward, as saved in the session's `forwards` option (JSON list).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForward {
    /// "local": the host's `remote_host:remote_port` at localhost:`local_port` here (-L).
    /// "remote": this computer's localhost:`local_port` at 127.0.0.1:`remote_port` on the host (-R).
    /// "dynamic": a SOCKS5 proxy at localhost:`local_port` (-D).
    pub kind: String,
    #[serde(default)]
    pub local_port: u16,
    #[serde(default)]
    pub remote_host: String,
    #[serde(default)]
    pub remote_port: u16,
}

impl SessionForward {
    pub fn validate(&self) -> AppResult<()> {
        let bad = |m: &str| Err(AppError::msg(format!("Port forward: {m}")));
        if self.local_port == 0 {
            return bad("enter the port on this computer");
        }
        match self.kind.as_str() {
            "local" => {
                let h = self.remote_host.trim();
                if h.starts_with('-') || h.contains(char::is_whitespace) || h.contains(['[', ']']) {
                    return bad("the remote host can't start with - or contain spaces");
                }
                if self.remote_port == 0 {
                    return bad("enter the port on the host");
                }
            }
            "remote" if self.remote_port == 0 => return bad("enter the port on the host"),
            "remote" | "dynamic" => {}
            k => return bad(&format!("unknown kind {k}")),
        }
        Ok(())
    }

    fn target_host(&self) -> String {
        let h = self.remote_host.trim();
        let h = if h.is_empty() { "localhost" } else { h };
        if h.contains(':') { format!("[{h}]") } else { h.to_string() }
    }

    /// The ssh option for this forward.
    fn arg(&self) -> [String; 2] {
        match self.kind.as_str() {
            "remote" => ["-R".into(), format!("127.0.0.1:{}:127.0.0.1:{}", self.remote_port, self.local_port)],
            "dynamic" => ["-D".into(), format!("127.0.0.1:{}", self.local_port)],
            _ => ["-L".into(), format!("127.0.0.1:{}:{}:{}", self.local_port, self.target_host(), self.remote_port)],
        }
    }

    /// "localhost:3000 → localhost:3000 on the host", for the status line.
    pub fn label(&self) -> String {
        let remote = self.remote_host.trim();
        let remote = if remote.is_empty() { "localhost" } else { remote };
        match self.kind.as_str() {
            "remote" => format!("host's localhost:{} → localhost:{}", self.remote_port, self.local_port),
            "dynamic" => format!("SOCKS5 proxy on localhost:{}", self.local_port),
            _ => format!("localhost:{} → {remote}:{} on the host", self.local_port, self.remote_port),
        }
    }
}

/// Parse the `forwards` option (a JSON list; empty = none).
pub fn parse(json: Option<&str>) -> AppResult<Vec<SessionForward>> {
    let Some(json) = json.filter(|j| !j.trim().is_empty()) else { return Ok(vec![]) };
    let list: Vec<SessionForward> = serde_json::from_str(json).map_err(|e| AppError::msg(format!("Port forwards: {e}")))?;
    for f in &list {
        f.validate()?;
    }
    Ok(list)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStatus {
    pub conn_id: String,
    /// "starting" | "running" | "stopped" | "error"
    pub state: String,
    pub error: Option<String>,
    /// One line per forward.
    pub forwards: Vec<String>,
}

struct Running {
    status: ForwardStatus,
    generation: u64,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

static RUNNING: Lazy<Mutex<HashMap<String, Running>>> = Lazy::new(Default::default);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Printed by ssh (LocalCommand) once it's logged in and listening.
const READY: &str = "devcat-forwards-ready";

fn publish(status: &ForwardStatus) {
    crate::events::emit("ssh-forward", status);
}

/// Update the entry of `generation` (a stop or restart may have replaced it).
fn set(id: &str, generation: u64, f: impl FnOnce(&mut ForwardStatus)) {
    let mut map = RUNNING.lock();
    let Some(r) = map.get_mut(id).filter(|r| r.generation == generation) else { return };
    f(&mut r.status);
    publish(&r.status);
    if r.status.state != "starting" && r.status.state != "running" {
        r.stop = None;
    }
}

/// The ssh arguments for `t`'s forwards.
fn forward_args(t: &Target, forwards: &[SessionForward]) -> Vec<String> {
    let mut a = vec!["-N".to_string(), "-T".into()];
    a.extend(remote::ssh_args_with(t, false, false));
    a.extend(
        ["ExitOnForwardFailure=yes", "ControlMaster=no", "ControlPath=none", "PermitLocalCommand=yes"]
            .into_iter()
            .flat_map(|o| ["-o".to_string(), o.to_string()]),
    );
    a.extend(["-o".into(), format!("LocalCommand=echo {READY}")]);
    for f in forwards {
        a.extend(f.arg());
    }
    a.push("--".into());
    a.push(t.ssh_dest().to_string());
    a
}

/// Start session `conn_id`'s saved forwards (restarting them if they're running).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ssh_forward_start(conn_id: String) -> AppResult<ForwardStatus> {
    let session = Session::load(&format!("{}{conn_id}", remote::SESSION_PREFIX)).ok_or_else(|| AppError::msg("This SSH session was deleted"))?;
    if session.forwards.is_empty() {
        return Err(AppError::msg("This session has no port forwards: add them in Edit connection → Port forwards"));
    }
    stop(&conn_id);
    let t = Target::for_session(session.clone(), "~");
    t.ready().await?;
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let status = ForwardStatus {
        conn_id: conn_id.clone(),
        state: "starting".into(),
        error: None,
        forwards: session.forwards.iter().map(SessionForward::label).collect(),
    };
    let (env, askpass) = remote::askpass_env(&t);
    let mut askpass = Some(askpass);
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(forward_args(&t, &session.forwards))
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // no console window
    }
    kill_leftover(&conn_id);
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("Couldn't run ssh ({e}). Install the OpenSSH client.")))?;
    remember_pid(&conn_id, child.id());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    RUNNING.lock().insert(conn_id.clone(), Running { status: status.clone(), generation, stop: Some(stop_tx) });
    publish(&status);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let id = conn_id.clone();
    tokio::spawn(async move {
        // Logged in and listening: the password file has done its job.
        let ready_id = id.clone();
        let ready = async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if l.trim() == READY {
                        askpass.take();
                        set(&ready_id, generation, |s| s.state = "running".into());
                    }
                }
            }
        };
        let errors = async move {
            let mut b = Vec::new();
            if let Some(mut e) = stderr {
                let _ = e.read_to_end(&mut b).await;
            }
            b
        };
        let ready_task = tokio::spawn(ready);
        let errors_task = tokio::spawn(errors);
        tokio::select! {
            res = child.wait() => {
                ready_task.abort();
                let err = errors_task.await.unwrap_or_default();
                let err: Vec<String> = String::from_utf8_lossy(&err)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
                    .map(str::to_string)
                    .collect();
                let was_running = RUNNING.lock().get(&id).is_some_and(|r| r.generation == generation && r.status.state == "running");
                let msg = match (res.ok().and_then(|s| s.code()), err.join(" ")) {
                    (_, e) if was_running => format!("The forwards stopped: {}", if e.is_empty() { "the connection closed".into() } else { e }),
                    (_, e) if e.contains("forwarding") || e.contains("bind") => format!("Port forwards couldn't start: {e}"),
                    (Some(255), e) => remote::connect_error(&t, &e).to_string(),
                    (_, e) if e.is_empty() => "ssh exited before the forwards started".into(),
                    (_, e) => e,
                };
                set(&id, generation, |s| {
                    s.state = "error".into();
                    s.error = Some(msg);
                });
            }
            _ = stop_rx => {
                let _ = child.kill().await;
                ready_task.abort();
                errors_task.abort();
            }
        }
    });
    Ok(status)
}

/// Where the pid of session `id`'s forward process is kept, so one left behind by a DevCat that
/// crashed or was killed (its ports still taken) is stopped the next time.
fn pid_file(id: &str) -> Option<std::path::PathBuf> {
    let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    remote::private_dir().map(|d| d.join(format!("fwd-{safe}.pid")))
}

fn remember_pid(id: &str, pid: Option<u32>) {
    if let (Some(f), Some(pid)) = (pid_file(id), pid) {
        let _ = std::fs::write(f, pid.to_string());
    }
}

fn kill_leftover(id: &str) {
    let Some(f) = pid_file(id) else { return };
    let pid = std::fs::read_to_string(&f).ok().and_then(|p| p.trim().parse::<i32>().ok());
    let _ = std::fs::remove_file(&f);
    #[cfg(unix)]
    if let Some(pid) = pid.filter(|p| *p > 1) {
        // Only if it's still one of ours (the pid may have been reused).
        let cmd = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]).output();
        if cmd.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(READY)) {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Stop session `id`'s forwards (no-op when they aren't running).
pub fn stop(id: &str) {
    if let Some(f) = pid_file(id) {
        let _ = std::fs::remove_file(f);
    }
    let Some(mut r) = RUNNING.lock().remove(id) else { return };
    if let Some(tx) = r.stop.take() {
        let _ = tx.send(());
    }
    r.status.state = "stopped".into();
    r.status.error = None;
    publish(&r.status);
}

/// Stop every session's forwards (DevCat is quitting).
pub fn stop_all() {
    let ids: Vec<String> = RUNNING.lock().keys().cloned().collect();
    for id in ids {
        stop(&id);
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ssh_forward_stop(conn_id: String) -> AppResult<()> {
    stop(&conn_id);
    Ok(())
}

/// Every session whose forwards are starting, running or failed.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ssh_forward_list() -> AppResult<Vec<ForwardStatus>> {
    Ok(RUNNING.lock().values().map(|r| r.status.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd(kind: &str, local: u16, host: &str, remote: u16) -> SessionForward {
        SessionForward { kind: kind.into(), local_port: local, remote_host: host.into(), remote_port: remote }
    }

    #[test]
    fn forward_options() {
        assert_eq!(fwd("local", 3000, "", 3000).arg(), ["-L".to_string(), "127.0.0.1:3000:localhost:3000".into()]);
        assert_eq!(fwd("local", 5433, "db.internal", 5432).arg(), ["-L".to_string(), "127.0.0.1:5433:db.internal:5432".into()]);
        assert_eq!(fwd("local", 8080, "::1", 80).arg(), ["-L".to_string(), "127.0.0.1:8080:[::1]:80".into()]);
        assert_eq!(fwd("remote", 3000, "", 8080).arg(), ["-R".to_string(), "127.0.0.1:8080:127.0.0.1:3000".into()]);
        assert_eq!(fwd("dynamic", 1080, "", 0).arg(), ["-D".to_string(), "127.0.0.1:1080".into()]);
        assert_eq!(fwd("local", 3000, "", 3000).label(), "localhost:3000 → localhost:3000 on the host");
    }

    #[test]
    fn parses_and_validates() {
        assert_eq!(parse(None).unwrap(), vec![]);
        assert_eq!(parse(Some(" ")).unwrap(), vec![]);
        let ok = parse(Some(r#"[{"kind":"local","localPort":3000,"remoteHost":"localhost","remotePort":3000},{"kind":"dynamic","localPort":1080}]"#)).unwrap();
        assert_eq!(ok.len(), 2);
        for bad in [
            r#"[{"kind":"local","localPort":0,"remotePort":3000}]"#,
            r#"[{"kind":"local","localPort":3000,"remotePort":0}]"#,
            r#"[{"kind":"local","localPort":3000,"remoteHost":"-oProxyCommand=x","remotePort":1}]"#,
            r#"[{"kind":"remote","localPort":3000}]"#,
            r#"[{"kind":"sideways","localPort":3000}]"#,
            "not json",
        ] {
            assert!(parse(Some(bad)).is_err(), "{bad}");
        }
    }
}
