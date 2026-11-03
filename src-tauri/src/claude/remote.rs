//! Project folders on another machine, reached with the system `ssh` (so ~/.ssh/config hosts,
//! ProxyJump, ssh-agent, keys and known_hosts all work as in a terminal). A remote folder is
//! written `ssh://[user@]host[:port]/absolute/path`; the commands that take a folder or file
//! path (file tree, preview, save, git status, terminals) route such paths here.
//!
//! A saved SSH session (an `ssh` connection in the sidebar) is written `ssh://devcat-<id>/path`:
//! its host, user, port, key, jump host and password come from the saved connection (`Session`).
//!
//! On macOS / Linux one master connection per host is shared (OpenSSH ControlMaster), so file
//! operations are quick and a password typed once in a terminal covers them too. File commands
//! never prompt (BatchMode): without a key or agent, open a terminal first. A session with a saved
//! password answers through DevCat as SSH_ASKPASS instead (`askpass_main`).
use super::git::{parse_status_v2, StatusIndex};
use super::{FileEntry, FilePreview, GitStatus};
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub const SCHEME: &str = "ssh://";
const TIMEOUT: Duration = Duration::from_secs(40);
/// Files up to this size are copied whole for previews; bigger ones only their start.
const PREVIEW_COPY_CAP: u64 = 30 * 1024 * 1024;
const PREVIEW_HEAD: u64 = 2 * 1024 * 1024;
/// `ssh://devcat-<connection id>/…` = a saved SSH session.
pub const SESSION_PREFIX: &str = "devcat-";

/// How to reach a saved SSH session (from its connection profile).
#[derive(Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub name: String,
    /// Host name, IP or a ~/.ssh/config alias (passed to ssh as the destination, so the alias's
    /// settings apply).
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub key_path: Option<String>,
    /// ProxyJump (`bastion`, `me@jump:2222`, comma-separated hops).
    pub jump: Option<String>,
    /// Folder the terminal starts in (default: home).
    pub start_dir: Option<String>,
    /// Command run before the login shell (e.g. `tmux attach || tmux`).
    pub startup: Option<String>,
    /// From the keychain; never logged.
    pub password: Option<String>,
    /// VPN / tunnel profile connected first.
    pub network_id: Option<String>,
    /// Port forwards started from the sidebar (`ssh_forward`).
    pub forwards: Vec<super::ssh_forward::SessionForward>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("id", &self.id).field("host", &self.host).field("user", &self.user).field("port", &self.port).finish_non_exhaustive()
    }
}

fn opt(c: &ConnectionConfig, key: &str) -> Option<String> {
    c.options.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Session {
    pub fn from_config(c: &ConnectionConfig) -> AppResult<Session> {
        let host = c.host.trim();
        // Same rules as a typed destination: it's an ssh argument.
        if host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace) {
            return Err(AppError::msg("Enter the SSH host (a name, an IP or a ~/.ssh/config alias)"));
        }
        let user = Some(c.user.trim().to_string()).filter(|u| !u.is_empty());
        if user.as_deref().is_some_and(|u| u.starts_with('-') || u.contains(char::is_whitespace)) {
            return Err(AppError::msg("The SSH user can't start with - or contain spaces"));
        }
        // "a, b" → "a,b" (the hint says comma-separated; ssh wants no spaces).
        let jump = opt(c, "jump").map(|j| j.split(',').map(str::trim).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(","));
        if jump.as_deref().is_some_and(|j| j.starts_with('-') || j.contains(char::is_whitespace)) {
            return Err(AppError::msg("The jump host can't start with - or contain spaces"));
        }
        Ok(Session {
            id: c.id.clone(),
            name: c.name.clone(),
            host: host.to_string(),
            user,
            port: c.port.filter(|p| *p != 0),
            key_path: opt(c, "keyPath").map(|k| crate::db::ssh::expand_home(&k).to_string_lossy().into_owned()),
            jump,
            start_dir: opt(c, "startDir"),
            startup: opt(c, "startup"),
            password: c.password.clone().filter(|p| !p.is_empty()),
            network_id: opt(c, "networkId"),
            forwards: super::ssh_forward::parse(opt(c, "forwards").as_deref())?,
        })
    }

    /// The saved session `devcat-<id>` (with its password from the keychain).
    pub(crate) fn load(alias: &str) -> Option<Session> {
        let id = alias.strip_prefix(SESSION_PREFIX)?;
        let st = crate::state::St::try_get()?;
        let c = st.store.get(id).ok().filter(|c| c.kind == ConnectionKind::Ssh)?;
        Session::from_config(&c).ok()
    }

}

/// A folder or file on an SSH host.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// What `ssh` connects to: a ~/.ssh/config alias, `host` or `user@host` (or `devcat-<id>`
    /// for a saved session, see `session`).
    pub dest: String,
    pub port: Option<u16>,
    /// Absolute path on the host (`~` / `~/…` allowed: resolved there).
    pub path: String,
    /// A saved session (`dest` = `devcat-<id>`); None if it was deleted.
    pub session: Option<Box<Session>>,
}

impl Target {
    /// `ssh://dest[:port]/path` → Target (None for local paths).
    pub fn parse(s: &str) -> Option<Target> {
        let rest = s.strip_prefix(SCHEME)?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        // `/~/x` and `/~` mean the remote home
        let path = if path == "/~" || path.starts_with("/~/") { &path[1..] } else { path };
        let (dest, port) = match authority.rsplit_once(':') {
            Some((d, p)) if !d.is_empty() && p.parse::<u16>().is_ok() => (d, p.parse().ok()),
            _ => (authority, None),
        };
        if dest.is_empty() || dest.starts_with('-') || dest.contains(char::is_whitespace) {
            return None;
        }
        let session = if dest.starts_with(SESSION_PREFIX) { Session::load(dest).map(Box::new) } else { None };
        Some(Target { dest: dest.to_string(), port, path: path.to_string(), session })
    }

    /// A target for a session that may not be saved yet (Test connection).
    pub fn for_session(s: Session, path: &str) -> Target {
        Target { dest: format!("{SESSION_PREFIX}{}", s.id), port: None, path: path.to_string(), session: Some(Box::new(s)) }
    }

    /// Back to `ssh://…` for `path` on the same host.
    pub fn uri(&self, path: &str) -> String {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        format!("{SCHEME}{}{port}{path}", self.dest)
    }

    /// `devcat-…` written in the URI.
    pub fn is_session(&self) -> bool {
        self.dest.starts_with(SESSION_PREFIX)
    }

    /// The destination argument for `ssh`.
    pub fn ssh_dest(&self) -> &str {
        self.session.as_ref().map(|s| s.host.as_str()).unwrap_or(&self.dest)
    }

    /// For messages: the session's name, or the destination.
    pub fn label(&self) -> &str {
        self.session.as_ref().map(|s| s.name.as_str()).unwrap_or(&self.dest)
    }

    /// A saved session that no longer exists can't be reached.
    pub fn check(&self) -> AppResult<()> {
        if self.is_session() && self.session.is_none() {
            return Err(AppError::msg("This SSH session was deleted. Open it again from the sidebar."));
        }
        Ok(())
    }

    /// `check`, then connect the session's VPN / tunnel (if it has one) and wait for it.
    pub async fn ready(&self) -> AppResult<()> {
        self.check()?;
        if let Some(net) = self.session.as_ref().and_then(|s| s.network_id.as_deref()) {
            crate::network::ensure(net).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn with_path(&self, path: &str) -> Target {
        Target { path: path.to_string(), ..self.clone() }
    }
}

/// Single-quote for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A path for the remote shell: `~` / `~/x` expand there, the rest is quoted.
fn sh_path(p: &str) -> String {
    if p == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest))
    } else {
        sh_quote(p)
    }
}

/// Where master connections' sockets and one-shot password files live: short (Unix socket paths
/// max out around 104 bytes, and macOS' temp dir is long), per user, private. None when the
/// folder isn't safely ours (someone else made `/tmp/devcat-ssh-<uid>`, or it's a symlink).
pub(crate) fn private_dir() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let uid = unsafe { libc::geteuid() };
        let dir = std::path::PathBuf::from(format!("/tmp/devcat-ssh-{uid}"));
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
        let m = std::fs::symlink_metadata(&dir).ok()?;
        if !m.is_dir() || m.uid() != uid {
            return None;
        }
        if m.mode() & 0o077 != 0 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
        }
        Some(dir)
    }
    #[cfg(not(unix))]
    {
        let dir = std::env::temp_dir().join("devcat-ssh");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }
}

/// `ssh` options shared by file commands and terminals.
pub fn ssh_args(t: &Target, interactive: bool) -> Vec<String> {
    ssh_args_with(t, interactive, true)
}

/// `ssh_args`, optionally without the shared master connection (`mux`): a port-forward process
/// keeps its own connection, so stopping it never touches terminals.
pub(crate) fn ssh_args_with(t: &Target, interactive: bool, mux: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-o".into(), "ConnectTimeout=15".into(), "-o".into(), "ServerAliveInterval=30".into()];
    let askpass = !interactive && t.session.as_ref().is_some_and(|s| s.password.is_some());
    if !interactive && !askpass {
        a.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if askpass {
        // One try: a wrong saved password shouldn't lock the account out.
        a.extend(["-o".into(), "NumberOfPasswordPrompts=1".into()]);
    }
    if cfg!(unix) && mux {
        if let Some(dir) = private_dir() {
            a.extend([
                "-o".into(),
                "ControlMaster=auto".into(),
                "-o".into(),
                format!("ControlPath={}/%C", dir.display()),
                "-o".into(),
                "ControlPersist=600".into(),
            ]);
        }
    }
    let session = t.session.as_deref();
    if let Some(s) = session {
        if let Some(u) = &s.user {
            a.extend(["-o".into(), format!("User={u}")]);
        }
        if let Some(k) = &s.key_path {
            a.extend(["-i".into(), k.clone(), "-o".into(), "IdentitiesOnly=yes".into()]);
        }
        if let Some(j) = &s.jump {
            a.extend(["-J".into(), j.clone()]);
        }
    }
    if let Some(p) = t.port.or(session.and_then(|s| s.port)) {
        a.extend(["-p".into(), p.to_string()]);
    }
    a
}

/// The saved password for one `ssh` run, in a private file DevCat-as-askpass reads (see
/// `askpass_main`). Deleted when dropped: a master connection that outlives the command keeps
/// only the file's (dead) path in its environment, never the password.
#[derive(Default)]
pub struct Askpass(Option<std::path::PathBuf>);

impl Drop for Askpass {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Env for an `ssh` that answers a saved password through DevCat; keep the guard until it exits.
pub fn askpass_env(t: &Target) -> (Vec<(String, String)>, Askpass) {
    let (Some(pw), Ok(exe)) = (t.session.as_ref().and_then(|s| s.password.clone()), std::env::current_exe()) else {
        return (vec![], Askpass::default());
    };
    let Some(dir) = private_dir() else {
        return (vec![], Askpass::default());
    };
    let prompt = LoginPrompt::for_target(t);
    let file = dir.join(format!("pw-{}", uuid::Uuid::new_v4().simple()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    if o.open(&file).and_then(|mut f| std::io::Write::write_all(&mut f, pw.as_bytes())).is_err() {
        return (vec![], Askpass::default());
    }
    let env = vec![
        ("SSH_ASKPASS".into(), exe.to_string_lossy().into_owned()),
        ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        // Older OpenSSH only uses SSH_ASKPASS with a DISPLAY.
        ("DISPLAY".into(), std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into())),
        (ASKPASS_FILE.into(), file.to_string_lossy().into_owned()),
        (ASKPASS_FOR.into(), prompt.dests.join("\n")),
        (ASKPASS_KEY.into(), prompt.key.unwrap_or_default()),
    ];
    (env, Askpass(Some(file)))
}

const ASKPASS_FILE: &str = "DEVCAT_ASKPASS_FILE";
/// The `user@host` names (newline-separated) whose login prompt the saved password answers.
const ASKPASS_FOR: &str = "DEVCAT_ASKPASS_FOR";
/// The session's private key: its passphrase prompt is answered too.
const ASKPASS_KEY: &str = "DEVCAT_ASKPASS_KEY";

/// DevCat started by ssh as SSH_ASKPASS: print the saved password for the session's own login
/// prompt, refuse anything else: a jump host's password prompt (it must never see the target's
/// password) or an unknown host key (accepted in a terminal). Returns the exit code, or None when
/// this process isn't an askpass call.
pub fn askpass_main() -> Option<i32> {
    let file = std::env::var_os(ASKPASS_FILE)?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let expected = LoginPrompt {
        dests: std::env::var(ASKPASS_FOR).unwrap_or_default().lines().map(str::to_string).filter(|d| !d.is_empty()).collect(),
        key: std::env::var(ASKPASS_KEY).ok().filter(|k| !k.is_empty()),
    };
    match std::fs::read_to_string(file) {
        Ok(pw) if expected.matches(&prompt) => {
            println!("{pw}");
            Some(0)
        }
        _ => Some(1),
    }
}

/// The local ssh client's login prompts for one destination: `me@host's password:`,
/// `(me@host) Password:` (keyboard-interactive, OpenSSH 8.6+) and `Enter passphrase for key
/// '<the session's key>':`. A jump host's prompt names the jump host, so it doesn't match; nor does
/// a remote program's (`Password:` from su, sudo, mysql…).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoginPrompt {
    /// Lowercase `user@host` as ssh prints it.
    pub dests: Vec<String>,
    pub key: Option<String>,
}

/// `ssh -G` answers, cached briefly: the askpass env is built for every file command.
static RESOLVED: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<Vec<String>, (std::time::Instant, Vec<String>)>>> =
    once_cell::sync::Lazy::new(Default::default);

impl LoginPrompt {
    /// What ssh will print for `t`: the user and host after ~/.ssh/config is applied (`ssh -G`;
    /// an alias's HostName, a config User), plus the session's own spelling as a fallback.
    pub fn for_target(t: &Target) -> LoginPrompt {
        let s = t.session.as_deref();
        let mut dests = Vec::new();
        let mut args = ssh_args_with(t, false, false);
        args.extend(["-G".into(), "--".into(), t.ssh_dest().to_string()]);
        let cached = RESOLVED.lock().get(&args).filter(|(at, _)| at.elapsed() < Duration::from_secs(60)).map(|(_, d)| d.clone());
        let resolved = cached.unwrap_or_else(|| {
            let mut d = Vec::new();
            let mut cmd = std::process::Command::new("ssh");
            cmd.args(&args).stdin(Stdio::null()).stderr(Stdio::null());
            // A console program started by a GUI app opens a console window: it flashed on screen
            // for every SSH command of a session with a saved password (pasting an image, opening
            // a folder, a port forward, a terminal).
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            if let Ok(out) = cmd.output() {
                let text = String::from_utf8_lossy(&out.stdout);
                let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()));
                if let Some(user) = get("user ") {
                    for host in [get("hostname "), get("hostkeyalias ")].into_iter().flatten() {
                        d.push(format!("{user}@{host}"));
                    }
                }
            }
            RESOLVED.lock().insert(args, (std::time::Instant::now(), d.clone()));
            d
        });
        dests.extend(resolved);
        if let Some(s) = s {
            let user = s.user.clone().or_else(|| std::env::var("USER").ok()).or_else(|| std::env::var("USERNAME").ok());
            if let Some(u) = user {
                dests.push(format!("{u}@{}", s.host));
            }
        }
        let mut dests: Vec<String> = dests.into_iter().map(|d| d.to_lowercase()).collect();
        dests.dedup();
        LoginPrompt { dests, key: s.and_then(|s| s.key_path.clone()) }
    }

    /// Is `out`'s last line this destination's login prompt?
    pub fn matches(&self, out: &str) -> bool {
        let line = last_line(out);
        let named = |d: &String| {
            // keyboard-interactive: the server's own wording ("Password:", "Password for me@REALM:")
            let kbd = line.strip_prefix(&format!("({d}) ")).is_some_and(|p| p.contains("password") && p.ends_with(':') && !p.contains("new password") && !p.contains("retype"));
            kbd || line == format!("{d}'s password:")
        };
        if self.dests.iter().any(named) {
            return true;
        }
        let key = self.key.as_deref().map(str::to_lowercase);
        key.is_some_and(|k| line == format!("enter passphrase for key '{k}':"))
    }
}

/// The last line of terminal output, lowercase, without colours and other escape sequences (a
/// Windows console redraws its screen with them, moving the cursor instead of printing newlines).
pub fn last_line(out: &str) -> String {
    let mut text = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\x1b', Some('[')) => {
                chars.next();
                let mut end = None;
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        end = Some(c);
                        break;
                    }
                }
                // cursor moved to another line
                if matches!(end, Some('H' | 'f' | 'd' | 'E' | 'F')) {
                    text.push('\n');
                }
            }
            ('\x1b', Some(']')) => {
                // a title: ends with BEL or ESC \\
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            ('\x1b', Some(_)) => {
                chars.next();
            }
            (c, _) if c.is_control() && !matches!(c, '\n' | '\r') => {}
            (c, _) => text.push(c),
        }
    }
    text.trim_end().rsplit(['\n', '\r']).next().unwrap_or("").trim().to_lowercase()
}

/// A saved SSH session is being edited or deleted: let go of its master connection (so its old
/// login isn't reused) and stop its port forwards. No-op for other connections.
pub async fn forget_session(id: &str) {
    let Some(s) = Session::load(&format!("{SESSION_PREFIX}{id}")) else { return };
    super::ssh_forward::stop(id);
    let t = Target::for_session(s, "~");
    HOST_OS.lock().remove(&host_key(&t));
    let _ = tokio::task::spawn_blocking(move || release_master(&t)).await;
}

/// Stop accepting new commands on `t`'s shared master connection, so the next one logs in afresh
/// (after the session's host, user or password changed). Running terminals keep their connection.
pub fn release_master(t: &Target) {
    if !cfg!(unix) {
        return;
    }
    let mut args = ssh_args_with(t, false, true);
    args.extend(["-O".into(), "stop".into(), "--".into(), t.ssh_dest().to_string()]);
    let _ = std::process::Command::new("ssh").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// An `ssh` running `script` (POSIX sh) on the host with `args` as $1…; stdio not set up.
/// Keep the returned guard alive until the command has finished.
pub(crate) fn ssh_command(t: &Target, script: &str, args: &[&str]) -> (tokio::process::Command, Askpass) {
    let quoted: Vec<String> = args.iter().map(|a| sh_path(a)).collect();
    ssh_command_quoted(t, script, &quoted)
}

/// Tests: an SSH folder on this host name runs its commands with the local `sh`, as the host's
/// sshd would, so the SSH code paths (scripts, quoting, exit codes) are tested without a server.
#[cfg(test)]
pub(crate) const LOCAL_TEST_HOST: &str = "local-test.devcat.invalid";

/// `ssh_command` with `args` already quoted for the remote shell.
fn ssh_command_quoted(t: &Target, script: &str, args: &[String]) -> (tokio::process::Command, Askpass) {
    let mut remote = format!("sh -c {} devcat", sh_quote(script));
    for a in args {
        remote.push(' ');
        remote.push_str(a);
    }
    #[cfg(test)]
    if t.dest == LOCAL_TEST_HOST {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(remote).kill_on_drop(true);
        return (cmd, Askpass::default());
    }
    let (env, guard) = askpass_env(t);
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(ssh_args(t, false)).arg("--").arg(t.ssh_dest()).arg(remote);
    cmd.envs(env).kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // no console window
    }
    (cmd, guard)
}

/// ssh's own failure (exit 255) as a readable error.
pub(crate) fn connect_error(t: &Target, err: &str) -> AppError {
    let who = t.label();
    let hint = if err.contains("Host key verification failed") {
        " Open a terminal on it first to check and accept its host key."
    } else if t.session.as_ref().is_some_and(|s| s.password.is_some()) && err.contains("Permission denied") {
        " Check the saved password, or open a terminal and log in there."
    } else if err.contains("Permission denied") || err.contains("passphrase") {
        " Add your key to ssh-agent (ssh-add), save a password in the session, or open a terminal here first and log in there."
    } else {
        ""
    };
    AppError::msg(format!("Couldn't connect to {who} over SSH: {err}.{hint}"))
}

/// Run `script` (POSIX sh) on the host with `args` as $1…, optional stdin. Returns stdout.
async fn run(t: &Target, script: &str, args: &[&str], stdin: Option<&[u8]>) -> AppResult<Vec<u8>> {
    let quoted: Vec<String> = args.iter().map(|a| sh_path(a)).collect();
    run_quoted(t, script, &quoted, stdin).await
}

async fn run_quoted(t: &Target, script: &str, args: &[String], stdin: Option<&[u8]>) -> AppResult<Vec<u8>> {
    let out = run_raw(t, script, args, stdin, TIMEOUT).await?;
    if out.code == 0 {
        return Ok(out.stdout);
    }
    let err = out.stderr.trim().to_string();
    Err(AppError::msg(if err.is_empty() { format!("Command failed on {}", t.label()) } else { err }))
}

/// `script` on the host, its exit code and output as they are (ssh's own failure, exit 255, is an
/// error). `args` are quoted for the remote shell already.
async fn run_raw(t: &Target, script: &str, args: &[String], stdin: Option<&[u8]>, timeout: Duration) -> AppResult<super::git::GitOut> {
    t.ready().await?;
    let (mut cmd, _askpass) = ssh_command_quoted(t, script, args);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("Couldn't run ssh ({e}). Install the OpenSSH client.")))?;
    if let (Some(data), Some(mut w)) = (stdin, child.stdin.take()) {
        w.write_all(data).await.map_err(|e| AppError::msg(format!("ssh: {e}")))?;
        drop(w);
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| AppError::msg(format!("{} didn't answer in time", t.label())))?
        .map_err(|e| AppError::msg(format!("ssh: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.code() == Some(255) {
        return Err(connect_error(t, stderr.trim()));
    }
    Ok(super::git::GitOut { code: out.status.code().unwrap_or(-1), stdout: out.stdout, stderr })
}

/// Records separated by NUL; sections by a NUL-wrapped marker.
const SECTION: &str = "\u{1}DEVCAT\u{1}";

fn sections(out: &[u8]) -> Vec<Vec<String>> {
    let mut all = vec![Vec::new()];
    for rec in out.split(|b| *b == 0) {
        let s = String::from_utf8_lossy(rec).into_owned();
        if s == SECTION {
            all.push(Vec::new());
        } else if !s.is_empty() {
            all.last_mut().unwrap().push(s);
        }
    }
    all
}

// ---------- folder listing ----------

const LIST_SCRIPT: &str = r#"
cd "$1" 2>/dev/null || { echo "Cannot read $1 (no such folder or no permission)" >&2; exit 3; }
S=$(printf '\001DEVCAT\001')
pwd -P | tr -d '\n'; printf '\000%s\000' "$S"
if find . -maxdepth 0 -printf '' >/dev/null 2>&1; then
  find . -mindepth 1 -maxdepth 1 -printf '%Y\t%s\t%T@\t%f\000'
else
  for f in * .[!.]* ..?*; do
    [ -e "$f" ] || [ -L "$f" ] || continue
    if [ -d "$f" ]; then t=d; else t=f; fi
    m=$(stat -f '%z %m' "$f" 2>/dev/null || echo '0 0')
    printf '%s\t%s\t%s\t%s\000' "$t" "${m% *}" "${m#* }" "$f"
  done
fi
printf '%s\000' "$S"
for f in * .[!.]* ..?*; do [ -e "$f/.git" ] && printf '%s\000' "$f"; done
printf '%s\000' "$S"
if command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git rev-parse --show-prefix | tr -d '\n'; printf '\000%s\000' "$S"
  git status --porcelain=v2 -z -unormal 2>/dev/null; printf '%s\000' "$S"
  git ls-files -z --others --ignored --exclude-standard --directory . 2>/dev/null
fi
"#;

fn unix_rfc3339(secs: &str) -> Option<String> {
    let f: f64 = secs.parse().ok()?;
    chrono::DateTime::<chrono::Utc>::from_timestamp(f as i64, 0).map(|d| d.to_rfc3339())
}

pub async fn fs_list(t: &Target, show_ignored: bool) -> AppResult<Vec<FileEntry>> {
    let out = run(t, LIST_SCRIPT, &[&t.path], None).await?;
    let s = sections(&out);
    let real = s.first().and_then(|v| v.first()).cloned().unwrap_or_else(|| t.path.clone());
    let repos: std::collections::HashSet<&str> = s.get(2).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default();
    // In a git work tree there are sections 3–5; the prefix is "" at the repo root.
    let prefix = if s.len() > 4 { Some(s[3].first().cloned().unwrap_or_default()) } else { None };
    let status = s.get(4).map(|v| {
        let mut raw = Vec::new();
        for r in v {
            raw.extend_from_slice(r.as_bytes());
            raw.push(0);
        }
        StatusIndex::from_status(&parse_status_v2(&raw))
    });
    let ignored: std::collections::HashSet<String> = s
        .get(5)
        .map(|v| v.iter().filter_map(|p| p.trim_start_matches("./").split('/').next().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut entries: Vec<FileEntry> = s
        .get(1)
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|line| {
            let mut it = line.splitn(4, '\t');
            let (ty, size, mtime, name) = (it.next()?, it.next()?, it.next()?, it.next()?);
            if name == ".git" {
                return None;
            }
            let is_dir = ty == "d";
            let is_ignored = ignored.contains(name);
            if is_ignored && !show_ignored {
                return None;
            }
            let git = match (&status, &prefix) {
                (Some(idx), Some(pre)) if !is_ignored => idx.lookup(&format!("{pre}{name}"), is_dir),
                _ => None,
            };
            let path = if real.ends_with('/') { format!("{real}{name}") } else { format!("{real}/{name}") };
            Some(FileEntry {
                name: name.to_string(),
                path: t.uri(&path),
                is_dir,
                size: if is_dir { 0 } else { size.parse().unwrap_or(0) },
                modified: unix_rfc3339(mtime),
                git,
                ignored: is_ignored,
                is_repo: is_dir && repos.contains(name),
            })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

/// Check a folder on an SSH host and return it as the host sees it (`~` and symlinks
/// resolved): `ssh://devbox/~/app` → `ssh://devbox/home/me/app`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_resolve_dir(uri: String) -> AppResult<String> {
    let t = Target::parse(&uri).ok_or_else(|| AppError::msg("Not an SSH folder (ssh://host/path)"))?;
    resolve_dir(&t).await
}

pub async fn resolve_dir(t: &Target) -> AppResult<String> {
    let out = run(t, r#"cd "$1" 2>/dev/null || { echo "No folder $1 there" >&2; exit 3; }; pwd -P"#, &[&t.path], None).await?;
    Ok(t.uri(String::from_utf8_lossy(&out).trim()))
}

// ---------- files ----------

const STAT: &str = r#"stat -c '%Y %s' "$1" 2>/dev/null || stat -f '%m %z' "$1""#;

/// "mtime:size" of a remote file (the version token for saves).
async fn version(t: &Target) -> AppResult<Option<(String, u64)>> {
    let script = format!(r#"[ -f "$1" ] || exit 4; {STAT}"#);
    match run(t, &script, &[&t.path], None).await {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out);
            let mut it = s.split_whitespace();
            let (m, z) = (it.next().unwrap_or("0"), it.next().unwrap_or("0").parse().unwrap_or(0));
            Ok(Some((format!("ssh:{m}:{z}"), z)))
        }
        Err(e) if e.to_string().is_empty() || e.to_string().contains("Command failed") => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn fs_preview(t: &Target) -> AppResult<FilePreview> {
    let (ver, size) = version(t).await?.ok_or_else(|| AppError::msg(format!("{} is not a file there", t.path)))?;
    let head = size > PREVIEW_COPY_CAP;
    let data = if head {
        run(t, &format!("head -c {PREVIEW_HEAD} \"$1\""), &[&t.path], None).await?
    } else {
        run(t, r#"cat "$1""#, &[&t.path], None).await?
    };
    // Preview a local copy with the same name (the previewer goes by name and content).
    // The host names it: on Windows a `\`, `..` or drive in it would put the copy anywhere.
    let name = t.path.rsplit('/').next().unwrap_or_default().replace(['\\', '/', ':'], "_");
    let name = if super::sftp::plain_local_name(&name) { name } else { "file".to_string() };
    let dir = std::env::temp_dir().join(format!("devcat-remote-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir)?;
    let local = dir.join(&name);
    std::fs::write(&local, &data)?;
    let lp = local.to_string_lossy().into_owned();
    let res = crate::rt::spawn_blocking(move || super::files::preview_sync(&lp)).await.map_err(|e| AppError::msg(e.to_string()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut p = res??;
    p.path = t.uri(&t.path);
    p.size = size;
    p.version = Some(ver);
    if head {
        p.truncated = true;
        p.editable = false;
        p.readonly_reason = Some("Only the start of this large file was loaded".into());
    }
    Ok(p)
}

pub async fn fs_write_text(t: &Target, content: &str, expected_version: Option<&str>, eol: Option<&str>) -> AppResult<String> {
    let current = version(t).await?;
    match (&current, expected_version) {
        (Some((v, _)), Some(e)) if v != e => return Err(AppError::msg(format!("CONFLICT: {} was changed on the host since it was opened.", t.path))),
        (None, Some(_)) => return Err(AppError::msg(format!("CONFLICT: {} was deleted or moved on the host since it was opened.", t.path))),
        _ => {}
    }
    let body = super::files::normalize_eol(content, eol);
    // In place (keeps the file's owner and permissions).
    run(t, r#"cat > "$1""#, &[&t.path], Some(body.as_bytes())).await?;
    Ok(version(t).await?.map(|(v, _)| v).unwrap_or_default())
}

// ---------- git ----------

pub async fn git_status(t: &Target) -> AppResult<GitStatus> {
    let script = r#"cd "$1" 2>/dev/null || exit 3
command -v git >/dev/null 2>&1 || exit 0
r=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
printf '%s\000' "$r"; cd "$r" && git status --porcelain=v2 --branch -z -uall"#;
    let out = run(t, script, &[&t.path], None).await?;
    let Some(i) = out.iter().position(|b| *b == 0) else {
        return Ok(GitStatus { is_repo: false, root: None, branch: None, ahead: 0, behind: 0, files: vec![] });
    };
    let root = String::from_utf8_lossy(&out[..i]).into_owned();
    let st = parse_status_v2(&out[i + 1..]);
    Ok(GitStatus { is_repo: true, root: Some(t.uri(&root)), branch: st.branch(), ahead: st.ahead, behind: st.behind, files: st.files })
}

// ---------- git: the Git tab on an SSH folder ----------

/// The start of every git script: in the folder `$1` (then shifted away), with what `git_command`
/// sets here: no pager, prompts, optional locks, translated messages or fsmonitor (a repository's
/// config could run a program on `git status` with it). `G` is that git.
const GIT_PRELUDE: &str = r#"cd "$1" 2>/dev/null || { echo "No folder $1 there" >&2; exit 3; }
shift
PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
command -v git >/dev/null 2>&1 || { echo "git isn't installed on this host" >&2; exit 127; }
export GIT_OPTIONAL_LOCKS=0 GIT_PAGER=cat GIT_TERMINAL_PROMPT=0 LC_ALL=C
G() { git -c core.quotepath=false -c color.ui=false -c core.fsmonitor=false --no-pager "$@"; }
"#;

/// Long enough for a fetch / pull / push; callers pass a shorter one where they have it.
const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// The scripts are POSIX sh: a Windows host (cmd / PowerShell) can't run them.
async fn posix_host(t: &Target) -> AppResult<()> {
    if host_os(t).await == HostOs::Windows {
        return Err(AppError::msg(format!("{} is a Windows host: Git over SSH needs a Linux or macOS host for now.", t.label())));
    }
    Ok(())
}

/// `GIT_PRELUDE` + `body`, run in `t.path` with `args` ($1… in `body`, quoted here).
async fn git_script(t: &Target, body: &str, args: &[String], timeout: Duration) -> AppResult<super::git::GitOut> {
    posix_host(t).await?;
    let script = format!("{GIT_PRELUDE}{body}");
    let mut q = vec![sh_path(&t.path)];
    q.extend(args.iter().map(|a| sh_quote(a)));
    run_raw(t, &script, &q, None, timeout).await
}

/// `git args…` in `t.path` on the host, `env` exported first (names are DevCat's own constants).
pub(crate) async fn git(t: &Target, args: &[String], env: &[(&str, String)], timeout: Option<Duration>) -> AppResult<super::git::GitOut> {
    let mut body = String::new();
    for (k, v) in env {
        body.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    body.push_str("G \"$@\"\n");
    git_script(t, &body, args, timeout.unwrap_or(GIT_TIMEOUT)).await
}

/// A work-tree file `rel` (under `t.path`) as git sees it: its bytes, or a symlink's target;
/// None when it's missing or not a file. Larger than `cap`: an error naming its size.
pub(crate) async fn worktree_read(t: &Target, rel: &str, cap: u64) -> AppResult<Option<Vec<u8>>> {
    let body = r#"f=$1
if [ -L "$f" ]; then printf L; readlink "$f" | tr -d '\n'
elif [ -f "$f" ]; then
  z=$(wc -c < "$f" | tr -d ' ')
  if [ "$z" -gt "$2" ]; then printf 'B%s' "$z"; else printf F; cat "$f"; fi
else printf N; fi"#;
    let out = git_script(t, body, &[rel.to_string(), cap.to_string()], TIMEOUT).await?;
    if out.code != 0 {
        return Err(AppError::msg(out.stderr.trim().to_string()));
    }
    let (kind, rest) = out.stdout.split_first().map(|(k, r)| (*k, r.to_vec())).unwrap_or((b'N', vec![]));
    match kind {
        b'L' | b'F' => Ok(Some(rest)),
        b'B' => {
            let n: u64 = String::from_utf8_lossy(&rest).trim().parse().unwrap_or(0);
            Err(AppError::msg(format!("File too large to diff ({} MB)", n / (1024 * 1024))))
        }
        _ => Ok(None),
    }
}

/// Whether `rel` (under `t.path`) is a file on the host (following symlinks, like `Path::is_file`).
pub(crate) async fn worktree_is_file(t: &Target, rel: &str) -> AppResult<bool> {
    Ok(git_script(t, r#"[ -f "$1" ]"#, &[rel.to_string()], TIMEOUT).await?.code == 0)
}

/// Delete the file or symlink `rel` (under `t.path`); anything else is left alone.
pub(crate) async fn worktree_remove(t: &Target, rel: &str) -> AppResult<()> {
    let out = git_script(t, r#"if [ -L "$1" ] || [ -f "$1" ]; then rm -f -- "$1"; fi"#, &[rel.to_string()], TIMEOUT).await?;
    if out.code != 0 {
        return Err(AppError::msg(out.stderr.trim().to_string()));
    }
    Ok(())
}

/// The operation in progress in the repository at `t.path`, as `gitx::commands::operation` tells.
pub(crate) async fn git_operation(t: &Target) -> AppResult<Option<String>> {
    let body = r#"d=$(G rev-parse --git-dir) || exit 1
if [ -d "$d/rebase-merge" ]; then echo rebase
elif [ -d "$d/rebase-apply" ]; then if [ -e "$d/rebase-apply/applying" ]; then echo am; else echo rebase; fi
elif [ -f "$d/MERGE_HEAD" ]; then echo merge
elif [ -f "$d/CHERRY_PICK_HEAD" ]; then echo cherry-pick
elif [ -f "$d/REVERT_HEAD" ]; then echo revert
fi"#;
    let out = git_script(t, body, &[], TIMEOUT).await?;
    if out.code != 0 {
        return Err(AppError::msg(out.stderr.trim().to_string()));
    }
    let op = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!op.is_empty()).then_some(op))
}

/// New-file diffs of untracked `files` (under `t.path`), in one go: per file its diff, or None
/// when it's larger than `cap`. Stops once about `total` bytes are out.
pub(crate) async fn untracked_diffs(t: &Target, files: &[String], cap: u64, total: usize) -> AppResult<Vec<Option<Vec<u8>>>> {
    let body = r#"S=$(printf '\001DEVCAT\001'); cap=$1; total=$2; shift 2; sum=0
for f in "$@"; do
  [ "$sum" -gt "$total" ] && break
  printf '%s\000' "$S"
  z=$(wc -c < "$f" 2>/dev/null | tr -d ' ')
  if [ "${z:-0}" -gt "$cap" ]; then printf 'BIG\000'; continue; fi
  sum=$((sum + ${z:-0}))
  printf 'OK\000'
  G diff --no-color --no-ext-diff --no-index -- /dev/null "$f"; c=$?
  [ "$c" -gt 1 ] && exit "$c"
  printf '\000'
done"#;
    let mut args = vec![cap.to_string(), total.to_string()];
    args.extend(files.iter().cloned());
    let out = git_script(t, body, &args, GIT_TIMEOUT).await?;
    if out.code != 0 {
        return Err(AppError::msg(out.stderr.trim().to_string()));
    }
    Ok(sections(&out.stdout)
        .into_iter()
        .skip(1)
        .map(|sec| match sec.first().map(String::as_str) {
            Some("BIG") => None,
            _ => Some(sec[1..].join("\0").into_bytes()),
        })
        .collect())
}

/// A repository `git_scan` found: its path on the host, whether `.git` is a file, whether it's
/// the one enclosing the scanned folder, and its `git status --porcelain=v2 --branch -z`.
pub(crate) struct ScannedRepo {
    pub path: String,
    pub is_submodule: bool,
    pub enclosing: bool,
    pub status: super::git::GitOut,
}

/// The repositories under `t.path` down to `depth` (and the one enclosing it), with their status,
/// in one go. Skips hidden folders and `skip` (node_modules…) as the local scan does. Returns the
/// scanned folder as the host sees it.
pub(crate) async fn git_scan(t: &Target, depth: u32, skip: &[&str]) -> AppResult<(String, Vec<ScannedRepo>)> {
    let prune: String = skip.iter().map(|n| format!(" -o -name {}", sh_quote(n))).collect();
    let body = format!(
        r#"S=$(printf '\001DEVCAT\001'); E=$(printf '\002')
root=$(pwd -P); printf '%s\000' "$root"
tmp=$(mktemp 2>/dev/null || echo "/tmp/devcat-git.$$"); trap 'rm -f "$tmp"' EXIT
emit() {{
  printf '%s\000%s\t%s\000' "$S" "$1" "$2"
  ( cd "$2" && G status --porcelain=v2 --branch -z --untracked-files=normal ) 2>"$tmp"; c=$?
  printf '\000%s%s\t' "$E" "$c"; tr -d '\000' < "$tmp"; printf '\000'
}}
kind() {{ if [ -d "$1/.git" ] && [ ! -L "$1/.git" ]; then echo dir; elif [ -f "$1/.git" ]; then echo file; fi; }}
if [ -z "$(kind .)" ]; then
  top=$(G rev-parse --show-toplevel 2>/dev/null)
  if [ -n "$top" ] && [ "$top" != "$root" ]; then k=$(kind "$top"); emit "enclosing-${{k:-dir}}" "$top"; fi
fi
find . -maxdepth "$(($1 + 1))" \( -name .git \( -type d -o -type f \) -print -prune \) -o \( -type d ! -name . \( -name '.*'{prune} \) -prune \) 2>/dev/null |
while IFS= read -r g; do
  d=${{g%/.git}}; [ "$d" = . ] && p="$root" || p="$root/${{d#./}}"
  if [ -f "$g" ]; then emit file "$p"; else emit dir "$p"; fi
done"#
    );
    let out = git_script(t, &body, &[depth.to_string()], Duration::from_secs(120)).await?;
    if out.code != 0 {
        return Err(AppError::msg(out.stderr.trim().to_string()));
    }
    let mut secs = sections(&out.stdout).into_iter();
    let root = secs.next().and_then(|v| v.into_iter().next()).unwrap_or_else(|| t.path.clone());
    let repos = secs
        .filter_map(|sec| {
            let (head, rest) = sec.split_first()?;
            let (kind, path) = head.split_once('\t')?;
            let end = rest.iter().rposition(|r| r.starts_with('\u{2}'))?;
            let (code, stderr) = rest[end][1..].split_once('\t').unwrap_or((&rest[end][1..], ""));
            let mut stdout = Vec::new();
            for r in &rest[..end] {
                stdout.extend_from_slice(r.as_bytes());
                stdout.push(0);
            }
            Some(ScannedRepo {
                path: path.to_string(),
                is_submodule: kind.ends_with("file"),
                enclosing: kind.starts_with("enclosing"),
                status: super::git::GitOut { code: code.parse().unwrap_or(-1), stdout, stderr: stderr.to_string() },
            })
        })
        .collect();
    Ok((root, repos))
}

// ---------- Claude Code transcripts ----------

/// Claude Code's transcripts for the folder `$1` on the host (`$2`: its encoded project-dir name
/// as spelled here, may be empty). Prints the folder as the host sees it (`pwd -P`, `pwd`), then
/// one section per session file: `name\tmtime size\tcount`, and for a file not listed on stdin
/// (`name mtime size` lines: unchanged since last time, count `=`) its first 128 KB, its title /
/// cost lines and its last 256 KB, or all of it when small. Transcripts run to tens of MB, so they
/// are never copied whole; lines cut off at those edges just don't parse.
const SESSIONS_SCRIPT: &str = r#"
cd "$1" 2>/dev/null || { echo "No folder $1 there" >&2; exit 3; }
known=$(cat)
S=$(printf '\001DEVCAT\001')
root="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects"
enc() { printf '%s' "$1" | sed 's/[^a-zA-Z0-9]/-/g'; }
real=$(pwd -P); here=$(pwd)
printf '%s\000%s\000' "$real" "$here"
[ -d "$root" ] || exit 0
seen=
for n in "$2" "$(enc "$real")" "$(enc "$here")"; do
  [ -n "$n" ] || continue
  case " $seen " in *" $n "*) continue ;; esac
  seen="$seen $n"
  [ -d "$root/$n" ] || continue
  for f in "$root/$n"/*.jsonl; do
    [ -f "$f" ] || continue
    m=$(stat -c '%Y %s' "$f" 2>/dev/null || stat -f '%m %z' "$f" 2>/dev/null) || continue
    name=${f##*/}
    case "
$known
" in *"
$name $m
"*) printf '%s\000%s\t%s\t=\000' "$S" "$name" "$m"; continue ;; esac
    c=$(grep -E '"type":"(user|assistant)"' "$f" | grep -vc '"isSidechain":true')
    printf '%s\000%s\t%s\t%s\000' "$S" "$name" "$m" "${c:-0}"
    if [ "${m#* }" -le 393216 ]; then
      cat "$f"
    else
      head -c 131072 "$f"; printf '\n'
      grep -E '"type":"(custom-title|ai-title|summary|last-prompt|cost-state)"' "$f"
      tail -c 262144 "$f"
    fi
    printf '\000'
  done
done
"#;

/// One session file from `claude_transcripts`.
pub(crate) struct Transcript {
    /// `<session id>.jsonl`
    pub name: String,
    /// "mtime size" on the host (unchanged when equal).
    pub version: String,
    pub mtime: Option<String>,
    /// user / assistant messages; None when the host said it's unchanged (no `lines` either).
    pub count: Option<usize>,
    pub lines: Vec<String>,
}

/// The folder as the host sees it (resolved, as typed) and its Claude Code transcripts. `known`:
/// files already read ("name mtime size"), which come back without their lines.
pub(crate) async fn claude_transcripts(t: &Target, known: &[String]) -> AppResult<(Vec<String>, Vec<Transcript>)> {
    t.ready().await?;
    if host_os(t).await == HostOs::Windows {
        return Err(AppError::msg(format!("{} is a Windows host: its Claude Code sessions can't be listed yet.", t.label())));
    }
    // Spelled as Claude Code does (one `-` per character); the host's sed may count bytes.
    let encoded = if t.path.starts_with('/') { super::sessions::encode_cwd(t.path.trim_end_matches('/')) } else { String::new() };
    let mut stdin = known.join("\n");
    stdin.push('\n');
    let out = run(t, SESSIONS_SCRIPT, &[&t.path, &encoded], Some(stdin.as_bytes())).await?;
    Ok(parse_transcripts(&out))
}

/// `SESSIONS_SCRIPT` run by the local `sh` (tests: this computer stands in for the host).
#[cfg(all(test, unix))]
pub(crate) fn transcripts_here(dir: &str, encoded: &str, known: &[String], config_dir: &std::path::Path) -> (Vec<String>, Vec<Transcript>) {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .args(["-c", SESSIONS_SCRIPT, "devcat", dir, encoded])
        .env("CLAUDE_CONFIG_DIR", config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{}", known.join("\n")).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    parse_transcripts(&out.stdout)
}

fn parse_transcripts(out: &[u8]) -> (Vec<String>, Vec<Transcript>) {
    let mut s = sections(out).into_iter();
    let dirs = s.next().unwrap_or_default();
    let files = s
        .filter_map(|sec| {
            let mut it = sec.into_iter();
            let head = it.next()?;
            let mut f = head.splitn(3, '\t');
            let (name, version, count) = (f.next()?.to_string(), f.next()?.to_string(), f.next()?);
            let mtime = version.split(' ').next().and_then(unix_rfc3339);
            let count = if count == "=" { None } else { Some(count.trim().parse().unwrap_or(0)) };
            let lines = it.flat_map(|body| body.lines().map(str::to_string).collect::<Vec<_>>()).collect();
            Some(Transcript { name, version, mtime, count, lines })
        })
        .collect();
    (dirs, files)
}

// ---------- sessions ----------

/// Log in to a (possibly unsaved) session: "user on host (Linux 6.8 x86_64)".
pub async fn test_session(c: &ConnectionConfig) -> AppResult<String> {
    let mut s = Session::from_config(c)?;
    if s.id.is_empty() {
        s.id = "test".into();
    }
    let t = Target::for_session(s, "~");
    // A master left by the session's old settings would answer for the new ones.
    release_master(&t);
    let out = run(&t, r#"printf '%s on %s (%s)' "$(id -un)" "$(hostname)" "$(uname -srm)""#, &[], None).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

// ---------- staged files (pasted images, dropped files) ----------

/// On the host: `~/.cache/devcat/paste`, private, files older than a day removed. Prints the
/// file's absolute path.
const STAGE_SCRIPT: &str = r#"d="$HOME/.cache/devcat/paste"
mkdir -p "$d" && chmod 700 "$d" || exit 3
find "$d" -type f -mtime +1 -exec rm -f {} + 2>/dev/null
f="$d/$1"; cat > "$f" && printf '%s' "$f""#;

/// A file name safe for the host: letters, digits, `._-` (others → `_`), and unique.
fn stage_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let safe = safe.trim_start_matches('.');
    let safe = if safe.is_empty() { "file" } else { safe };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let rand = &uuid::Uuid::new_v4().simple().to_string()[..6];
    format!("{stamp}-{rand}-{safe}")
}

/// Copy `bytes` to the host's paste folder; returns the path there (for typing into a terminal).
pub async fn stage_bytes(t: &Target, name: &str, bytes: &[u8]) -> AppResult<String> {
    let out = run(t, STAGE_SCRIPT, &[&stage_name(name)], Some(bytes)).await?;
    let path = String::from_utf8_lossy(&out).trim().to_string();
    if path.is_empty() {
        return Err(AppError::msg(format!("Couldn't save the file on {}", t.label())));
    }
    Ok(path)
}

// ---------- file management (SFTP tab) ----------

/// Create a folder (and its parents).
pub async fn mkdir(t: &Target) -> AppResult<()> {
    run(t, r#"mkdir -p -- "$1""#, &[&t.path], None).await.map(|_| ())
}

/// Rename / move `t.path` to `to` (an absolute path on the same host). Refuses to overwrite.
pub async fn rename(t: &Target, to: &str) -> AppResult<()> {
    run(t, r#"[ -e "$2" ] && { echo "$2 already exists" >&2; exit 3; }; mv -- "$1" "$2""#, &[&t.path, to], None).await.map(|_| ())
}

/// Delete files and folders (recursively).
pub async fn remove(t: &Target, paths: &[String]) -> AppResult<()> {
    if paths.iter().any(|p| matches!(p.trim_end_matches('/'), "" | "~" | "/" | "." | "..")) {
        return Err(AppError::msg("Refusing to delete a root or home folder"));
    }
    let args: Vec<&str> = paths.iter().map(String::as_str).collect();
    // Also as the host resolves them (`/home/me` is home too).
    let script = r#"h=$(cd ~ && pwd -P)
for a in "$@"; do
  r=$(cd "$a" 2>/dev/null && pwd -P)
  if [ "$r" = "/" ] || [ "$r" = "$h" ]; then echo "Refusing to delete $a: it's the root or home folder" >&2; exit 3; fi
done
rm -rf -- "$@""#;
    run(t, script, &args, None).await.map(|_| ())
}

/// One file or folder under a folder being downloaded.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeItem {
    /// Relative to the listed root's parent (`root/sub/file`).
    pub rel: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Everything under `t.path` (itself included), for a download: folders and regular files.
pub async fn list_tree(t: &Target) -> AppResult<Vec<TreeItem>> {
    let script = r#"[ -e "$1" ] || { echo "$1 doesn't exist" >&2; exit 3; }
cd "$(dirname -- "$1")" || exit 3
b=$(basename -- "$1")
if find . -maxdepth 0 -printf '' >/dev/null 2>&1; then
  find "$b" \( -type d -o -type f \) -printf '%y\t%s\t%p\000'
else
  find "$b" \( -type d -o -type f \) -exec sh -c 'for f; do
    if [ -d "$f" ]; then printf "d\t0\t%s\000" "$f"
    else printf "f\t%s\t%s\000" "$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f")" "$f"; fi
  done' sh {} +
fi"#;
    let out = run(t, script, &[&t.path], None).await?;
    Ok(parse_tree(&out))
}

fn parse_tree(out: &[u8]) -> Vec<TreeItem> {
    out.split(|b| *b == 0)
        .filter_map(|rec| {
            let s = String::from_utf8_lossy(rec);
            let mut it = s.splitn(3, '\t');
            let (ty, size, rel) = (it.next()?, it.next()?, it.next()?);
            Some(TreeItem { rel: rel.to_string(), is_dir: ty == "d", size: size.parse().unwrap_or(0) })
        })
        .collect()
}

// ---------- terminals ----------

/// The shell a host runs remote commands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    /// sh / bash / zsh…: Linux, macOS, BSD.
    Posix,
    /// Windows OpenSSH: the command goes to cmd or PowerShell.
    Windows,
}

/// Hosts already probed (`dest:port` → Windows?), for the app's lifetime.
static HOST_OS: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<String, HostOs>>> = once_cell::sync::Lazy::new(Default::default);

fn host_key(t: &Target) -> String {
    format!("{}:{}", t.dest, t.port.unwrap_or(0))
}

/// Whether a terminal here needs a remote command at all (see `terminal_args`): a plain shell in
/// the home folder doesn't, and then the host's OS doesn't matter.
pub fn needs_command(t: &Target, program: Option<&str>, env: &[(String, String)]) -> bool {
    let startup = t.session.as_ref().and_then(|s| s.startup.as_deref());
    !(program.is_none_or(str::is_empty) && env.is_empty() && startup.is_none() && matches!(t.path.as_str(), "~" | "~/"))
}

/// What the host runs commands in. One `echo $env:OS %OS%` tells them apart: cmd expands `%OS%`,
/// PowerShell `$env:OS` (both to `Windows_NT`), sh neither. Cached once known; when the probe
/// can't log in (no key, agent or saved password: the user types it in the terminal) it's
/// `Posix`, as before, and asked again next time.
pub async fn host_os(t: &Target) -> HostOs {
    #[cfg(test)]
    if t.dest == LOCAL_TEST_HOST {
        return HostOs::Posix;
    }
    let key = host_key(t);
    if let Some(os) = HOST_OS.lock().get(&key) {
        return *os;
    }
    let (env, _askpass) = askpass_env(t);
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(ssh_args(t, false)).arg("--").arg(t.ssh_dest()).arg("echo $env:OS %OS%");
    cmd.envs(env).kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await else { return HostOs::Posix };
    if out.status.code() == Some(255) {
        return HostOs::Posix;
    }
    let os = if String::from_utf8_lossy(&out.stdout).contains("Windows_NT") { HostOs::Windows } else { HostOs::Posix };
    HOST_OS.lock().insert(key, os);
    os
}

/// `ssh` arguments for a terminal in `t.path`: a login shell, or `program args` (e.g. claude).
/// `env` is exported first on the host; `forward` = (remote port, local port) for -R.
/// A session's startup command runs before its login shell.
pub fn terminal_args(t: &Target, os: HostOs, program: Option<&str>, args: &[String], env: &[(String, String)], forward: Option<(u16, u16)>) -> Vec<String> {
    let mut a = vec!["-tt".to_string()];
    a.extend(ssh_args(t, true));
    if let Some((rp, lp)) = forward {
        a.extend(["-o".into(), "ExitOnForwardFailure=no".into(), "-R".into(), format!("127.0.0.1:{rp}:127.0.0.1:{lp}")]);
    }
    a.push("--".into());
    a.push(t.ssh_dest().to_string());
    // A plain shell in the home folder: no remote command, so the host starts the account's own
    // login shell, whatever it is.
    if !needs_command(t, program, env) {
        return a;
    }
    let startup = t.session.as_ref().and_then(|s| s.startup.as_deref());
    let program = program.filter(|p| !p.is_empty());
    a.push(match os {
        HostOs::Posix => posix_command(&t.path, startup, program, args, env),
        HostOs::Windows => powershell_command(&t.path, startup, program, args, env),
    });
    a
}

fn posix_command(path: &str, startup: Option<&str>, program: Option<&str>, args: &[String], env: &[(String, String)]) -> String {
    let mut inner = String::new();
    for (k, v) in env {
        inner.push_str(&format!("export {k}={}; ", sh_quote(v)));
    }
    inner.push_str(&format!("cd {} || exit 1; ", sh_path(path)));
    match program {
        None => {
            if let Some(cmd) = startup {
                inner.push_str(cmd);
                inner.push_str("; ");
            }
            inner.push_str("exec \"${SHELL:-/bin/sh}\" -l")
        }
        Some(p) => {
            inner.push_str("exec ");
            inner.push_str(&sh_quote(p));
            for a in args {
                inner.push(' ');
                inner.push_str(&sh_quote(a));
            }
        }
    }
    // Through a login shell, so the PATH from the user's profile (~/.local/bin, nvm…) applies.
    format!("exec \"${{SHELL:-/bin/sh}}\" -lc {}", sh_quote(&inner))
}

/// Single-quote for PowerShell (it also ends a string at curly single quotes: doubled too).
fn ps_quote(s: &str) -> String {
    let mut q = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            q.push(c);
        }
        q.push(c);
    }
    q.push('\'');
    q
}

/// A path for PowerShell: `~` / `~/x` from $HOME, `/C:/x` (as written in an ssh:// URI) → `C:/x`.
fn ps_path(p: &str) -> String {
    if p == "~" || p == "~/" {
        return "$HOME".into();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return format!("(Join-Path $HOME {})", ps_quote(rest));
    }
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    ps_quote(if drive { &p[1..] } else { p })
}

/// A Windows host: the same terminal as a PowerShell script, sent as `-EncodedCommand` (base64 of
/// UTF-16LE), so nothing in it needs quoting for cmd or PowerShell, whichever runs the command.
/// The session's startup command is the user's own, in PowerShell there.
fn powershell_command(path: &str, startup: Option<&str>, program: Option<&str>, args: &[String], env: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in env {
        s.push_str(&format!("$env:{k} = {}; ", ps_quote(v)));
    }
    s.push_str(&format!("try {{ Set-Location -LiteralPath {} -ErrorAction Stop }} catch {{ Write-Host $_; exit 1 }}; ", ps_path(path)));
    let keep = match program {
        None => {
            if let Some(cmd) = startup {
                s.push_str(cmd);
                s.push_str("; ");
            }
            true
        }
        Some(p) => {
            s.push_str("& ");
            s.push_str(&ps_quote(p));
            for a in args {
                s.push(' ');
                s.push_str(&ps_quote(a));
            }
            s.push_str("; exit $LASTEXITCODE");
            false
        }
    };
    let utf16: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    format!("powershell -NoLogo{} -EncodedCommand {encoded}", if keep { " -NoExit" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        let t = Target::parse("ssh://me@box.example.com:2222/home/me/app").unwrap();
        assert_eq!((t.dest.as_str(), t.port, t.path.as_str()), ("me@box.example.com", Some(2222), "/home/me/app"));
        assert_eq!(t.uri("/home/me/app/src"), "ssh://me@box.example.com:2222/home/me/app/src");
        let a = Target::parse("ssh://devbox/~/work").unwrap();
        assert_eq!((a.dest.as_str(), a.port, a.path.as_str()), ("devbox", None, "~/work"));
        assert_eq!(Target::parse("ssh://devbox").unwrap().path, "/");
        assert!(Target::parse("ssh://-oProxyCommand=x/").is_none());
        assert!(Target::parse("/Users/me").is_none());
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_path("~/a b"), "\"$HOME\"/'a b'");
        let t = Target::parse("ssh://devbox/srv/app").unwrap();
        let a = terminal_args(&t, HostOs::Posix, Some("claude"), &["--resume".into(), "x y".into()], &[], None);
        assert_eq!(a[0], "-tt");
        assert_eq!(a[a.len() - 2], "devbox");
        assert!(a.last().unwrap().contains("exec 'claude' '--resume' 'x y'") || a.last().unwrap().contains("claude"), "{a:?}");
        // A plain shell at home sends no remote command (Windows hosts can't run the POSIX one).
        let home = Target::parse("ssh://devbox/~").unwrap();
        assert_eq!(terminal_args(&home, HostOs::Windows, None, &[], &[], None).last().unwrap(), "devbox");
        // Anywhere else it still goes through the login shell with a cd.
        assert!(terminal_args(&t, HostOs::Posix, None, &[], &[], None).last().unwrap().contains("/srv/app"));
    }

    #[test]
    fn powershell_for_windows_hosts() {
        let decode = |cmd: &str| {
            use base64::Engine;
            let b64 = cmd.rsplit(' ').next().unwrap();
            let b = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            String::from_utf16(&b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap()
        };
        assert_eq!(ps_quote("it's ‘x’"), "'it''s ‘‘x’’'");
        assert_eq!(ps_path("~"), "$HOME");
        assert_eq!(ps_path("~/a b"), "(Join-Path $HOME 'a b')");
        assert_eq!(ps_path("/C:/work"), "'C:/work'");
        assert_eq!(ps_path("/D:\\x"), "'D:\\x'");

        let t = Target::parse("ssh://winbox/C:/work").unwrap();
        let shell = terminal_args(&t, HostOs::Windows, None, &[], &[], None);
        let cmd = shell.last().unwrap();
        assert!(cmd.starts_with("powershell -NoLogo -NoExit -EncodedCommand "), "{cmd}");
        assert!(!cmd.contains('$') && !cmd.contains('\''), "{cmd}");
        assert_eq!(decode(cmd), "try { Set-Location -LiteralPath 'C:/work' -ErrorAction Stop } catch { Write-Host $_; exit 1 }; ");

        let home = Target::parse("ssh://winbox/~").unwrap();
        let cmd = terminal_args(&home, HostOs::Windows, Some("claude"), &["--resume".into(), "it's".into()], &[("DEVCAT_X".into(), "v".into())], None);
        let cmd = cmd.last().unwrap();
        assert!(cmd.starts_with("powershell -NoLogo -EncodedCommand "), "{cmd}");
        assert_eq!(
            decode(cmd),
            "$env:DEVCAT_X = 'v'; try { Set-Location -LiteralPath $HOME -ErrorAction Stop } catch { Write-Host $_; exit 1 }; & 'claude' '--resume' 'it''s'; exit $LASTEXITCODE"
        );
    }

    /// End to end against a real host: DEVCAT_SSH_TEST=ssh://host/scratch/dir (it's wiped).
    #[tokio::test]
    #[ignore]
    async fn against_a_host() {
        let Ok(uri) = std::env::var("DEVCAT_SSH_TEST") else { return };
        let t = Target::parse(&uri).unwrap();
        let setup = r#"rm -rf "$1" && mkdir -p "$1/src" "$1/node_modules/x" && cd "$1" && git init -q . && printf 'node_modules/\n' > .gitignore
printf 'hello\n' > README.md && printf 'fn main() {}\n' > src/main.rs && git add -A && git -c user.email=t@t -c user.name=t commit -qm init && printf 'changed\n' >> README.md"#;
        run(&t, setup, &[&t.path], None).await.unwrap();

        let dir = Target::parse(&resolve_dir(&t).await.unwrap()).unwrap();
        let list = fs_list(&dir, false).await.unwrap();
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", ".gitignore", "README.md"], "{list:?}");
        assert_eq!(list.iter().find(|e| e.name == "README.md").unwrap().git.as_deref(), Some("M"));
        assert!(fs_list(&dir, true).await.unwrap().iter().any(|e| e.name == "node_modules" && e.ignored));

        let file = dir.with_path(&format!("{}/README.md", dir.path));
        let p = fs_preview(&file).await.unwrap();
        assert_eq!((p.kind.as_str(), p.text.as_deref()), ("markdown", Some("hello\nchanged\n")), "{p:?}");
        let v = fs_write_text(&file, "new text\n", p.version.as_deref(), None).await.unwrap();
        assert!(fs_write_text(&file, "x", p.version.as_deref(), None).await.unwrap_err().to_string().starts_with("CONFLICT:"));
        assert_ne!(v, p.version.unwrap());
        assert_eq!(fs_preview(&file).await.unwrap().text.as_deref(), Some("new text\n"));

        let g = git_status(&dir).await.unwrap();
        assert!(g.is_repo && g.root.as_deref().unwrap().starts_with("ssh://"), "{g:?}");
        assert!(g.files.iter().any(|f| f.path == "README.md"));

        let a = terminal_args(&dir, HostOs::Posix, Some("sh"), &["-c".into(), "pwd; echo $DEVCAT_X".into()], &[("DEVCAT_X".into(), "it's set".into())], None);
        let out = tokio::process::Command::new("ssh").args(&a).output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(&dir.path) && text.contains("it's set"), "{text}");
    }

    #[test]
    fn splits_sections() {
        let out = format!("/home/me\0{SECTION}\0d\t0\t1.5\tsrc\0f\t12\t2\ta b.txt\0{SECTION}\0src\0");
        let s = sections(out.as_bytes());
        assert_eq!(s[0], vec!["/home/me"]);
        assert_eq!(s[1].len(), 2);
        assert_eq!(s[2], vec!["src"]);
    }
}
