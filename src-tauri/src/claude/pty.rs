//! Claude workspace: terminal. Pseudo-terminals running interactive `claude` (or the
//! user's shell / any program) for the xterm.js view.
//!
//! Events to the frontend:
//!   `pty-output` → { id, data }   (raw bytes, base64; batched ~12ms / 16KB)
//!   `pty-exit`   → { id, code }
use super::locate;
use super::runner::Sink;
use crate::error::{AppError, AppResult};
use base64::Engine;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BATCH_WINDOW: Duration = Duration::from_millis(12);
/// A saved SSH password is typed into ssh's login prompt only this early in a session.
const AUTOPASS_WINDOW: Duration = Duration::from_secs(90);
const AUTOPASS_BYTES: usize = 16 * 1024;
const BATCH_BYTES: usize = 16 * 1024;

/// Recent output kept per terminal, so a phone attaching later sees the current screen.
const SCROLLBACK_MAX: usize = 1024 * 1024;
const SCROLLBACK_KEEP: usize = 512 * 1024;

/// What the remote app shows about a terminal.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyInfo {
    pub id: String,
    pub cwd: String,
    /// Program name ("claude", "zsh", …).
    pub program: String,
    /// Runs Claude Code (gets the DevCat hooks: done / needs input notifications).
    pub agent: bool,
    /// "desktop" or "phone:<device name>".
    pub origin: String,
    pub started_at: i64,
    pub cols: u16,
    pub rows: u16,
}

struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Label of the window that opened the terminal (closed with it).
    owner: Mutex<Option<String>>,
    info: Mutex<PtyInfo>,
    scrollback: Mutex<Vec<u8>>,
    /// Input has been written (by the user, a quick command, a phone…): see `AutoPass`.
    typed: std::sync::atomic::AtomicBool,
}

static PTYS: Lazy<Mutex<HashMap<String, Arc<Pty>>>> = Lazy::new(Default::default);

fn get(id: &str) -> AppResult<Arc<Pty>> {
    PTYS.lock().get(id).cloned().ok_or_else(|| AppError::msg(format!("Terminal {id} is closed")))
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 }
}

/// The user's interactive shell.
fn default_shell() -> CommandBuilder {
    if cfg!(windows) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find_map(|p| std::env::split_paths(&path).map(|d| d.join(p)).find(|f| f.is_file()));
        match found {
            Some(p) => CommandBuilder::new(p),
            None => CommandBuilder::new("cmd.exe"),
        }
    } else {
        let shell = std::env::var("SHELL").ok().filter(|s| Path::new(s).is_file()).unwrap_or_else(|| "/bin/zsh".into());
        let mut c = CommandBuilder::new(shell);
        c.arg("-l");
        c
    }
}

/// Does this output end with the local ssh client's own login prompt? Only its formats:
/// `me@host's password:`, `(me@host) Password:` (keyboard-interactive, OpenSSH 8.6+) and
/// `Enter passphrase for key '…':`, never a remote program's (`Password:` from su, sudo, mysql…).
fn is_login_prompt(out: &[u8]) -> bool {
    let text = String::from_utf8_lossy(out);
    let line = text.trim_end().rsplit(['\n', '\r']).next().unwrap_or("").trim().to_lowercase();
    (line.ends_with("'s password:") && line.contains('@'))
        || (line.starts_with('(') && line.contains('@') && line.ends_with(") password:"))
        || (line.starts_with("enter passphrase for key") && line.ends_with(':'))
}

/// Answers ssh's login prompt with a saved SSH session's password: once, only at the start of the
/// session, and never after the user typed something (then they're past the login, or doing it).
struct AutoPass {
    password: Option<String>,
    seen: usize,
    started: Instant,
}

impl AutoPass {
    fn new(password: Option<String>) -> Self {
        AutoPass { password, seen: 0, started: Instant::now() }
    }

    /// The next output batch; returns what to type, if anything.
    fn feed(&mut self, batch: &[u8], now: Instant, user_typed: bool) -> Option<String> {
        self.password.as_ref()?;
        self.seen += batch.len();
        if user_typed || self.seen > AUTOPASS_BYTES || now.duration_since(self.started) > AUTOPASS_WINDOW {
            self.password = None;
            return None;
        }
        // Once: a second prompt means it was wrong; the user types from there.
        is_login_prompt(batch).then(|| format!("{}\r", self.password.take().unwrap_or_default()))
    }
}

/// Is this program Claude Code ("claude" or a path to the claude binary)?
fn is_claude(program: Option<&str>) -> bool {
    program.is_some_and(|p| Path::new(p).file_stem().and_then(|s| s.to_str()) == Some("claude"))
}

fn program_label(program: Option<&str>) -> String {
    match program.filter(|p| !p.is_empty()) {
        Some(p) => Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string()),
        None => std::env::var("SHELL").ok().and_then(|s| Path::new(&s).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "shell".into()),
    }
}

async fn build_command(cwd: &str, program: Option<String>, args: Vec<String>, env: Vec<(String, String)>) -> AppResult<CommandBuilder> {
    let mut cmd = match program.as_deref().filter(|p| !p.is_empty()) {
        None => default_shell(),
        Some("claude") => CommandBuilder::new(
            locate::claude_path().ok_or_else(|| AppError::msg("claude CLI not found (install Claude Code or set CLAUDE_PATH)"))?,
        ),
        Some(p) => CommandBuilder::new(p),
    };
    cmd.args(args);
    cmd.cwd(cwd);
    cmd.env("PATH", locate::child_path().await);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if std::env::var_os("LANG").is_none_or(|v| v.is_empty()) {
        cmd.env("LANG", "en_US.UTF-8");
    }
    // Don't let claude think it's nested in another Claude Code / misdetect the host terminal.
    for k in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_CHILD_SESSION", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"] {
        cmd.env_remove(k);
    }
    // AI connection env (incl. its API key from the keychain) last, so it can override the above.
    for (k, v) in env {
        cmd.env(k, v);
    }
    Ok(cmd)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn open(
    sink: Sink,
    cwd: &str,
    program: Option<String>,
    mut args: Vec<String>,
    mut env: Vec<(String, String)>,
    cols: u16,
    rows: u16,
    origin: &str,
) -> AppResult<String> {
    // A folder on an SSH host (ssh://…): the program runs there, through the system ssh.
    let remote = super::remote::Target::parse(cwd);
    if remote.is_none() && !Path::new(cwd).is_dir() {
        return Err(AppError::msg(format!("Not a directory: {cwd}")));
    }
    // A saved SSH session: its VPN / tunnel first; its password answers ssh's login prompt.
    let mut autopass = None;
    if let Some(t) = &remote {
        t.ready().await?;
        autopass = t.session.as_ref().and_then(|s| s.password.clone());
    }
    let uid = uuid::Uuid::new_v4();
    let id = uid.to_string();
    let agent = is_claude(program.as_deref());
    // Remote agents report through a reverse forward: host 127.0.0.1:<this> → DevCat's hook server.
    let mut forward = None;
    if agent {
        // Claude Code hooks report "finished" / "needs input" back to DevCat (notifications).
        if let Some(h) = crate::notify::hooks::launch(&id) {
            args.splice(0..0, h.args);
            if remote.is_some() {
                let rp = 20000 + (uid.as_u128() % 40000) as u16;
                for (k, v) in h.env {
                    let local = v.split("127.0.0.1:").nth(1).and_then(|r| r.split('/').next()).and_then(|p| p.parse::<u16>().ok());
                    if let Some(lp) = local {
                        forward = Some((rp, lp));
                        env.push((k, v.replacen(&format!("127.0.0.1:{lp}"), &format!("127.0.0.1:{rp}"), 1)));
                    }
                }
            } else {
                env.extend(h.env);
            }
        }
    }
    let info = PtyInfo {
        id: id.clone(),
        cwd: cwd.to_string(),
        program: program_label(program.as_deref()),
        agent,
        origin: origin.to_string(),
        started_at: chrono::Utc::now().timestamp_millis(),
        cols,
        rows,
    };
    let cmd = match &remote {
        Some(t) => {
            // Only DevCat's own variables go to the host (an AI connection's keys stay here).
            let env: Vec<(String, String)> = env.into_iter().filter(|(k, _)| k.starts_with("DEVCAT_")).collect();
            let prog = program.as_deref().map(|p| if is_claude(Some(p)) { "claude" } else { p });
            let mut c = CommandBuilder::new("ssh");
            c.args(super::remote::terminal_args(t, prog, &args, &env, forward));
            if let Some(h) = dirs::home_dir() {
                c.cwd(h);
            }
            c.env("PATH", locate::child_path().await);
            c.env("TERM", "xterm-256color");
            c.env("COLORTERM", "truecolor");
            c
        }
        None => build_command(cwd, program, args, env).await?,
    };
    let pair = native_pty_system().openpty(size(cols, rows)).map_err(|e| AppError::msg(format!("openpty: {e}")))?;
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| AppError::msg(format!("Failed to start terminal program: {e}")))?;
    drop(pair.slave); // so the reader sees EOF once the child exits
    let mut reader = pair.master.try_clone_reader().map_err(|e| AppError::msg(e.to_string()))?;
    let writer = pair.master.take_writer().map_err(|e| AppError::msg(e.to_string()))?;
    let pty = Arc::new(Pty {
        master: Mutex::new(pair.master),
        writer: Mutex::new(Some(writer)),
        killer: Mutex::new(child.clone_killer()),
        owner: Mutex::new(None),
        info: Mutex::new(info),
        scrollback: Mutex::new(Vec::new()),
        typed: std::sync::atomic::AtomicBool::new(false),
    });
    PTYS.lock().insert(id.clone(), pty.clone());

    // reader → batcher → pty-output
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let (s, pid) = (sink.clone(), id.clone());
    let mut autopass = AutoPass::new(autopass);
    let batcher = std::thread::spawn(move || {
        let emit = |data: &[u8]| {
            {
                let mut sb = pty.scrollback.lock();
                sb.extend_from_slice(data);
                if sb.len() > SCROLLBACK_MAX {
                    let cut = sb.len() - SCROLLBACK_KEEP;
                    sb.drain(..cut);
                }
            }
            s("pty-output", json!({ "id": pid, "data": base64::engine::general_purpose::STANDARD.encode(data) }));
        };
        while let Ok(first) = rx.recv() {
            let mut batch = first;
            let deadline = Instant::now() + BATCH_WINDOW;
            while batch.len() < BATCH_BYTES {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(more) => batch.extend_from_slice(&more),
                    Err(_) => break,
                }
            }
            emit(&batch);
            if let Some(answer) = autopass.feed(&batch, Instant::now(), pty.typed.load(std::sync::atomic::Ordering::Relaxed)) {
                if let Some(w) = pty.writer.lock().as_mut() {
                    let _ = w.write_all(answer.as_bytes()).and_then(|_| w.flush());
                }
            }
        }
    });

    // exit watcher
    let pid = id.clone();
    std::thread::spawn(move || {
        let code = child.wait().map(|s| s.exit_code()).ok();
        // Flush remaining output first (bounded: background jobs may keep the tty open).
        let deadline = Instant::now() + Duration::from_millis(500);
        while !batcher.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        PTYS.lock().remove(&pid);
        crate::notify::hooks::forget(&pid);
        sink("pty-exit", json!({ "id": pid, "code": code }));
    });
    Ok(id)
}

/// Open a terminal in `cwd`. `program`: "claude" (resolved binary), None (login shell) or any program.
/// With `provider_id` the AI connection decides program / env instead (its args come before `args`).
#[cfg_attr(feature = "gui", tauri::command)]
#[allow(clippy::too_many_arguments)]
pub async fn pty_open(
    window: crate::events::Owner,
    cwd: String,
    program: Option<String>,
    args: Vec<String>,
    cols: u16,
    rows: u16,
    provider_id: Option<String>,
) -> AppResult<String> {
    let (program, args, env) = match provider_id.filter(|p| !p.is_empty()) {
        Some(pid) => {
            let l = crate::ai::launch(&pid, args).await?;
            (Some(l.program), l.args, l.env)
        }
        None => (program, args, vec![]),
    };
    let id = open(crate::events::sink(), &cwd, program, args, env, cols, rows, "desktop").await?;
    if let Some(p) = PTYS.lock().get(&id) {
        *p.owner.lock() = Some(window.label().to_string());
    }
    Ok(id)
}

/// Raw keystrokes (UTF-8).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn pty_write(id: String, data: String) -> AppResult<()> {
    let pty = get(&id)?;
    pty.typed.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut w = pty.writer.lock();
    let w = w.as_mut().ok_or_else(|| AppError::msg("Terminal input is closed"))?;
    w.write_all(data.as_bytes())?;
    w.flush()?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn pty_resize(id: String, cols: u16, rows: u16) -> AppResult<()> {
    let pty = get(&id)?;
    pty.master.lock().resize(size(cols, rows)).map_err(|e| AppError::msg(format!("resize: {e}")))?;
    let mut info = pty.info.lock();
    (info.cols, info.rows) = (cols.max(1), rows.max(1));
    Ok(())
}

/// Every open terminal, oldest first.
pub fn list() -> Vec<PtyInfo> {
    let mut v: Vec<PtyInfo> = PTYS.lock().values().map(|p| p.info.lock().clone()).collect();
    v.sort_by_key(|i| i.started_at);
    v
}

pub fn info(id: &str) -> AppResult<PtyInfo> {
    Ok(get(id)?.info.lock().clone())
}

/// Recent output (raw bytes) of a terminal, for a viewer attaching late.
pub fn scrollback(id: &str) -> AppResult<Vec<u8>> {
    Ok(get(id)?.scrollback.lock().clone())
}

fn kill(pty: &Pty) {
    // Kill before dropping the writer: dropping it sends EOF, which lets the program exit cleanly first.
    let _ = pty.killer.lock().kill();
    pty.writer.lock().take();
}

/// Kill the program and drop the terminal (`pty-exit` still follows).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn pty_close(id: String) -> AppResult<()> {
    if let Some(p) = PTYS.lock().remove(&id) {
        kill(&p);
    }
    Ok(())
}

/// Kill every terminal opened by a window (it was closed).
pub fn close_owned_by(label: &str) {
    let mut ptys = PTYS.lock();
    let ids: Vec<String> = ptys.iter().filter(|(_, p)| p.owner.lock().as_deref() == Some(label)).map(|(id, _)| id.clone()).collect();
    let owned: Vec<Arc<Pty>> = ids.iter().filter_map(|id| ptys.remove(id)).collect();
    drop(ptys);
    for p in owned {
        kill(&p);
    }
}

/// Kill every terminal (app exit).
pub fn shutdown_all() {
    let all: Vec<Arc<Pty>> = PTYS.lock().drain().map(|(_, p)| p).collect();
    for p in all {
        kill(&p);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::claude::runner::tests::channel_sink;

    async fn collect_until_exit(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(&'static str, serde_json::Value)>) -> (String, serde_json::Value) {
        let mut out = Vec::new();
        loop {
            let (e, v) = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("timeout").expect("closed");
            match e {
                "pty-output" => out.extend(base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap()),
                "pty-exit" => return (String::from_utf8_lossy(&out).into_owned(), v["code"].clone()),
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn runs_program_and_reports_exit() {
        let (sink, mut rx) = channel_sink();
        let id = open(sink, "/tmp", Some("sh".into()), vec!["-c".into(), "printf hi-$TERM-$DEVCAT_T; exit 3".into()], vec![("DEVCAT_T".into(), "x1".into())], 80, 24, "desktop").await.unwrap();
        pty_resize(id.clone(), 100, 30).await.unwrap();
        let (out, code) = collect_until_exit(&mut rx).await;
        assert!(out.contains("hi-xterm-256color-x1"), "{out:?}");
        assert_eq!(code, 3);
        assert!(pty_write(id, "x".into()).await.is_err(), "closed after exit");
    }

    #[tokio::test]
    async fn interactive_input_and_close() {
        let (sink, mut rx) = channel_sink();
        let id = open(sink, "/tmp", Some("cat".into()), vec![], vec![], 80, 24, "phone:Test").await.unwrap();
        assert!(list().iter().any(|i| i.id == id && i.origin == "phone:Test" && i.program == "cat" && !i.agent));
        pty_write(id.clone(), "ping\r".into()).await.unwrap();
        let mut seen = String::new();
        while !seen.contains("ping") {
            let (e, v) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
            if e == "pty-output" {
                seen.push_str(&String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap()));
            }
        }
        assert!(String::from_utf8_lossy(&scrollback(&id).unwrap()).contains("ping"));
        pty_close(id.clone()).await.unwrap();
        let (_, code) = collect_until_exit(&mut rx).await;
        assert_ne!(code, 0);
        assert!(pty_resize(id, 10, 10).await.is_err());
    }

    /// Resolves the real claude binary (free: --version only).
    #[tokio::test]
    #[ignore]
    async fn real_claude_version() {
        let (sink, mut rx) = channel_sink();
        open(sink, "/tmp", Some("claude".into()), vec!["--version".into()], vec![], 80, 24, "desktop").await.unwrap();
        let (out, code) = collect_until_exit(&mut rx).await;
        println!("{out:?} {code}");
        assert!(out.contains("Claude Code") && code == 0);
    }

    #[tokio::test]
    async fn rejects_bad_cwd() {
        let (sink, _rx) = channel_sink();
        assert!(open(sink, "/definitely/not/here", None, vec![], vec![], 80, 24, "desktop").await.is_err());
    }

    #[test]
    fn spots_ssh_login_prompts() {
        assert!(is_login_prompt(b"me@10.0.0.5's password: "));
        assert!(is_login_prompt(b"Warning: Permanently added 'x' (ED25519) to the list of known hosts.\r\n(me@box) Password:"));
        for remote in [&b"Password:"[..], b"[sudo] password for me: ", b"Enter password: ", b"me@box:~$ su\r\nPassword: "] {
            assert!(!is_login_prompt(remote), "{}", String::from_utf8_lossy(remote));
        }
        assert!(is_login_prompt(b"Enter passphrase for key '/Users/me/.ssh/id_ed25519': "));
        assert!(!is_login_prompt(b"Are you sure you want to continue connecting (yes/no/[fingerprint])? "));
        assert!(!is_login_prompt(b"password: hunter2 was rejected\r\nme@box:~$ "));
    }

    #[test]
    fn autopass_answers_once_and_only_early() {
        let t0 = Instant::now();
        let prompt = b"me@box's password: ";
        let mut a = AutoPass::new(Some("s3cret".into()));
        assert_eq!(a.feed(b"Warning: Permanently added '[box]:22' to known hosts.\r\n", t0, false), None);
        assert_eq!(a.feed(prompt, t0, false).as_deref(), Some("s3cret\r"));
        assert_eq!(a.feed(b"Permission denied, please try again.\r\nme@box's password: ", t0, false), None, "never twice");

        let mut late = AutoPass::new(Some("s3cret".into()));
        assert_eq!(late.feed(prompt, t0 + AUTOPASS_WINDOW + Duration::from_secs(1), false), None);
        assert_eq!(late.feed(prompt, t0, false), None, "gone once the window passed");

        let mut chatty = AutoPass::new(Some("s3cret".into()));
        assert_eq!(chatty.feed(&vec![b'x'; AUTOPASS_BYTES + 1], t0, false), None);
        assert_eq!(chatty.feed(prompt, t0, false), None, "gone after the login output");

        let mut typed = AutoPass::new(Some("s3cret".into()));
        assert_eq!(typed.feed(prompt, t0, true), None, "the user is typing it");
        assert_eq!(typed.feed(prompt, t0, false), None);

        assert_eq!(AutoPass::new(None).feed(prompt, t0, false), None);
    }

    #[test]
    fn detects_claude() {
        assert!(is_claude(Some("claude")) && is_claude(Some("/opt/homebrew/bin/claude")));
        assert!(!is_claude(None) && !is_claude(Some("codex")));
    }
}
