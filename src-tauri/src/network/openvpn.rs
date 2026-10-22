//! OpenVPN: launched elevated as a daemon, then driven over its management interface
//! (127.0.0.1, password protected): credentials, state changes and the log stream.
use super::registry::{self, Run};
use super::store::{NetStore, write_private};
use super::helper::{self, Outcome};
use super::{NetKind, NetProfile, elevate, tools};
use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------- management protocol

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Command to write to the management socket (may contain secrets: never log it).
    Send(String),
    Log(String),
    Connecting(String),
    Connected(String),
    Error(String),
    /// Cumulative bytes (in, out) from `>BYTECOUNT`.
    Bytes(u64, u64),
}

const STATES: &[&str] = &[
    "CONNECTING", "WAIT", "AUTH", "GET_CONFIG", "ASSIGN_IP", "ADD_ROUTES", "CONNECTED", "RECONNECTING", "EXITING",
    "RESOLVE", "TCP_CONNECT", "AUTH_PENDING",
];

/// Quote a value for the management protocol: `"…"` with `\` and `"` escaped.
/// Line breaks would end the command, so they are dropped.
pub fn mgmt_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' | '\n' => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Reacts to management-interface lines.
pub struct Machine {
    user: Option<String>,
    pass: Option<String>,
    /// A terminal error was reported; suppress "connecting" updates until connected again.
    failed: bool,
    fatal: Option<String>,
}

impl Machine {
    pub fn new(user: Option<String>, pass: Option<String>) -> Self {
        Self { user: user.filter(|u| !u.is_empty()), pass: pass.filter(|p| !p.is_empty()), failed: false, fatal: None }
    }

    fn fail(&mut self, msg: String, stop: bool) -> Vec<Action> {
        self.failed = true;
        let mut a = vec![Action::Error(msg)];
        if stop {
            a.push(Action::Send("signal SIGTERM".into()));
        }
        a
    }

    /// Error message for an unexpected exit of the process.
    pub fn exit_message(&self) -> String {
        match &self.fatal {
            Some(f) => format!("OpenVPN exited: {f}"),
            None => "OpenVPN exited unexpectedly (see the log)".into(),
        }
    }

    pub fn has_failed(&self) -> bool {
        self.failed
    }

    pub fn on_line(&mut self, line: &str) -> Vec<Action> {
        let mut line = line.trim_end_matches(['\r', '\n']);
        while let Some(rest) = line.strip_prefix("ENTER PASSWORD:") {
            line = rest;
        }
        if line.is_empty() {
            return vec![];
        }
        if let Some(rest) = line.strip_prefix(">LOG:") {
            let mut f = rest.splitn(3, ',');
            let (_ts, flags, msg) = (f.next(), f.next().unwrap_or(""), f.next().unwrap_or(rest));
            if flags.contains('F') {
                self.fatal = Some(msg.to_string());
            }
            return vec![Action::Log(msg.to_string())];
        }
        if let Some(rest) = line.strip_prefix(">STATE:") {
            let fields: Vec<&str> = rest.split(',').collect();
            return self.on_state(&fields);
        }
        if line.starts_with(">HOLD:") {
            return vec![Action::Send("hold release".into())];
        }
        if let Some(rest) = line.strip_prefix(">PASSWORD:") {
            return self.on_password(rest);
        }
        if let Some(rest) = line.strip_prefix(">FATAL:") {
            self.fatal = Some(rest.to_string());
            return self.fail(rest.to_string(), false);
        }
        if let Some(rest) = line.strip_prefix(">INFO:") {
            return vec![Action::Log(rest.to_string())];
        }
        if let Some(rest) = line.strip_prefix(">BYTECOUNT:") {
            let mut f = rest.split(',').map(|x| x.trim().parse::<u64>());
            if let (Some(Ok(rx)), Some(Ok(tx))) = (f.next(), f.next()) {
                return vec![Action::Bytes(rx, tx)];
            }
            return vec![];
        }
        if line.starts_with(">PKCS11ID") {
            return vec![];
        }
        if let Some(rest) = line.strip_prefix('>') {
            return vec![Action::Log(rest.to_string())];
        }
        if line.starts_with("SUCCESS:") || line == "END" {
            return vec![];
        }
        if let Some(rest) = line.strip_prefix("ERROR:") {
            return vec![Action::Log(format!("management: {}", rest.trim()))];
        }
        // History dumped by `log on all` / `state on all`: plain CSV lines.
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() >= 2 && fields[0].chars().all(|c| c.is_ascii_digit()) {
            if STATES.contains(&fields[1]) {
                return self.on_state(&fields);
            }
            if fields.len() >= 3 {
                return vec![Action::Log(fields[2..].join(","))];
            }
        }
        vec![Action::Log(line.to_string())]
    }

    /// `time,STATE,desc,local_ip,remote_ip,remote_port,…`
    fn on_state(&mut self, f: &[&str]) -> Vec<Action> {
        let name = f.get(1).copied().unwrap_or("");
        let desc = f.get(2).copied().unwrap_or("");
        match name {
            "CONNECTED" => {
                self.failed = false;
                let local = f.get(3).copied().unwrap_or("");
                let remote = match (f.get(4).copied().unwrap_or(""), f.get(5).copied().unwrap_or("")) {
                    ("", _) => String::new(),
                    (h, "") => h.to_string(),
                    (h, p) => format!("{h}:{p}"),
                };
                let mut detail = match (local.is_empty(), remote.is_empty()) {
                    (false, false) => format!("{local} ({remote})"),
                    (false, true) => local.to_string(),
                    (true, false) => remote,
                    (true, true) => "connected".into(),
                };
                if desc == "ERROR" {
                    detail.push_str(" — with errors, see the log");
                }
                vec![Action::Connected(detail)]
            }
            "EXITING" => {
                if self.failed {
                    vec![]
                } else if desc == "auth-failure" {
                    self.fail("Authentication failed (check the username/password)".into(), false)
                } else {
                    let msg = self.fatal.clone().unwrap_or_else(|| format!("OpenVPN exited ({desc})"));
                    self.fail(msg, false)
                }
            }
            _ if self.failed => vec![],
            "RECONNECTING" => vec![Action::Connecting(format!("reconnecting ({desc})"))],
            other => vec![Action::Connecting(other.to_ascii_lowercase().replace('_', " "))],
        }
    }

    fn on_password(&mut self, rest: &str) -> Vec<Action> {
        if let Some(r) = rest.strip_prefix("Verification Failed") {
            if r.contains("CRV1") {
                return self.fail("The server requires a challenge/response (2FA), which DevCat does not support yet".into(), true);
            }
            let what = r.split('\'').nth(1).unwrap_or("Auth");
            let msg = if what == "Auth" {
                "Authentication failed (check the username/password)".to_string()
            } else {
                format!("Password verification failed: {what}")
            };
            return self.fail(msg, true);
        }
        let Some(r) = rest.strip_prefix("Need '") else { return vec![] };
        let Some((kind, tail)) = r.split_once('\'') else { return vec![] };
        if tail.contains("SC:") {
            return self.fail("The server requires a one-time code (static challenge), which DevCat does not support yet".into(), true);
        }
        if tail.contains("username/password") {
            return match (&self.user, &self.pass) {
                (Some(u), Some(p)) => vec![
                    Action::Log(format!("Sending credentials for '{kind}' (user {u})")),
                    Action::Send(format!("username {} {}", mgmt_quote(kind), mgmt_quote(u))),
                    Action::Send(format!("password {} {}", mgmt_quote(kind), mgmt_quote(p))),
                ],
                _ => self.fail("The server requires a username and password: set them in the profile".into(), true),
            };
        }
        if tail.contains("password") {
            return match &self.pass {
                Some(p) => vec![
                    Action::Log(format!("Sending password for '{kind}'")),
                    Action::Send(format!("password {} {}", mgmt_quote(kind), mgmt_quote(p))),
                ],
                None => self.fail(format!("OpenVPN needs a password for '{kind}': set it in the profile"), true),
            };
        }
        vec![]
    }
}

// ---------------------------------------------------------------- process control

pub struct Paths {
    pub config: PathBuf,
    pub mgmt_pw: PathBuf,
    pub pid: PathBuf,
    pub log: PathBuf,
}

/// Command line for the elevated openvpn process.
pub fn build_args(p: &Paths, mgmt_port: u16, extra: &str, windows: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--config".into(),
        p.config.to_string_lossy().into(),
        "--management".into(),
        "127.0.0.1".into(),
        mgmt_port.to_string(),
        p.mgmt_pw.to_string_lossy().into(),
        "--management-query-passwords".into(),
        "--management-hold".into(),
        "--writepid".into(),
        p.pid.to_string_lossy().into(),
        "--log".into(),
        p.log.to_string_lossy().into(),
    ];
    if !windows {
        a.push("--daemon".into());
    }
    a.extend(KEEP_SERVER_ROUTE.iter().map(|s| s.to_string()));
    a.extend(extra.split_whitespace().map(str::to_string));
    a
}

/// Keeps the VPN server reachable outside the tunnel. Servers often push a route that covers
/// their own address (e.g. `route 152.118.0.0 255.255.0.0` from 152.118.33.20); without this
/// host route the encrypted packets would be sent into the tunnel itself ("Recursive routing
/// detected") and nothing gets through. redirect-gateway adds the same route on its own.
pub const KEEP_SERVER_ROUTE: [&str; 4] = ["--route", "remote_host", "255.255.255.255", "net_gateway"];

/// Directives that make OpenVPN run a script (they need `--script-security 2`).
const SCRIPT_DIRECTIVES: &[&str] = &[
    "up", "down", "route-up", "route-pre-down", "ipchange", "client-connect", "client-disconnect", "learn-address",
    "tls-verify", "auth-user-pass-verify", "script-security", "dns-updown",
];

/// Arguments placed before `--config` (so the config can still override them) that let the
/// bundled OpenVPN apply pushed DNS servers when started with the password prompt. Its built-in
/// DNS script path is the root-owned copy from the passwordless helper; without that copy the
/// script next to the binary is used, which needs `--script-security 2`. Skipped for configs that
/// run scripts of their own or choose their DNS script, so they behave as before.
pub fn dns_prefix(bin: &Path, bundled: Option<&Path>, bundled_script: Option<&Path>, root_copy_installed: bool, config: &str) -> Vec<String> {
    let (Some(bundled), Some(script)) = (bundled, bundled_script) else { return vec![] };
    if bin != bundled || root_copy_installed {
        return vec![];
    }
    let runs_scripts = config.lines().any(|l| {
        let d = l.trim_start().split_whitespace().next().unwrap_or("").trim_start_matches("--").to_ascii_lowercase();
        SCRIPT_DIRECTIVES.contains(&d.as_str())
    });
    if runs_scripts {
        return vec![];
    }
    vec!["--script-security".into(), "2".into(), "--dns-updown".into(), script.to_string_lossy().into_owned()]
}

/// Running OpenVPN daemon. Dropping it asks the daemon to exit (best effort).
pub struct OvpnRun {
    tx: mpsc::UnboundedSender<String>,
    reader: JoinHandle<()>,
    pid_file: PathBuf,
    /// Started through the passwordless helper (stop / clean up through it too).
    via_helper: bool,
}

impl Drop for OvpnRun {
    fn drop(&mut self) {
        let _ = self.tx.send("signal SIGTERM".into());
    }
}

impl OvpnRun {
    /// SIGTERM over management; falls back to an elevated kill if that doesn't work.
    pub async fn stop(mut self, id: &str) -> AppResult<()> {
        if self.reader.is_finished() {
            return Ok(());
        }
        registry::log(id, "Disconnecting");
        let sent = self.tx.send("signal SIGTERM".into()).is_ok();
        if sent && tokio::time::timeout(Duration::from_secs(8), &mut self.reader).await.is_ok() {
            if self.via_helper {
                // Removes the helper's root-only copies of the config (no prompt).
                let args = vec!["openvpn-stop".to_string(), "--profile-id".into(), id.to_string()];
                tokio::spawn(async move { helper::run(&args).await });
            }
            return Ok(());
        }
        if self.via_helper {
            registry::log(id, "Management interface not responding; stopping the process through the passwordless helper");
            match helper::run(&["openvpn-stop".into(), "--profile-id".into(), id.to_string()]).await {
                Outcome::Done(msg) => {
                    registry::log(id, msg.trim());
                    self.reader.abort();
                    return Ok(());
                }
                Outcome::Rejected(m) | Outcome::Failed(m) => {
                    self.reader.abort();
                    return Err(AppError::msg(format!("Cannot stop OpenVPN: {m}")));
                }
                Outcome::Unavailable(why) => registry::log(id, &format!("Passwordless helper unavailable ({why})")),
            }
        }
        registry::log(id, "Management interface not responding; stopping the process with administrator rights");
        let pid = std::fs::read_to_string(&self.pid_file).ok().and_then(|s| s.trim().parse::<u32>().ok());
        let Some(pid) = pid else {
            return Err(AppError::msg("OpenVPN is not responding and its process id is unknown"));
        };
        let out = if cfg!(windows) {
            elevate::run("taskkill.exe", &["/PID".into(), pid.to_string(), "/F".into()], true, None).await?
        } else {
            elevate::run("/bin/kill", &[pid.to_string()], true, None).await?
        };
        self.reader.abort();
        if out.success { Ok(()) } else { Err(AppError::msg(format!("Cannot stop OpenVPN: {}", out.message()))) }
    }
}

/// Management session of a running daemon, kept in the keychain so a DevCat that restarted
/// (crash, update) re-attaches to it instead of leaving OpenVPN running unattended: after a
/// restart (e.g. ping-restart) OpenVPN waits for "hold release" from the management client and
/// removes its routes until it gets it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Session {
    pub port: u16,
    pub token: String,
    pub pid_file: PathBuf,
    pub via_helper: bool,
}

fn session_key(id: &str) -> String {
    format!("net:{id}:session")
}

fn save_session(id: &str, s: &Session) {
    if let Ok(json) = serde_json::to_string(s) {
        let _ = crate::secrets::set(&session_key(id), &json);
    }
}

/// A previous DevCat process saved a session for this profile (cheap: one keychain lookup).
pub fn has_session(id: &str) -> bool {
    load_session(id).is_some()
}

fn load_session(id: &str) -> Option<Session> {
    crate::secrets::get(&session_key(id)).and_then(|j| serde_json::from_str(&j).ok())
}

/// Forget the session, but only if it is still the one with `token` (a newer one may exist).
fn forget_session(id: &str, token: &str) {
    if load_session(id).is_some_and(|s| s.token == token) {
        crate::secrets::delete(&session_key(id));
    }
}

/// Re-attach to the OpenVPN daemon a previous DevCat process started (see [`Session`]).
/// Returns false when there is nothing to re-attach to.
pub async fn adopt(id: String, profile: NetProfile) -> bool {
    let Some(s) = load_session(&id) else { return false };
    let stream = match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", s.port))).await {
        Ok(Ok(stream)) => stream,
        _ => {
            forget_session(&id, &s.token); // the daemon is gone
            return false;
        }
    };
    let Some(generation) = registry::begin_connect(&id) else { return false };
    registry::log(&id, "Re-attached to the OpenVPN that was running before DevCat restarted");
    let (tx, rx) = mpsc::unbounded_channel();
    // "hold release": a daemon that restarted meanwhile is waiting for it (no routes until then).
    for cmd in [s.token.as_str(), "log on all", "state on all", "bytecount 1", "hold release"] {
        let _ = tx.send(cmd.to_string());
    }
    let machine = Machine::new(profile.vpn_user.clone(), profile.vpn_password.clone());
    let reader = tokio::spawn(mgmt_task(id.clone(), generation, stream, rx, machine, s.token.clone()));
    let run = OvpnRun { tx, reader, pid_file: s.pid_file.clone(), via_helper: s.via_helper };
    match registry::store_run(&id, generation, Run::Openvpn(run)) {
        Ok(()) => {
            registry::set_connecting(&id, generation, Some("re-attaching".into()));
            true
        }
        Err(_) => false,
    }
}

/// An OpenVPN of this profile still running without DevCat (left over from a DevCat that
/// crashed before it could save a session): its pid, if the pid file points at a live
/// openvpn process using this profile's config.
#[cfg(unix)]
fn leftover_pid(pid_file: &Path, config: &Path) -> Option<u32> {
    let pid: i32 = std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()?;
    if pid <= 1 {
        return None;
    }
    // SAFETY: signal 0 only checks that the process exists (EPERM = exists, owned by root).
    let alive = unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return None;
    }
    let out = std::process::Command::new("/bin/ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    let cmd = String::from_utf8_lossy(&out.stdout);
    (cmd.contains("openvpn") && cmd.contains(&*config.to_string_lossy())).then_some(pid as u32)
}

#[cfg(not(unix))]
fn leftover_pid(_pid_file: &Path, _config: &Path) -> Option<u32> {
    None
}

fn free_port() -> AppResult<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn log_tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// `ovpnArgs` the helper can pass on: nothing or `--verb N`. `Err` = anything else.
pub fn helper_verb(extra: &str) -> Result<Option<u8>, ()> {
    match extra.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        ["--verb", n] => n.parse::<u8>().ok().filter(|n| *n <= 11).map(Some).ok_or(()),
        _ => Err(()),
    }
}

/// `Some(verb)` when this connect goes through the passwordless helper.
async fn use_helper(id: &str, prompt: bool, extra: &str) -> Option<Option<u8>> {
    if prompt || cfg!(windows) || !helper::ready().await {
        return None;
    }
    if let Err(e) = helper::find_openvpn(0) {
        registry::log(id, &format!("Passwordless VPN can't run OpenVPN here ({e}); asking for the administrator password instead"));
        return None;
    }
    match helper_verb(extra) {
        Ok(v) => Some(v),
        Err(()) => {
            registry::log(id, "Extra OpenVPN arguments aren't passed through the passwordless helper; asking for the administrator password instead");
            None
        }
    }
}

/// Start the daemon through the helper. `None` = the helper isn't usable (fall back).
async fn launch_with_helper(id: &str, paths: &Paths, port: u16, verb: Option<u8>) -> Option<AppResult<()>> {
    registry::log(id, "Starting OpenVPN through the passwordless helper");
    let mut args: Vec<String> = vec![
        "openvpn-start".into(),
        "--config".into(),
        paths.config.to_string_lossy().into(),
        "--mgmt-port".into(),
        port.to_string(),
        "--mgmt-pass-file".into(),
        paths.mgmt_pw.to_string_lossy().into(),
        "--profile-id".into(),
        id.into(),
    ];
    if let Some(v) = verb {
        args.extend(["--verb".into(), v.to_string()]);
    }
    match helper::run(&args).await {
        Outcome::Done(_) => Some(Ok(())),
        Outcome::Rejected(m) => Some(Err(AppError::msg(helper::rejection(&m)))),
        Outcome::Failed(m) => {
            let tail = log_tail(&paths.log, 5);
            Some(Err(AppError::msg(format!("OpenVPN failed to start: {}", if tail.is_empty() { m } else { tail }))))
        }
        Outcome::Unavailable(why) => {
            registry::log(id, &format!("Passwordless helper unavailable ({why}); asking for the administrator password instead"));
            None
        }
    }
}

pub async fn connect(id: String, generation: u64, profile: NetProfile, store: &NetStore, prompt: bool) -> AppResult<()> {
    let dir = store.dir()?;
    let mut paths = Paths {
        config: store.config_path(&id, NetKind::Openvpn)?,
        mgmt_pw: dir.join(format!("{id}.mgmt")),
        pid: dir.join(format!("{id}.pid")),
        log: dir.join(format!("{id}.log")),
    };
    if !paths.config.exists() {
        return Err(AppError::msg("The OpenVPN configuration file is missing; edit the profile and paste it again"));
    }
    let stored = paths.config.clone();
    paths.config = run_config(&id, &stored, &dir)?;
    // A daemon left running by a DevCat that crashed is stopped first (in the same admin prompt).
    let leftover = leftover_pid(&paths.pid, &stored).or_else(|| leftover_pid(&paths.pid, &paths.config));
    if let Some(pid) = leftover {
        registry::log(&id, &format!("An OpenVPN from an earlier DevCat session is still running (pid {pid}); it will be stopped first"));
    }
    let prompt = prompt || leftover.is_some();
    let port = free_port()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    write_private(&paths.mgmt_pw, format!("{token}\n").as_bytes())?;
    let extra = profile.options.get("ovpnArgs").cloned().unwrap_or_default();

    let mut via_helper = false;
    let mut launched = None;
    if let Some(verb) = use_helper(&id, prompt, &extra).await {
        let (pid, log) = helper::run_files(&id);
        let helper_paths = Paths { pid, log, ..paths };
        registry::set_connecting(&id, generation, Some("starting".into()));
        launched = launch_with_helper(&id, &helper_paths, port, verb).await;
        via_helper = launched.is_some();
        // Back to the app's own pid / log files if the helper wasn't usable.
        paths = if via_helper { helper_paths } else { Paths { pid: dir.join(format!("{id}.pid")), log: dir.join(format!("{id}.log")), ..helper_paths } };
    }
    let launched = match launched {
        Some(r) => r,
        None => launch_with_prompt(&id, generation, &paths, port, &extra, leftover).await,
    };
    if let Err(e) = launched {
        let _ = std::fs::remove_file(&paths.mgmt_pw);
        return Err(e);
    }

    let mut stream = None;
    for _ in 0..40 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = std::fs::remove_file(&paths.mgmt_pw);
    let Some(stream) = stream else {
        let tail = log_tail(&paths.log, 8);
        return Err(AppError::msg(format!(
            "OpenVPN did not start{}",
            if tail.is_empty() { String::new() } else { format!(":\n{tail}") }
        )));
    };
    save_session(&id, &Session { port, token: token.clone(), pid_file: paths.pid.clone(), via_helper });
    let (tx, rx) = mpsc::unbounded_channel();
    for cmd in [token.as_str(), "log on all", "state on all", "bytecount 1", "hold release"] {
        let _ = tx.send(cmd.to_string());
    }
    let machine = Machine::new(profile.vpn_user.clone(), profile.vpn_password.clone());
    let reader = tokio::spawn(mgmt_task(id.clone(), generation, stream, rx, machine, token));
    let run = OvpnRun { tx, reader, pid_file: paths.pid.clone(), via_helper };
    match registry::store_run(&id, generation, Run::Openvpn(run)) {
        Ok(()) => {
            registry::set_connecting(&id, generation, Some("connecting".into()));
        }
        Err(Run::Openvpn(stale)) => {
            // Disconnected while we were starting: stop the daemon again.
            let _ = stale.stop(&id).await;
        }
        Err(_) => {}
    }
    Ok(())
}

/// `config` with `user` / `group` commented out, or `None` when it has neither. Lines inside
/// inline `<tag>` blocks are left alone.
pub fn without_privilege_drop(config: &str) -> Option<String> {
    let mut block: Option<String> = None;
    let mut changed = false;
    let out: Vec<String> = config
        .split('\n')
        .map(|line| {
            let t = line.trim_start();
            let lower = t.to_ascii_lowercase();
            if let Some(close) = &block {
                if lower.starts_with(close.as_str()) {
                    block = None;
                }
                return line.to_string();
            }
            if lower.starts_with('<') && !lower.starts_with("</") {
                if let Some(end) = lower.find('>') {
                    block = Some(format!("</{}>", &lower[1..end]));
                }
                return line.to_string();
            }
            let toks = helper::ovpn_tokens(line);
            let name = toks.first().map(|t| t.strip_prefix("--").unwrap_or(t).to_ascii_lowercase());
            if matches!(name.as_deref(), Some("user" | "group")) {
                changed = true;
                return format!("# {line}  (ignored by DevCat: OpenVPN must stay root to remove its routes)");
            }
            line.to_string()
        })
        .collect();
    changed.then(|| out.join("\n"))
}

/// The config file OpenVPN is started with. On macOS / Linux, OpenVPN that dropped root with
/// `user` / `group` can't delete its routes on disconnect, and the host route to the server
/// (KEEP_SERVER_ROUTE) is left behind; once the Wi-Fi address changes it blocks every app from
/// reaching the server ("Can't assign requested address"). Such configs run from a copy
/// (`<id>.run.ovpn`) without those lines; the profile itself is left as imported.
fn run_config(id: &str, stored: &Path, dir: &Path) -> AppResult<PathBuf> {
    let run = dir.join(format!("{id}.run.ovpn"));
    let text = std::fs::read_to_string(stored).unwrap_or_default();
    match without_privilege_drop(&text) {
        Some(copy) if !cfg!(windows) => {
            write_private(&run, copy.as_bytes())?;
            registry::log(id, "Ignoring `user` / `group` in this config so OpenVPN can remove its routes when it disconnects");
            Ok(run)
        }
        _ => {
            let _ = std::fs::remove_file(&run);
            Ok(stored.to_path_buf())
        }
    }
}

/// Start the daemon elevated with the OS password prompt.
async fn launch_with_prompt(id: &str, generation: u64, paths: &Paths, port: u16, extra: &str, leftover: Option<u32>) -> AppResult<()> {
    let bin = tools::find_openvpn()
        .ok_or_else(|| AppError::msg(format!("OpenVPN is not installed. {}", tools::openvpn_hint())))?;
    // Created by us so the log stays readable after root writes to it.
    write_private(&paths.log, b"")?;
    let _ = std::fs::remove_file(&paths.pid);
    let config = std::fs::read_to_string(&paths.config).unwrap_or_default();
    let root_copy = helper::HELPER_DNS_UPDOWN.is_some_and(|p| Path::new(p).is_file());
    let mut args = dns_prefix(&bin, tools::bundled_openvpn().as_deref(), tools::bundled_dns_updown().as_deref(), root_copy, &config);
    args.extend(build_args(paths, port, extra, cfg!(windows)));
    registry::log(id, &format!("Starting {} (administrator rights required)", bin.display()));
    registry::set_connecting(id, generation, Some("waiting for authorization".into()));
    let (program, args) = match leftover {
        Some(pid) if !cfg!(windows) => (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), format!("/bin/kill {pid} 2>/dev/null; sleep 1; exec {}", elevate::sh_command(&bin.to_string_lossy(), &args))],
        ),
        _ => (bin.to_string_lossy().into_owned(), args),
    };
    match elevate::run(&program, &args, !cfg!(windows), None).await {
        Ok(o) if o.success => Ok(()),
        Ok(o) => {
            let tail = log_tail(&paths.log, 5);
            Err(AppError::msg(format!("OpenVPN failed to start: {}", if tail.is_empty() { o.message() } else { tail })))
        }
        Err(e) => Err(e),
    }
}

async fn mgmt_task(id: String, generation: u64, stream: TcpStream, mut rx: mpsc::UnboundedReceiver<String>, mut m: Machine, token: String) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut rx_open = true;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                for a in m.on_line(&line) {
                    match a {
                        Action::Send(cmd) => {
                            if w.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Action::Log(l) => registry::log(&id, &l),
                        Action::Connecting(d) => {
                            registry::set_connecting(&id, generation, Some(d));
                        }
                        Action::Connected(d) => {
                            registry::log(&id, &format!("Connected: {d}"));
                            registry::set_connected(&id, generation, Some(d), vec![]);
                        }
                        Action::Error(e) => {
                            registry::set_error(&id, generation, e);
                        }
                        Action::Bytes(rx, tx) => super::traffic::set_totals(&id, rx, tx),
                    }
                }
            }
            cmd = rx.recv(), if rx_open => match cmd {
                Some(cmd) => {
                    if w.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
                None => rx_open = false,
            }
        }
    }
    registry::log(&id, "OpenVPN management connection closed");
    // The daemon exited (or was stopped): nothing to re-attach to after a restart.
    forget_session(&id, &token);
    if registry::generation(&id) == Some(generation) && !m.has_failed() && registry::status(&id).state != registry::ERROR {
        registry::set_error(&id, generation, m.exit_message());
    }
    drop(registry::take_run_if(&id, generation));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(m: &mut Machine, lines: &[&str]) -> Vec<Action> {
        lines.iter().flat_map(|l| m.on_line(l)).collect()
    }

    #[test]
    fn quoting() {
        assert_eq!(mgmt_quote("Auth"), "\"Auth\"");
        assert_eq!(mgmt_quote(r#"p"a\ss"#), r#""p\"a\\ss""#);
        assert_eq!(mgmt_quote("a\nb\r"), "\"ab\"");
    }

    #[test]
    fn happy_path() {
        let mut m = Machine::new(Some("alice".into()), Some("pa\"ss".into()));
        let a = run(
            &mut m,
            &[
                "ENTER PASSWORD:SUCCESS: password is correct",
                ">INFO:OpenVPN Management Interface Version 5 -- type 'help' for more info",
                "1700000000,,OpenVPN 2.6.12 starting",
                "END",
                "SUCCESS: real-time state notification set to ON",
                ">HOLD:Waiting for hold release:0",
                ">PASSWORD:Need 'Auth' username/password",
                ">STATE:1700000001,RESOLVE,,,,,,",
                ">STATE:1700000002,WAIT,,,,,,",
                ">STATE:1700000003,AUTH,,,,,,",
                ">LOG:1700000004,W,WARNING: something odd",
                ">STATE:1700000005,CONNECTED,SUCCESS,10.8.0.6,203.0.113.5,1194,,",
            ],
        );
        assert_eq!(
            a,
            vec![
                Action::Log("OpenVPN Management Interface Version 5 -- type 'help' for more info".into()),
                Action::Log("OpenVPN 2.6.12 starting".into()),
                Action::Send("hold release".into()),
                Action::Log("Sending credentials for 'Auth' (user alice)".into()),
                Action::Send("username \"Auth\" \"alice\"".into()),
                Action::Send("password \"Auth\" \"pa\\\"ss\"".into()),
                Action::Connecting("resolve".into()),
                Action::Connecting("wait".into()),
                Action::Connecting("auth".into()),
                Action::Log("WARNING: something odd".into()),
                Action::Connected("10.8.0.6 (203.0.113.5:1194)".into()),
            ]
        );
        // Credentials never end up in log actions.
        assert!(!a.iter().any(|x| matches!(x, Action::Log(l) if l.contains("pa\"ss"))));
    }

    #[test]
    fn state_history_and_reconnect() {
        let mut m = Machine::new(None, None);
        assert_eq!(m.on_line("1700000000,CONNECTED,ERROR,10.8.0.6,1.2.3.4,443,,"), vec![Action::Connected("10.8.0.6 (1.2.3.4:443) — with errors, see the log".into())]);
        assert_eq!(m.on_line(">STATE:1,RECONNECTING,ping-restart,,,,,"), vec![Action::Connecting("reconnecting (ping-restart)".into())]);
        assert_eq!(m.on_line(">STATE:1,GET_CONFIG,,,,,,"), vec![Action::Connecting("get config".into())]);
        assert_eq!(m.on_line(">BYTECOUNT:1,2"), vec![Action::Bytes(1, 2)]);
        assert_eq!(m.on_line(">BYTECOUNT:x"), vec![]);
    }

    #[test]
    fn auth_failure() {
        let mut m = Machine::new(Some("bob".into()), Some("wrong".into()));
        let a = run(
            &mut m,
            &[
                ">PASSWORD:Need 'Auth' username/password",
                ">PASSWORD:Verification Failed: 'Auth'",
                ">STATE:1,RECONNECTING,auth-failure,,,,,",
                ">STATE:1,EXITING,auth-failure,,,,,",
            ],
        );
        let errors: Vec<_> = a.iter().filter(|x| matches!(x, Action::Error(_))).collect();
        assert_eq!(errors, vec![&Action::Error("Authentication failed (check the username/password)".into())]);
        assert!(a.contains(&Action::Send("signal SIGTERM".into())));
        assert!(!a.iter().any(|x| matches!(x, Action::Connecting(_))), "{a:?}");
        assert!(m.has_failed());
    }

    #[test]
    fn missing_credentials_and_key_password() {
        let mut m = Machine::new(Some("bob".into()), None);
        let a = m.on_line(">PASSWORD:Need 'Auth' username/password");
        assert!(matches!(&a[0], Action::Error(e) if e.contains("username and password")));
        assert_eq!(a[1], Action::Send("signal SIGTERM".into()));

        let mut m = Machine::new(None, Some("keypass".into()));
        assert_eq!(
            m.on_line(">PASSWORD:Need 'Private Key' password"),
            vec![Action::Log("Sending password for 'Private Key'".into()), Action::Send("password \"Private Key\" \"keypass\"".into())]
        );

        let mut m = Machine::new(Some("u".into()), Some("p".into()));
        let a = m.on_line(">PASSWORD:Need 'Auth' username/password SC:1,Enter your token");
        assert!(matches!(&a[0], Action::Error(e) if e.contains("static challenge")));
        let a = Machine::new(None, None).on_line(">PASSWORD:Verification Failed: 'Auth' ['CRV1:R,E:Om01u7Fh4LrGBS7uh0SWmzwabUiGiW6l:Y3Ix:Please enter token PIN']");
        assert!(matches!(&a[0], Action::Error(e) if e.contains("challenge")));
        assert_eq!(Machine::new(None, None).on_line(">PASSWORD:Auth-Token:abc"), vec![]);
    }

    #[test]
    fn fatal_and_exit() {
        let mut m = Machine::new(None, None);
        assert_eq!(m.on_line(">LOG:1,F,Cannot open TUN/TAP dev"), vec![Action::Log("Cannot open TUN/TAP dev".into())]);
        let a = m.on_line(">STATE:1,EXITING,exit-with-notification,,,,,");
        assert_eq!(a, vec![Action::Error("Cannot open TUN/TAP dev".into())]);
        assert_eq!(m.exit_message(), "OpenVPN exited: Cannot open TUN/TAP dev");

        let mut m = Machine::new(None, None);
        assert_eq!(m.on_line(">FATAL:Options error"), vec![Action::Error("Options error".into())]);
        assert_eq!(m.on_line(">STATE:1,EXITING,SIGTERM,,,,,"), vec![]);

        let mut m = Machine::new(None, None);
        assert_eq!(m.on_line(">STATE:1,EXITING,SIGTERM,,,,,"), vec![Action::Error("OpenVPN exited (SIGTERM)".into())]);
        assert_eq!(Machine::new(None, None).exit_message(), "OpenVPN exited unexpectedly (see the log)");
    }

    #[test]
    fn privilege_drop_is_commented_out() {
        assert_eq!(without_privilege_drop("client\nremote a 1194\n"), None);
        let out = without_privilege_drop("client\nuser nobody\n  group nogroup\n<key>\nuser x\n</key>\npersist-tun\n").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "client");
        assert!(lines[1].starts_with("# user nobody"));
        assert!(lines[2].starts_with("#   group nogroup"));
        assert_eq!(&lines[3..], ["<key>", "user x", "</key>", "persist-tun"]);
    }

    #[test]
    fn helper_extra_args() {
        assert_eq!(helper_verb(""), Ok(None));
        assert_eq!(helper_verb("  --verb 4 "), Ok(Some(4)));
        assert_eq!(helper_verb("--verb 12"), Err(()));
        assert_eq!(helper_verb("--verb x"), Err(()));
        assert_eq!(helper_verb("--route-nopull"), Err(()));
        assert_eq!(helper_verb("--verb 3 --up /x"), Err(()));
    }

    #[test]
    fn args() {
        let p = Paths {
            config: "/data dir/network/x.ovpn".into(),
            mgmt_pw: "/data dir/network/x.mgmt".into(),
            pid: "/data dir/network/x.pid".into(),
            log: "/data dir/network/x.log".into(),
        };
        let a = build_args(&p, 40000, " --verb 4 ", false);
        assert_eq!(
            a,
            vec![
                "--config", "/data dir/network/x.ovpn", "--management", "127.0.0.1", "40000", "/data dir/network/x.mgmt",
                "--management-query-passwords", "--management-hold", "--writepid", "/data dir/network/x.pid", "--log",
                "/data dir/network/x.log", "--daemon", "--route", "remote_host", "255.255.255.255", "net_gateway", "--verb", "4",
            ]
        );
        assert!(!build_args(&p, 1, "", true).contains(&"--daemon".to_string()));
        // The macOS elevation command line keeps the space-containing paths intact.
        let line = elevate::sh_command("/opt/homebrew/sbin/openvpn", &a);
        assert!(line.contains("--config '/data dir/network/x.ovpn'"), "{line}");
        let script = elevate::osascript_script(&line, "p");
        assert!(script.starts_with("do shell script \"/opt/homebrew/sbin/openvpn --config '/data dir/network/x.ovpn'"));
    }

    #[test]
    fn dns_script_args() {
        let (b, sc) = (Path::new("/A/vpn/openvpn"), Path::new("/A/vpn/dns-updown"));
        let plain = "client\ndev tun\nremote x 1194\n";
        assert_eq!(dns_prefix(b, Some(b), Some(sc), false, plain), vec!["--script-security", "2", "--dns-updown", "/A/vpn/dns-updown"]);
        // The root-owned copy is the binary's built-in default: nothing to add.
        assert!(dns_prefix(b, Some(b), Some(sc), true, plain).is_empty());
        // Another OpenVPN uses its own default script.
        assert!(dns_prefix(Path::new("/opt/local/sbin/openvpn2"), Some(b), Some(sc), false, plain).is_empty());
        assert!(dns_prefix(b, Some(b), None, false, plain).is_empty());
        for c in ["up /x.sh", "  --down /x.sh", "Dns-Updown disable", "script-security 3"] {
            assert!(dns_prefix(b, Some(b), Some(sc), false, &format!("{plain}{c}\n")).is_empty(), "{c}");
        }
        assert!(!dns_prefix(b, Some(b), Some(sc), false, "# up /x.sh\nupdown-ish\n").is_empty());
    }

    /// The incident this guards against, end to end with a real OpenVPN (no root: `--dev null`,
    /// static key, two local peers): DevCat goes away without stopping the daemon, the daemon
    /// restarts (ping-restart / SIGUSR1) and waits for "hold release" with no routes; a new
    /// DevCat must re-attach and release it. Needs an openvpn binary:
    /// DEVCAT_TEST_OPENVPN=src-tauri/vpn-bin/openvpn cargo test openvpn::tests::adopt_after_crash -- --ignored
    #[tokio::test]
    #[ignore]
    async fn adopt_after_crash() {
        let bin = std::env::var("DEVCAT_TEST_OPENVPN").expect("DEVCAT_TEST_OPENVPN");
        let dir = std::env::temp_dir().join(format!("devcat-adopt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("s.key");
        assert!(std::process::Command::new(&bin).args(["--genkey", "secret"]).arg(&key).status().unwrap().success());
        let (pa, pb) = (free_port().unwrap(), free_port().unwrap());
        let common = |lport: u16, rport: u16| -> Vec<String> {
            ["--dev", "null", "--secret", &key.to_string_lossy(), "--allow-deprecated-insecure-static-crypto",
             "--cipher", "AES-256-CBC", "--proto", "udp", "--remote", "127.0.0.1", "--verb", "3", "--ping", "1",
             "--lport", &lport.to_string(), "--rport", &rport.to_string()].iter().map(|s| s.to_string()).collect()
        };
        let mport = free_port().unwrap();
        let token = "t0ken";
        let pw = dir.join("m.pw");
        std::fs::write(&pw, format!("{token}\n")).unwrap();
        let pidf = dir.join("a.pid");
        let mut a = std::process::Command::new(&bin)
            .args(common(pa, pb))
            .args(["--management", "127.0.0.1", &mport.to_string(), &pw.to_string_lossy(), "--management-hold", "--writepid", &pidf.to_string_lossy()])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let mut b = std::process::Command::new(&bin).args(common(pb, pa)).stdout(std::process::Stdio::null()).spawn().unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;

        let profile = |id: &str| NetProfile {
            id: id.into(), name: "t".into(), kind: NetKind::Openvpn, project_id: None, color: None, auto_connect: false,
            ssh: None, forwards: vec![], vpn_user: None, vpn_password: None, summary: None, options: Default::default(),
        };
        let wait = |id: String, want: &'static str| async move {
            for _ in 0..100 {
                if registry::status(&id).state == want { return true; }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            false
        };
        // First DevCat attaches (the daemon waits in hold until it does).
        let s = Session { port: mport, token: token.into(), pid_file: pidf.clone(), via_helper: false };
        save_session("adopt-1", &s);
        assert!(adopt("adopt-1".into(), profile("adopt-1")).await);
        assert!(wait("adopt-1".into(), registry::CONNECTED).await, "first attach: {:?}", registry::logs("adopt-1"));

        // "Crash": the management connection drops without SIGTERM; the session stays saved.
        let gen1 = registry::generation("adopt-1").unwrap();
        if let Some(Run::Openvpn(run)) = registry::take_run_if("adopt-1", gen1) {
            run.reader.abort();
            std::mem::forget(run); // its Drop would queue a SIGTERM
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        // The daemon restarts and now waits for "hold release" (the state the incident ended in).
        let pid = std::fs::read_to_string(&pidf).unwrap().trim().to_string();
        assert!(std::process::Command::new("/bin/kill").args(["-USR1", &pid]).status().unwrap().success());
        tokio::time::sleep(Duration::from_millis(800)).await;

        // A new DevCat finds the session, re-attaches and the tunnel comes back up.
        save_session("adopt-2", &s);
        assert!(adopt("adopt-2".into(), profile("adopt-2")).await);
        assert!(wait("adopt-2".into(), registry::CONNECTED).await, "re-attach: {:?}", registry::logs("adopt-2"));
        assert!(registry::logs("adopt-2").iter().any(|l| l.contains("Re-attached")));
        if std::env::var("DEVCAT_TEST_VERBOSE").is_ok() {
            for l in registry::logs("adopt-2") { println!("{l}"); }
        }

        let _ = a.kill();
        let _ = b.kill();
        let _ = a.wait();
        let _ = b.wait();
        let _ = std::fs::remove_dir_all(dir);
    }
}
