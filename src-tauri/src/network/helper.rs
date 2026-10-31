//! Passwordless VPN helper (opt-in, macOS/Linux).
//!
//! A root-owned copy of the DevCat binary is installed at [`HELPER_PATH`] together with a
//! sudoers rule letting the current user run `sudo -n <helper> --vpn-helper …` without a
//! password. In helper mode (`main.rs`, before Tauri starts) the binary runs exactly one
//! subcommand:
//!
//!   version
//!   openvpn-start --config <path> --mgmt-port <port> --mgmt-pass-file <path> --profile-id <id> [--verb N]
//!   openvpn-stop  --profile-id <id>
//!   wg-up   --conf <path>
//!   wg-down --conf <path>
//!
//! Any process running as the user can call the helper, so it trusts nothing from the
//! caller: the caller is `SUDO_UID`, input files must belong to that uid and are read once
//! (no symlink as the last component), validated in memory and copied into a root-only
//! directory; the VPN tools only ever read those copies. Script / plugin directives are
//! refused, and the tools themselves must live in root-owned, non-writable locations.
//!
//! The second half of this file is the app side: status, install / uninstall scripts and
//! running the helper through `sudo -n`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
pub const HELPER_PATH: &str = "/Library/PrivilegedHelperTools/dev.devcat.vpn-helper";
#[cfg(not(target_os = "macos"))]
pub const HELPER_PATH: &str = "/usr/local/libexec/devcat-vpn-helper";
pub const SUDOERS_PATH: &str = "/etc/sudoers.d/dev-devcat-vpn";
/// Root-owned state: `<RUN_DIR>/<uid>/<id>.pid|.log` (user-readable) and `<uid>/private/` (0700).
pub const RUN_DIR: &str = "/var/run/devcat-vpn";
/// First argument selecting helper mode.
pub const FLAG: &str = "--vpn-helper";
/// The OpenVPN that ships with DevCat (macOS), copied here root-owned by the setup.
#[cfg(target_os = "macos")]
pub const HELPER_OPENVPN: Option<&str> = Some("/Library/PrivilegedHelperTools/dev.devcat.vpn-openvpn");
#[cfg(not(target_os = "macos"))]
pub const HELPER_OPENVPN: Option<&str> = None;
/// Root-owned copy of the DNS script that ships with DevCat's OpenVPN; that OpenVPN is built
/// with this as its default `--dns-updown` (scripts/build-openvpn-macos.sh, DNS_DIR).
#[cfg(target_os = "macos")]
pub const HELPER_DNS_UPDOWN: Option<&str> = Some("/Library/PrivilegedHelperTools/dev.devcat.vpn-scripts/dns-updown");
#[cfg(not(target_os = "macos"))]
pub const HELPER_DNS_UPDOWN: Option<&str> = None;
/// The app ↔ helper interface. Bump it whenever the helper's commands, arguments or checks
/// change: helpers with another number are reported as needing an update. A helper from an
/// older DevCat with the same number keeps working, so updating DevCat doesn't pause it.
pub const PROTOCOL: u32 = 2;
const SUDO: &str = "/usr/bin/sudo";
const VISUDO: &str = "/usr/sbin/visudo";

/// Exit codes: the helper refused the request (message on stderr) / the tool failed.
pub const EXIT_REJECTED: i32 = 10;
pub const EXIT_FAILED: i32 = 11;

/// Where the helper looks for the tools (fixed; never PATH). The first one that passes
/// `trusted_path` wins, so a root-owned MacPorts install (/opt/local) is used even when a
/// user-owned Homebrew one is listed earlier. MacPorts' `openvpn2` port names its binary openvpn2.
const OPENVPN_BINS: &[&str] = &[
    "/opt/homebrew/sbin/openvpn",
    "/usr/local/sbin/openvpn",
    "/opt/local/sbin/openvpn",
    "/opt/local/sbin/openvpn2",
    "/usr/sbin/openvpn",
    "/usr/local/opt/openvpn/sbin/openvpn",
];
const WG_QUICK_BINS: &[&str] = &["/opt/homebrew/bin/wg-quick", "/usr/local/bin/wg-quick", "/opt/local/bin/wg-quick", "/usr/bin/wg-quick", "/usr/sbin/wg-quick"];
const BASH_BINS: &[&str] = &["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/opt/local/bin/bash", "/usr/bin/bash", "/bin/bash"];
/// PATH for wg-quick (only the root-owned, non-writable ones are used).
const TOOL_PATH_DIRS: &[&str] = &[
    "/opt/homebrew/bin", "/usr/local/bin", "/usr/local/sbin", "/opt/local/bin", "/opt/local/sbin", "/usr/bin", "/bin", "/usr/sbin", "/sbin",
];

const MAX_CONFIG: u64 = 1024 * 1024;
const MAX_REF_FILE: u64 = 1024 * 1024;
const MAX_REF_FILES: usize = 32;
const MAX_MGMT_PW: u64 = 4096;
/// OpenVPN reads config lines into a 256 byte buffer; longer lines would be split
/// differently by OpenVPN than by this validator.
const MAX_LINE: usize = 250;

/// The app version (tauri.conf.json), used by both the helper and the app.
pub fn version() -> &'static str {
    static V: once_cell::sync::Lazy<String> = once_cell::sync::Lazy::new(|| {
        serde_json::from_str::<serde_json::Value>(include_str!("../../tauri.conf.json"))
            .ok()
            .and_then(|v| v.get("version").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
    });
    &V
}

// ================================================================ helper side

#[derive(Debug, Clone, PartialEq)]
pub enum HelperError {
    /// Refused by policy or bad arguments (exit 10).
    Rejected(String),
    /// The request was fine but doing it failed (exit 11).
    Failed(String),
}

type HResult<T> = Result<T, HelperError>;

fn rejected(s: impl Into<String>) -> HelperError {
    HelperError::Rejected(s.into())
}
fn failed(s: impl Into<String>) -> HelperError {
    HelperError::Failed(s.into())
}

fn uses_msg(what: &str) -> String {
    format!("This config uses `{what}`, which DevCat's passwordless helper won't run as root. Remove it, or connect with a password prompt.")
}

/// Everything the helper operations depend on, injectable for tests.
pub struct Ctx {
    /// The calling user (`SUDO_UID`).
    pub uid: u32,
    /// Required owner of the helper's own directories (0 in production).
    pub root_uid: u32,
    pub run_dir: PathBuf,
}

impl Ctx {
    pub fn user_dir(&self) -> PathBuf {
        self.run_dir.join(self.uid.to_string())
    }
    pub fn private_dir(&self) -> PathBuf {
        self.user_dir().join("private")
    }
}

/// `SUDO_UID` → caller uid; must be set, numeric and not root.
pub fn caller_uid(sudo_uid: Option<&str>) -> HResult<u32> {
    let Some(s) = sudo_uid.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(rejected("DevCat's passwordless helper must be run through sudo (SUDO_UID is not set)"));
    };
    match s.parse::<u32>() {
        Ok(0) => Err(rejected("DevCat's passwordless helper refuses to act for root (SUDO_UID=0)")),
        Ok(uid) => Ok(uid),
        Err(_) => Err(rejected(format!("DevCat's passwordless helper: invalid SUDO_UID \"{s}\""))),
    }
}

pub fn valid_profile_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// wg-quick's own interface name rule.
pub fn valid_iface(name: &str) -> bool {
    (1..=15).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || "_=+.-".contains(c))
}

/// `--flag value` pairs; every flag must be in `allowed` and appear at most once.
fn parse_flags(args: &[String], allowed: &[&str]) -> HResult<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if !allowed.contains(&flag.as_str()) {
            return Err(rejected(format!("DevCat's passwordless helper: unexpected argument \"{flag}\"")));
        }
        let Some(value) = it.next() else {
            return Err(rejected(format!("DevCat's passwordless helper: {flag} needs a value")));
        };
        if out.insert(flag.clone(), value.clone()).is_some() {
            return Err(rejected(format!("DevCat's passwordless helper: {flag} given twice")));
        }
    }
    Ok(out)
}

fn required<'a>(f: &'a HashMap<String, String>, flag: &str) -> HResult<&'a str> {
    f.get(flag).map(String::as_str).ok_or_else(|| rejected(format!("DevCat's passwordless helper: {flag} is required")))
}

fn abs_path(s: &str, flag: &str) -> HResult<PathBuf> {
    let p = PathBuf::from(s);
    if p.is_absolute() && !s.contains('\0') {
        Ok(p)
    } else {
        Err(rejected(format!("DevCat's passwordless helper: {flag} must be an absolute path")))
    }
}

fn profile_id_arg(f: &HashMap<String, String>) -> HResult<String> {
    let id = required(f, "--profile-id")?;
    if valid_profile_id(id) {
        Ok(id.to_string())
    } else {
        Err(rejected(format!("DevCat's passwordless helper: invalid profile id \"{id}\"")))
    }
}

#[derive(Debug, PartialEq)]
pub struct StartArgs {
    pub config: PathBuf,
    pub mgmt_port: u16,
    pub mgmt_pass_file: PathBuf,
    pub profile_id: String,
    pub verb: Option<u8>,
}

pub fn parse_start_args(args: &[String]) -> HResult<StartArgs> {
    let f = parse_flags(args, &["--config", "--mgmt-port", "--mgmt-pass-file", "--profile-id", "--verb"])?;
    let port = required(&f, "--mgmt-port")?;
    let mgmt_port = match port.parse::<u16>() {
        Ok(p) if p >= 1024 => p,
        _ => return Err(rejected(format!("DevCat's passwordless helper: management port must be 1024-65535, got \"{port}\""))),
    };
    let verb = match f.get("--verb") {
        None => None,
        Some(v) => match v.parse::<u8>() {
            Ok(n) if n <= 11 => Some(n),
            _ => return Err(rejected(format!("DevCat's passwordless helper: --verb must be 0-11, got \"{v}\""))),
        },
    };
    Ok(StartArgs {
        config: abs_path(required(&f, "--config")?, "--config")?,
        mgmt_port,
        mgmt_pass_file: abs_path(required(&f, "--mgmt-pass-file")?, "--mgmt-pass-file")?,
        profile_id: profile_id_arg(&f)?,
        verb,
    })
}

pub fn parse_stop_args(args: &[String]) -> HResult<String> {
    profile_id_arg(&parse_flags(args, &["--profile-id"])?)
}

pub fn parse_wg_args(args: &[String]) -> HResult<PathBuf> {
    let f = parse_flags(args, &["--conf"])?;
    abs_path(required(&f, "--conf")?, "--conf")
}

// ---------------------------------------------------------------- file checks

/// A tool / directory is trusted when it and every ancestor (after resolving symlinks) is
/// owned by root (or `root_uid`) and not writable by group or others.
#[cfg(unix)]
pub fn trusted_path(p: &Path, root_uid: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let c = std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()))?;
    for a in c.ancestors() {
        let m = std::fs::symlink_metadata(a).map_err(|e| format!("{}: {e}", a.display()))?;
        if m.uid() != 0 && m.uid() != root_uid {
            return Err(format!("{} is owned by a regular user account, not root", a.display()));
        }
        if m.mode() & 0o022 != 0 {
            return Err(format!("{} is writable by group or others", a.display()));
        }
    }
    Ok(c)
}

#[cfg(not(unix))]
pub fn trusted_path(p: &Path, _root_uid: u32) -> Result<PathBuf, String> {
    Err(format!("{}: not supported on this OS", p.display()))
}

/// First candidate that exists and is a trusted executable. If some exist but none is
/// trusted, the error explains why the first one isn't.
pub fn find_trusted(candidates: &[&str], root_uid: u32) -> Result<PathBuf, String> {
    let mut first_err = None;
    for c in candidates {
        let p = Path::new(c);
        if !p.exists() {
            continue;
        }
        match trusted_path(p, root_uid) {
            Ok(t) if is_exec_file(&t) => return Ok(t),
            Ok(t) => first_err = first_err.or(Some(format!("{} is not an executable file", t.display()))),
            Err(e) => first_err = first_err.or(Some(format!("{c} can't be run as root safely: {e}"))),
        }
    }
    Err(first_err.unwrap_or_else(|| format!("not found in {}", candidates.join(", "))))
}

fn is_exec_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub fn find_openvpn(root_uid: u32) -> Result<PathBuf, String> {
    // DevCat's own copy first: it is installed root-owned and only links system libraries.
    let candidates: Vec<&str> = HELPER_OPENVPN.into_iter().chain(OPENVPN_BINS.iter().copied()).collect();
    find_trusted(&candidates, root_uid).map_err(|e| format!("OpenVPN: {e}"))
}

/// (bash, wg-quick)
pub fn find_wg_tools(root_uid: u32) -> Result<(PathBuf, PathBuf), String> {
    let wg = find_trusted(WG_QUICK_BINS, root_uid).map_err(|e| format!("wg-quick: {e}"))?;
    let bash = find_trusted(BASH_BINS, root_uid).map_err(|e| format!("bash: {e}"))?;
    Ok((bash, wg))
}

fn tool_path(root_uid: u32) -> String {
    let dirs: Vec<&str> = TOOL_PATH_DIRS.iter().copied().filter(|d| trusted_path(Path::new(d), root_uid).is_ok()).collect();
    if dirs.is_empty() { "/usr/bin:/bin:/usr/sbin:/sbin".into() } else { dirs.join(":") }
}

/// Every parent dir must be owned by root or `uid` and not writable by others
/// (root-owned sticky dirs such as /tmp are fine).
#[cfg(unix)]
fn check_parents(path: &Path, uid: u32) -> HResult<()> {
    use std::os::unix::fs::MetadataExt;
    let parent = path.parent().ok_or_else(|| rejected(format!("{} has no parent directory", path.display())))?;
    let parent = std::fs::canonicalize(parent).map_err(|e| rejected(format!("{}: {e}", parent.display())))?;
    for a in parent.ancestors() {
        let m = std::fs::symlink_metadata(a).map_err(|e| rejected(format!("{}: {e}", a.display())))?;
        let owner_ok = m.uid() == 0 || m.uid() == uid;
        let sticky_root = m.uid() == 0 && m.mode() & 0o1000 != 0;
        if !owner_ok || (m.mode() & 0o022 != 0 && !sticky_root) {
            return Err(rejected(format!(
                "DevCat's passwordless helper won't use {}: the folder {} can be changed by other users",
                path.display(),
                a.display()
            )));
        }
    }
    Ok(())
}

/// Read a caller-owned file once: no symlink as the last component, a regular file owned
/// by `uid`, at most `cap` bytes, safe parent dirs.
#[cfg(unix)]
pub fn read_user_file(path: &Path, uid: u32, cap: u64, what: &str) -> HResult<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                rejected(format!("The {what} {} is a symbolic link, which DevCat's passwordless helper won't follow", path.display()))
            } else {
                rejected(format!("DevCat's passwordless helper can't open the {what} {}: {e}", path.display()))
            }
        })?;
    let m = f.metadata().map_err(|e| failed(e.to_string()))?;
    if !m.is_file() {
        return Err(rejected(format!("The {what} {} is not a regular file", path.display())));
    }
    if m.uid() != uid {
        return Err(rejected(format!(
            "The {what} {} doesn't belong to you, so DevCat's passwordless helper won't read it as root",
            path.display()
        )));
    }
    if m.len() > cap {
        return Err(rejected(format!("The {what} {} is too large", path.display())));
    }
    check_parents(path, uid)?;
    let mut data = Vec::new();
    f.take(cap + 1).read_to_end(&mut data).map_err(|e| failed(e.to_string()))?;
    if data.len() as u64 > cap {
        return Err(rejected(format!("The {what} {} is too large", path.display())));
    }
    Ok(data)
}

#[cfg(not(unix))]
pub fn read_user_file(path: &Path, _uid: u32, _cap: u64, _what: &str) -> HResult<Vec<u8>> {
    Err(rejected(format!("{}: not supported on this OS", path.display())))
}

fn utf8_text(data: Vec<u8>, what: &str) -> HResult<String> {
    let s = String::from_utf8(data).map_err(|_| rejected(format!("The {what} is not valid UTF-8 text")))?;
    if s.contains('\0') {
        return Err(rejected(format!("The {what} contains NUL bytes")));
    }
    Ok(s)
}

// ---------------------------------------------------------------- OpenVPN config validation

/// Directives that run programs, load code, write files as root or would override the
/// helper's own process setup.
const OVPN_REJECT: &[&str] = &[
    "plugin", "config", "up", "down", "route-up", "route-pre-down", "ipchange", "client-connect", "client-disconnect",
    "learn-address", "auth-user-pass-verify", "tls-verify", "tls-export-cert", "script-security", "writepid", "log",
    "log-append", "status", "management", "daemon", "cd", "chroot", "tmp-dir", "replay-persist", "ifconfig-pool-persist",
    // Beyond the obvious ones: load shared libraries / run commands / open or write paths.
    "engine", "providers", "pkcs11-providers", "iproute", "dns-updown", "tls-crypt-v2-verify", "dev-node", "capath",
    "client-config-dir", "genkey", "memstats", "port-share", "inetd",
];

/// Inline `<tag>` blocks OpenVPN accepts in a client config.
const OVPN_INLINE: &[&str] = &[
    "ca", "cert", "key", "pkcs12", "extra-certs", "tls-auth", "tls-crypt", "tls-crypt-v2", "secret", "dh", "crl-verify",
    "auth-user-pass", "http-proxy-user-pass", "connection", "peer-fingerprint",
];

/// OpenVPN's `isspace` set (other Unicode whitespace is part of a token).
fn ovpn_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// Split a config line into tokens like OpenVPN's `parse_line`: `"…"` (backslash escapes),
/// `'…'` (literal), `\x` outside single quotes, `#` / `;` at a token start = comment.
pub fn ovpn_tokens(line: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum St {
        Initial,
        Unquoted,
        Quoted,
        SQuoted,
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut st = St::Initial;
    let mut backslash = false;
    // Same state machine as OpenVPN: a backslash only marks the next character; in the
    // initial state whitespace is skipped and `#` / `;` start a comment even when escaped.
    for c in line.chars() {
        if !backslash && c == '\\' && st != St::SQuoted {
            backslash = true;
            continue;
        }
        match st {
            St::Initial => {
                if !ovpn_space(c) {
                    if c == '#' || c == ';' {
                        break;
                    }
                    if !backslash && c == '"' {
                        st = St::Quoted;
                    } else if !backslash && c == '\'' {
                        st = St::SQuoted;
                    } else {
                        cur.push(c);
                        st = St::Unquoted;
                    }
                }
            }
            St::Unquoted => {
                if !backslash && ovpn_space(c) {
                    out.push(std::mem::take(&mut cur));
                    st = St::Initial;
                } else {
                    cur.push(c);
                }
            }
            St::Quoted => {
                if !backslash && c == '"' {
                    out.push(std::mem::take(&mut cur));
                    st = St::Initial;
                } else {
                    cur.push(c);
                }
            }
            St::SQuoted => {
                if c == '\'' {
                    out.push(std::mem::take(&mut cur));
                    st = St::Initial;
                } else {
                    cur.push(c);
                }
            }
        }
        backslash = false;
    }
    if st != St::Initial {
        out.push(cur);
    }
    out
}

/// Quote a token for an OpenVPN config line.
fn ovpn_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Directive name as OpenVPN matches it, normalized: `--` prefix dropped, lowercase.
fn norm_directive(s: &str) -> String {
    s.strip_prefix("--").unwrap_or(s).to_ascii_lowercase()
}

fn is_rejected(name: &str) -> bool {
    OVPN_REJECT.contains(&name) || name.starts_with("management")
}

/// Index of the token that is a file path, if this directive (with these args) reads one.
fn file_arg(name: &str, toks: &[String]) -> HResult<Option<usize>> {
    let arg = |i: usize| toks.get(i).map(String::as_str);
    Ok(match name {
        "ca" | "cert" | "key" | "pkcs12" | "extra-certs" | "tls-auth" | "tls-crypt" | "tls-crypt-v2" | "secret"
        | "auth-gen-token-secret" | "http-proxy-user-pass" => Some(1),
        "dh" => (arg(1) != Some("none")).then_some(1),
        "crl-verify" => {
            if arg(2).is_some_and(|a| a.eq_ignore_ascii_case("dir")) {
                return Err(rejected(uses_msg("crl-verify … dir")));
            }
            Some(1)
        }
        "auth-user-pass" | "askpass" => arg(1).map(|_| 1),
        "http-proxy" => arg(3).filter(|a| !["auto", "auto-nct", "stdin"].contains(a)).map(|_| 3),
        "socks-proxy" => arg(3).filter(|a| *a != "stdin").map(|_| 3),
        _ => None,
    })
}

/// A validated OpenVPN config: rewritten text (file paths point at the private copies)
/// plus the referenced files to write there.
#[derive(Debug)]
pub struct CheckedOvpn {
    pub text: String,
    pub files: Vec<(PathBuf, Vec<u8>)>,
}

struct OvpnChecker<'a> {
    base: &'a Path,
    uid: u32,
    dest_dir: &'a Path,
    id: &'a str,
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl OvpnChecker<'_> {
    /// `Some(tokens)` when the line has to be rewritten.
    fn directive(&mut self, toks: &[String]) -> HResult<Option<Vec<String>>> {
        let name = norm_directive(&toks[0]);
        // `setenv opt <directive> …` runs <directive> if OpenVPN knows it.
        if name == "setenv" && toks.len() > 2 && toks[1].eq_ignore_ascii_case("opt") {
            return Ok(self.directive(&toks[2..])?.map(|t| toks[..2].iter().cloned().chain(t).collect()));
        }
        if is_rejected(&name) {
            return Err(rejected(uses_msg(&name)));
        }
        let Some(i) = file_arg(&name, toks)? else { return Ok(None) };
        let src = self.base.join(&toks[i]);
        let data = read_user_file(&src, self.uid, MAX_REF_FILE, &format!("`{name}` file"))?;
        if self.files.len() >= MAX_REF_FILES {
            return Err(rejected("This config references too many files for DevCat's passwordless helper"));
        }
        let dest = self.dest_dir.join(format!("{}.f{}", self.id, self.files.len() + 1));
        self.files.push((dest.clone(), data));
        let mut t = toks.to_vec();
        t[i] = dest.to_string_lossy().into_owned();
        Ok(Some(t))
    }

    fn lines(&mut self, lines: &[&str], nested: bool) -> HResult<Vec<String>> {
        let mut out = Vec::with_capacity(lines.len());
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            if line.len() > MAX_LINE {
                return Err(rejected(format!("This config has a line longer than {MAX_LINE} characters, which DevCat's passwordless helper can't check")));
            }
            let toks = ovpn_tokens(line);
            if toks.is_empty() {
                out.push(line.to_string());
                continue;
            }
            let t0 = &toks[0];
            if toks.len() == 1 && t0.len() > 2 && t0.starts_with('<') && t0.ends_with('>') && !t0.starts_with("</") {
                let tag = &t0[1..t0.len() - 1];
                let lower = tag.to_ascii_lowercase();
                if nested || !OVPN_INLINE.contains(&lower.as_str()) {
                    return Err(rejected(if is_rejected(&norm_directive(tag)) {
                        uses_msg(&lower)
                    } else {
                        format!("DevCat's passwordless helper doesn't accept an inline <{tag}> block here")
                    }));
                }
                // End the block no later than OpenVPN would (prefix match, any case).
                let close = format!("</{lower}>");
                let end = lines[i..]
                    .iter()
                    .position(|l| l.trim_start_matches(ovpn_space).to_ascii_lowercase().starts_with(&close))
                    .map(|p| i + p)
                    .ok_or_else(|| rejected(format!("This config has an unterminated <{tag}> block")))?;
                if lines[i..=end].iter().any(|l| l.len() > MAX_LINE) {
                    return Err(rejected(format!("This config has a line longer than {MAX_LINE} characters, which DevCat's passwordless helper can't check")));
                }
                out.push(line.to_string());
                if lower == "connection" {
                    out.extend(self.lines(&lines[i..end], true)?);
                } else {
                    out.extend(lines[i..end].iter().map(|l| l.to_string()));
                }
                out.push(lines[end].to_string());
                i = end + 1;
                continue;
            }
            match self.directive(&toks)? {
                None => out.push(line.to_string()),
                Some(t) => out.push(t.iter().map(|x| ovpn_quote(x)).collect::<Vec<_>>().join(" ")),
            }
        }
        Ok(out)
    }
}

/// Validate config text. Relative file paths resolve against `base` (the config's dir);
/// referenced files must belong to `uid` and are copied to `dest_dir/<id>.f<N>`.
pub fn check_ovpn_text(text: &str, base: &Path, uid: u32, dest_dir: &Path, id: &str) -> HResult<CheckedOvpn> {
    if text.contains('\0') {
        return Err(rejected("The OpenVPN config contains NUL bytes"));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut c = OvpnChecker { base, uid, dest_dir, id, files: vec![] };
    let out = c.lines(&lines, false)?;
    Ok(CheckedOvpn { text: out.join("\n"), files: c.files })
}

/// Read and validate the caller's config file.
pub fn check_ovpn_file(config: &Path, uid: u32, dest_dir: &Path, id: &str) -> HResult<CheckedOvpn> {
    let text = utf8_text(read_user_file(config, uid, MAX_CONFIG, "OpenVPN config")?, "OpenVPN config")?;
    let base = config.parent().unwrap_or(Path::new("/"));
    check_ovpn_text(&text, base, uid, dest_dir, id)
}

/// The openvpn command line built by the helper (mirrors `openvpn::build_args`).
pub fn openvpn_args(config: &Path, port: u16, mgmt_pw: &Path, pid: &Path, log: &Path, verb: Option<u8>) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a = vec![
        "--config".into(),
        s(config),
        // After --config so the config can't raise it (it's also rejected in the config).
        "--script-security".into(),
        "1".into(),
        "--management".into(),
        "127.0.0.1".into(),
        port.to_string(),
        s(mgmt_pw),
        "--management-query-passwords".into(),
        "--management-hold".into(),
        "--writepid".into(),
        s(pid),
        "--log".into(),
        s(log),
        "--daemon".into(),
    ];
    a.extend(super::openvpn::KEEP_SERVER_ROUTE.iter().map(|s| s.to_string()));
    if let Some(v) = verb {
        a.extend(["--verb".into(), v.to_string()]);
    }
    a
}

// ---------------------------------------------------------------- WireGuard config validation

const WG_REJECT: &[&str] = &["preup", "postup", "predown", "postdown", "saveconfig"];

/// Reject wg-quick hooks (they run as root through bash) and SaveConfig.
pub fn check_wg_text(text: &str) -> HResult<()> {
    if text.contains('\0') {
        return Err(rejected("The WireGuard config contains NUL bytes"));
    }
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let Some((key, _)) = line.split_once('=') else { continue };
        let key: String = key.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        if WG_REJECT.contains(&key.as_str()) {
            let shown = WG_KEYS.iter().find(|k| k.eq_ignore_ascii_case(&key)).copied().unwrap_or("");
            return Err(rejected(uses_msg(shown)));
        }
    }
    Ok(())
}
const WG_KEYS: &[&str] = &["PreUp", "PostUp", "PreDown", "PostDown", "SaveConfig"];

/// The first thing in a config an agent wrote that the helper would refuse to run as root:
/// OpenVPN directives that run programs, load code or write files (also inside `<connection>`
/// blocks and through `setenv opt`), inline blocks OpenVPN clients don't need, WireGuard's
/// PreUp / PostUp / PreDown / PostDown / SaveConfig. Nothing is read from disk.
pub fn agent_config_problem(openvpn: bool, text: &str) -> Option<String> {
    let msg = |e: HelperError| match e {
        HelperError::Rejected(m) | HelperError::Failed(m) => m,
    };
    if !openvpn {
        return check_wg_text(text).err().map(msg);
    }
    if text.contains('\0') {
        return Some("The OpenVPN config contains NUL bytes".into());
    }
    let uses = |what: &str| Some(format!("This config uses `{what}`, which can run programs or write files with admin rights; DevCat doesn't take that from an agent. Remove it, or let the user import the file in DevCat."));
    // the closing tag of the inline block we're in (its lines are data, not directives)
    let mut block: Option<String> = None;
    for line in text.split('\n') {
        if let Some(close) = &block {
            if line.trim_start_matches(ovpn_space).to_ascii_lowercase().starts_with(close.as_str()) {
                block = None;
            }
            continue;
        }
        let toks = ovpn_tokens(line);
        let Some(t0) = toks.first() else { continue };
        if toks.len() == 1 && t0.len() > 2 && t0.starts_with('<') && t0.ends_with('>') {
            let tag = t0[1..t0.len() - 1].to_ascii_lowercase();
            if let Some(end) = tag.strip_prefix('/') {
                if end == "connection" {
                    continue;
                }
                return Some(format!("This config has a stray </{end}>"));
            }
            if !OVPN_INLINE.contains(&tag.as_str()) {
                return uses(&format!("<{tag}>"));
            }
            if tag != "connection" {
                block = Some(format!("</{tag}>"));
            }
            continue;
        }
        let mut name = norm_directive(t0);
        if name == "setenv" && toks.len() > 2 && toks[1].eq_ignore_ascii_case("opt") {
            name = norm_directive(&toks[2]);
        }
        if is_rejected(&name) {
            return uses(&name);
        }
    }
    block.map(|b| format!("This config has an unterminated <{}> block", b.trim_start_matches("</").trim_end_matches('>')))
}

/// Interface name from `<iface>.conf`.
pub fn wg_iface_of(conf: &Path) -> HResult<String> {
    let name = conf.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name.strip_suffix(".conf") {
        Some(i) if valid_iface(i) => Ok(i.to_string()),
        _ => Err(rejected(format!("DevCat's passwordless helper: {} must be named <interface>.conf", conf.display()))),
    }
}

/// Read and validate the caller's WireGuard config: (interface, text).
pub fn check_wg_file(conf: &Path, uid: u32) -> HResult<(String, String)> {
    let iface = wg_iface_of(conf)?;
    let text = utf8_text(read_user_file(conf, uid, 256 * 1024, "WireGuard config")?, "WireGuard config")?;
    check_wg_text(&text)?;
    Ok((iface, text))
}

// ---------------------------------------------------------------- privileged operations

#[cfg(unix)]
mod ops {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    /// Create `p` (or accept it) as a real directory owned by `owner`, then set `mode`.
    fn ensure_dir(p: &Path, owner: u32, mode: u32) -> HResult<()> {
        match std::fs::symlink_metadata(p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(p).map_err(|e| failed(format!("{}: {e}", p.display())))?;
            }
            Err(e) => return Err(failed(format!("{}: {e}", p.display()))),
            Ok(m) => {
                if !m.file_type().is_dir() || m.uid() != owner {
                    return Err(failed(format!("{} exists but is not a directory owned by root", p.display())));
                }
            }
        }
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).map_err(|e| failed(format!("{}: {e}", p.display())))
    }

    pub fn prepare_dirs(ctx: &Ctx) -> HResult<()> {
        ensure_dir(&ctx.run_dir, ctx.root_uid, 0o755)?;
        ensure_dir(&ctx.user_dir(), ctx.root_uid, 0o755)?;
        ensure_dir(&ctx.private_dir(), ctx.root_uid, 0o700)?;
        ensure_dir(&ctx.private_dir().join("wg"), ctx.root_uid, 0o700)
    }

    fn remove(p: &Path) -> HResult<()> {
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(format!("{}: {e}", p.display()))),
            _ => Ok(()),
        }
    }

    /// Create a fresh file (never through an existing path) with `mode`.
    fn create_file(p: &Path, data: &[u8], mode: u32) -> HResult<std::fs::File> {
        remove(p)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(p)
            .map_err(|e| failed(format!("{}: {e}", p.display())))?;
        f.write_all(data).map_err(|e| failed(format!("{}: {e}", p.display())))?;
        Ok(f)
    }

    /// Remove `<private>/<id>.*`.
    fn clear_private(ctx: &Ctx, id: &str) {
        let prefix = format!("{id}.");
        if let Ok(rd) = std::fs::read_dir(ctx.private_dir()) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    fn run_tool(program: &Path, args: &[String], root_uid: u32) -> HResult<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", tool_path(root_uid))
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| failed(format!("Cannot run {}: {e}", program.display())))?;
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if out.status.success() {
            Ok(text)
        } else {
            let t = text.trim();
            Err(failed(if t.is_empty() { format!("{} exited with {}", program.display(), out.status) } else { t.to_string() }))
        }
    }

    /// pid from the helper's own pid file (root-owned, written by openvpn).
    fn read_pid(ctx: &Ctx, id: &str) -> Option<i32> {
        let p = ctx.user_dir().join(format!("{id}.pid"));
        let m = std::fs::symlink_metadata(&p).ok()?;
        if !m.file_type().is_file() || m.uid() != ctx.root_uid || m.len() > 32 {
            return None;
        }
        std::fs::read_to_string(&p).ok()?.trim().parse::<i32>().ok().filter(|&pid| pid > 1)
    }

    fn is_openvpn(pid: i32) -> bool {
        let out = std::process::Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "comm="]).env_clear().output();
        let Ok(out) = out else { return false };
        let comm = String::from_utf8_lossy(&out.stdout).trim().to_string();
        out.status.success() && Path::new(&comm).file_name().is_some_and(|n| n == "openvpn")
    }

    pub fn openvpn_stop(ctx: &Ctx, id: &str) -> HResult<String> {
        let mut msg = String::from("no running OpenVPN for this profile");
        if let Some(pid) = read_pid(ctx, id) {
            if is_openvpn(pid) {
                // SAFETY: plain syscall; pid came from our own root-owned pid file and is openvpn.
                if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                    return Err(failed(format!("Cannot stop OpenVPN (pid {pid}): {}", std::io::Error::last_os_error())));
                }
                msg = format!("sent SIGTERM to OpenVPN (pid {pid})");
            }
        }
        clear_private(ctx, id);
        Ok(msg)
    }

    pub fn openvpn_start(ctx: &Ctx, a: &StartArgs) -> HResult<String> {
        let bin = find_openvpn(ctx.root_uid).map_err(rejected)?;
        prepare_dirs(ctx)?;
        let private = ctx.private_dir();
        let checked = check_ovpn_file(&a.config, ctx.uid, &private, &a.profile_id)?;
        let mgmt_pw = read_user_file(&a.mgmt_pass_file, ctx.uid, MAX_MGMT_PW, "management password file")?;

        // A previous daemon for this profile would hold the tunnel: stop it first.
        let _ = openvpn_stop(ctx, &a.profile_id);
        let config = private.join(format!("{}.ovpn", a.profile_id));
        let mgmt = private.join(format!("{}.mgmt", a.profile_id));
        create_file(&config, checked.text.as_bytes(), 0o600)?;
        for (dest, data) in &checked.files {
            create_file(dest, data, 0o600)?;
        }
        create_file(&mgmt, &mgmt_pw, 0o600)?;

        let pid = ctx.user_dir().join(format!("{}.pid", a.profile_id));
        let log = ctx.user_dir().join(format!("{}.log", a.profile_id));
        remove(&pid)?;
        // The log belongs to the caller so DevCat can read it; the dir stays root-owned.
        let f = create_file(&log, b"", 0o600)?;
        std::os::unix::fs::fchown(&f, Some(ctx.uid), None).map_err(|e| failed(format!("{}: {e}", log.display())))?;
        drop(f);

        let args = openvpn_args(&config, a.mgmt_port, &mgmt, &pid, &log, a.verb);
        run_tool(&bin, &args, ctx.root_uid).inspect_err(|_| clear_private(ctx, &a.profile_id))
    }

    fn wg(ctx: &Ctx, conf: &Path, up: bool) -> HResult<String> {
        let (bash, wg_quick) = find_wg_tools(ctx.root_uid).map_err(rejected)?;
        prepare_dirs(ctx)?;
        let iface = wg_iface_of(conf)?;
        let copy = ctx.private_dir().join("wg").join(format!("{iface}.conf"));
        // `down` reuses the copy validated at `up` when it's still there.
        let have_copy = !up && std::fs::symlink_metadata(&copy).is_ok_and(|m| m.file_type().is_file() && m.uid() == ctx.root_uid);
        if !have_copy {
            let (_, text) = check_wg_file(conf, ctx.uid)?;
            create_file(&copy, text.as_bytes(), 0o600)?;
        }
        let args = vec![wg_quick.to_string_lossy().into_owned(), if up { "up" } else { "down" }.into(), copy.to_string_lossy().into_owned()];
        let res = run_tool(&bash, &args, ctx.root_uid);
        if (up && res.is_err()) || (!up && res.is_ok()) {
            let _ = std::fs::remove_file(&copy);
        }
        res
    }

    pub fn wg_up(ctx: &Ctx, conf: &Path) -> HResult<String> {
        wg(ctx, conf, true)
    }
    pub fn wg_down(ctx: &Ctx, conf: &Path) -> HResult<String> {
        wg(ctx, conf, false)
    }
}

/// Run one helper subcommand.
#[cfg(unix)]
pub fn dispatch(ctx: &Ctx, args: &[String]) -> HResult<String> {
    let (cmd, rest) = args.split_first().ok_or_else(|| rejected("DevCat's passwordless helper: missing command"))?;
    match cmd.as_str() {
        "version" if rest.is_empty() => Ok(format!("{}\nprotocol {PROTOCOL}\n", version())),
        "openvpn-start" => ops::openvpn_start(ctx, &parse_start_args(rest)?),
        "openvpn-stop" => ops::openvpn_stop(ctx, &parse_stop_args(rest)?),
        "wg-up" => ops::wg_up(ctx, &parse_wg_args(rest)?),
        "wg-down" => ops::wg_down(ctx, &parse_wg_args(rest)?),
        other => Err(rejected(format!("DevCat's passwordless helper: unknown command \"{other}\""))),
    }
}

/// Entry point for `devcat --vpn-helper …` (called from `main.rs`); returns the exit code.
pub fn cli_main(args: &[String]) -> i32 {
    let res = cli_run(args);
    match res {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(HelperError::Rejected(m)) => {
            eprintln!("{m}");
            EXIT_REJECTED
        }
        Err(HelperError::Failed(m)) => {
            eprintln!("{m}");
            EXIT_FAILED
        }
    }
}

#[cfg(unix)]
fn cli_run(args: &[String]) -> HResult<String> {
    // SAFETY: plain libc calls without pointers.
    unsafe { libc::umask(0o022) };
    let uid = caller_uid(std::env::var("SUDO_UID").ok().as_deref())?;
    // SAFETY: as above.
    if unsafe { libc::geteuid() } != 0 {
        return Err(rejected("DevCat's passwordless helper must be run through sudo"));
    }
    dispatch(&Ctx { uid, root_uid: 0, run_dir: PathBuf::from(RUN_DIR) }, args)
}

#[cfg(not(unix))]
fn cli_run(_args: &[String]) -> HResult<String> {
    Err(rejected("DevCat's passwordless helper is not available on this OS"))
}

/// Why this process must not start the GUI: it is the installed helper binary, or it was
/// started as root through sudo (the sudoers rule must never yield a root DevCat window).
pub fn gui_refusal() -> Option<String> {
    #[cfg(unix)]
    {
        if std::env::current_exe().is_ok_and(|p| p == Path::new(HELPER_PATH)) {
            return Some(format!("This is DevCat's VPN helper; it only runs `{FLAG}` commands."));
        }
        // SAFETY: plain libc call.
        if unsafe { libc::geteuid() } == 0 && std::env::var_os("SUDO_UID").is_some() {
            return Some("DevCat must not be started as root through sudo.".into());
        }
    }
    None
}

// ================================================================ app side

/// Usernames accepted in the sudoers rule.
pub fn valid_username(u: &str) -> bool {
    let mut cs = u.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && u.len() <= 32
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
}

pub fn sudoers_line(user: &str, helper: &str) -> String {
    format!("{user} ALL=(root) NOPASSWD: {helper} {FLAG} *")
}

/// Shell script run once with administrator rights: validate the sudoers rule first,
/// then install the helper (atomic rename) and the rule.
/// `copies` = (file shipped with DevCat, where to install it), e.g. its OpenVPN, copied the same way.
pub fn install_script(exe: &Path, copies: &[(&Path, &str)], user: &str, helper: &str, group: &str) -> Result<String, String> {
    use super::elevate::sh_quote as q;
    if !valid_username(user) {
        return Err(format!("Your account name \"{user}\" can't be used in a sudoers rule"));
    }
    let helper_p = Path::new(helper);
    let dir = helper_p.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    let name = helper_p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = format!("{dir}/.{name}.tmp");
    let sudoers_dir = Path::new(SUDOERS_PATH).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    // sudo ignores files in sudoers.d whose name contains a dot, so the temp file is inert.
    let stmp = format!("{sudoers_dir}/.dev-devcat-vpn.tmp");
    let line = sudoers_line(user, helper);
    let mut ovpn_steps: Vec<String> = Vec::new();
    for (src, dest) in copies {
        let otmp = format!("{dest}.tmp");
        let parent = Path::new(dest).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        if parent != dir {
            ovpn_steps.push(format!("mkdir -p {}", q(&parent)));
        }
        ovpn_steps.extend([
            format!("rm -f {}", q(&otmp)),
            format!("cp {} {}", q(&src.to_string_lossy()), q(&otmp)),
            format!("chown root:{group} {}", q(&otmp)),
            format!("chmod 0755 {}", q(&otmp)),
            format!("mv -f {} {}", q(&otmp), q(dest)),
        ]);
    }
    let steps: Vec<String> = [
        "set -e".to_string(),
        "umask 022".into(),
        format!("trap 'rm -f {} {}' EXIT", q(&tmp), q(&stmp)),
        format!("rm -f {}", q(&stmp)),
        format!("printf '%s\\n' {} > {}", q(&line), q(&stmp)),
        format!("if ! {VISUDO} -cf {} >/dev/null; then echo 'visudo rejected the generated sudoers rule' >&2; exit 1; fi", q(&stmp)),
        format!("mkdir -p {}", q(&dir)),
        format!("rm -f {}", q(&tmp)),
        format!("cp {} {}", q(&exe.to_string_lossy()), q(&tmp)),
        format!("chown root:{group} {}", q(&tmp)),
        format!("chmod 0755 {}", q(&tmp)),
        format!("mv -f {} {}", q(&tmp), q(helper)),
    ]
    .into_iter()
    .chain(ovpn_steps)
    .chain([
        format!("chown root:{group} {}", q(&stmp)),
        format!("chmod 0440 {}", q(&stmp)),
        format!("mv -f {} {}", q(&stmp), q(SUDOERS_PATH)),
    ])
    .collect();
    Ok(steps.join("\n"))
}

pub fn uninstall_script(helper: &str) -> String {
    use super::elevate::sh_quote as q;
    let mut s = format!("rm -f {} {}", q(SUDOERS_PATH), q(helper));
    for o in HELPER_OPENVPN.into_iter().chain(HELPER_DNS_UPDOWN) {
        s.push_str(&format!(" {}", q(o)));
    }
    if let Some(dir) = HELPER_DNS_UPDOWN.and_then(|p| Path::new(p).parent()) {
        s.push_str(&format!(" && {{ rmdir {} 2>/dev/null || true; }}", q(&dir.to_string_lossy())));
    }
    s
}

/// `version` output → (app version, protocol). Helpers before protocol 2 print only the version.
pub fn parse_version_output(out: &str) -> (Option<String>, Option<u32>) {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let version = lines.next().map(str::to_string);
    let protocol = out.lines().find_map(|l| l.trim().strip_prefix("protocol ")).and_then(|p| p.trim().parse().ok());
    (version, protocol)
}

fn root_group() -> &'static str {
    if cfg!(target_os = "macos") { "wheel" } else { "root" }
}

/// Login name of the current user (from the password database).
#[cfg(unix)]
fn current_username() -> Option<String> {
    // SAFETY: getpwuid_r writes into our buffer; the result pointer is checked before use.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0 as libc::c_char; 4096];
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        if libc::getpwuid_r(libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut res) != 0 || res.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(pw.pw_name).to_str().ok().map(str::to_string)
    }
}

#[cfg(not(unix))]
fn current_username() -> Option<String> {
    None
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperStatus {
    /// macOS / Linux.
    pub supported: bool,
    /// Helper is root-owned and not writable by others, and the sudoers rule exists.
    pub installed: bool,
    /// `sudo -n` works without a prompt and the helper speaks this app's [`PROTOCOL`].
    pub up_to_date: bool,
    pub helper_version: Option<String>,
    /// This DevCat ships an OpenVPN (macOS) that the helper doesn't have yet (or has an older
    /// copy of): "Update helper" installs it.
    pub openvpn_update: bool,
    pub app_version: String,
    pub helper_path: String,
    pub sudoers_path: String,
    /// The rule `net_helper_install` writes (None if the username can't be used).
    pub sudoers_line: Option<String>,
    /// Why the helper can't run OpenVPN / WireGuard here (None = it can).
    pub openvpn_issue: Option<String>,
    pub wireguard_issue: Option<String>,
}

/// Whether connects should go through the helper (None = not checked yet).
static READY: parking_lot::Mutex<Option<bool>> = parking_lot::Mutex::new(None);

fn same_file_contents(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y),
        _ => false,
    }
}

fn sudo_version() -> Option<(Option<String>, Option<u32>)> {
    let out = std::process::Command::new(SUDO)
        .args(["-n", HELPER_PATH, FLAG, "version"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| parse_version_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Full status (blocking: runs `sudo -n … version`). Refreshes the cached readiness.
pub fn status() -> HelperStatus {
    let supported = cfg!(any(target_os = "macos", target_os = "linux"));
    let mut s = HelperStatus {
        supported,
        installed: false,
        up_to_date: false,
        helper_version: None,
        openvpn_update: false,
        app_version: version().to_string(),
        helper_path: HELPER_PATH.into(),
        sudoers_path: SUDOERS_PATH.into(),
        sudoers_line: current_username().filter(|u| valid_username(u)).map(|u| sudoers_line(&u, HELPER_PATH)),
        openvpn_issue: None,
        wireguard_issue: None,
    };
    if !supported {
        return s;
    }
    s.installed = trusted_path(Path::new(HELPER_PATH), 0).is_ok() && Path::new(SUDOERS_PATH).exists();
    if s.installed {
        let (v, protocol) = sudo_version().unwrap_or((None, None));
        s.helper_version = v;
        s.up_to_date = protocol == Some(PROTOCOL);
    }
    let stale = |bundled: Option<PathBuf>, copy: Option<&str>| matches!((bundled, copy), (Some(b), Some(h)) if !same_file_contents(&b, Path::new(h)));
    s.openvpn_update = s.installed
        && (stale(super::tools::bundled_openvpn(), HELPER_OPENVPN) || stale(super::tools::bundled_dns_updown(), HELPER_DNS_UPDOWN));
    s.openvpn_issue = find_openvpn(0).err();
    if s.openvpn_issue.is_some() && s.openvpn_update {
        s.openvpn_issue = Some("Click \"Update helper\" to use the OpenVPN that comes with DevCat".into());
    }
    s.wireguard_issue = find_wg_tools(0).err();
    *READY.lock() = Some(s.up_to_date);
    s
}

/// Forget the cached readiness (after install / uninstall).
pub fn reset() {
    *READY.lock() = None;
}

/// Should this connect use the helper? Checks once, then uses the cached answer.
pub async fn ready() -> bool {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return false;
    }
    if let Some(r) = *READY.lock() {
        return r;
    }
    tokio::task::spawn_blocking(|| status().up_to_date).await.unwrap_or(false)
}

pub enum Outcome {
    /// Exit 0: the helper's output.
    Done(String),
    /// The helper refused (message meant for the user).
    Rejected(String),
    /// The helper ran but the tool failed.
    Failed(String),
    /// `sudo -n` didn't run the helper (rule removed, helper missing / changed…).
    Unavailable(String),
}

/// `sudo -n <helper> --vpn-helper <args…>`
pub async fn run(args: &[String]) -> Outcome {
    let out = tokio::process::Command::new(SUDO)
        .arg("-n")
        .arg(HELPER_PATH)
        .arg(FLAG)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(false)
        .output()
        .await;
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            reset();
            return Outcome::Unavailable(format!("cannot run sudo: {e}"));
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match out.status.code() {
        Some(0) => Outcome::Done(stdout),
        Some(EXIT_REJECTED) => Outcome::Rejected(stderr),
        Some(EXIT_FAILED) => Outcome::Failed(if stderr.is_empty() { stdout.trim().to_string() } else { stderr }),
        _ => {
            *READY.lock() = Some(false);
            Outcome::Unavailable(if stderr.is_empty() { format!("sudo exited with {}", out.status) } else { stderr })
        }
    }
}

/// Marker the UI looks for to offer "Connect with password prompt".
pub const REJECTION_MARK: &str = "passwordless helper";

/// A helper refusal as shown to the user (always contains [`REJECTION_MARK`]).
pub fn rejection(msg: &str) -> String {
    let msg = msg.trim();
    if msg.contains(REJECTION_MARK) {
        msg.to_string()
    } else {
        format!("DevCat's passwordless helper refused this connection: {msg}")
    }
}

/// Paths of the helper-managed pid / log files for a profile (readable by the user).
pub fn run_files(id: &str) -> (PathBuf, PathBuf) {
    #[cfg(unix)]
    // SAFETY: plain libc call.
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0u32;
    let dir = Path::new(RUN_DIR).join(uid.to_string());
    (dir.join(format!("{id}.pid")), dir.join(format!("{id}.log")))
}

/// Install (or update) the helper and the sudoers rule with one administrator prompt.
pub async fn install() -> crate::error::AppResult<HelperStatus> {
    use crate::error::AppError;
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(AppError::msg("The passwordless VPN helper is only available on macOS and Linux"));
    }
    let exe = std::env::current_exe().map_err(|e| AppError::msg(format!("Cannot locate the DevCat binary: {e}")))?;
    let user = current_username().ok_or_else(|| AppError::msg("Cannot determine your account name"))?;
    let (openvpn, dns) = (super::tools::bundled_openvpn(), super::tools::bundled_dns_updown());
    let copies: Vec<(&Path, &str)> = [openvpn.as_deref().zip(HELPER_OPENVPN), dns.as_deref().zip(HELPER_DNS_UPDOWN)].into_iter().flatten().collect();
    let script = install_script(&exe, &copies, &user, HELPER_PATH, root_group()).map_err(AppError::msg)?;
    let out = super::elevate::run("/bin/sh", &["-c".into(), script], true, None).await?;
    reset();
    if !out.success {
        return Err(AppError::msg(format!("Setting up passwordless VPN failed: {}", out.message())));
    }
    crate::rt::spawn_blocking(status).await.map_err(|e| AppError::msg(e.to_string()))
}

pub async fn uninstall() -> crate::error::AppResult<HelperStatus> {
    use crate::error::AppError;
    let out = super::elevate::run("/bin/sh", &["-c".into(), uninstall_script(HELPER_PATH)], true, None).await?;
    reset();
    if !out.success {
        return Err(AppError::msg(format!("Removing passwordless VPN failed: {}", out.message())));
    }
    crate::rt::spawn_blocking(status).await.map_err(|e| AppError::msg(e.to_string()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn uid() -> u32 {
        // SAFETY: plain libc call.
        unsafe { libc::getuid() }
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!("devcat-helper-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).unwrap();
            // Canonical (macOS: /var → /private/var) so path comparisons are stable.
            Tmp(std::fs::canonicalize(d).unwrap())
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, text).unwrap();
            p
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn check(dir: &Tmp, text: &str) -> HResult<CheckedOvpn> {
        check_ovpn_text(text, &dir.0, uid(), Path::new("/run/x/private"), "p1")
    }

    fn assert_rejected(r: HResult<CheckedOvpn>, needle: &str) {
        match r {
            Err(HelperError::Rejected(m)) => assert!(m.contains(needle), "message {m:?} lacks {needle:?}"),
            Err(e) => panic!("expected rejection containing {needle:?}, got {e:?}"),
            Ok(c) => panic!("expected rejection containing {needle:?}, got accepted:\n{}", c.text),
        }
    }

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIU\n-----END CERTIFICATE-----";

    #[test]
    fn tokenizer_mirrors_openvpn() {
        assert_eq!(ovpn_tokens("remote vpn.example.com 1194 udp"), s(&["remote", "vpn.example.com", "1194", "udp"]));
        assert_eq!(ovpn_tokens("  # comment"), Vec::<String>::new());
        assert_eq!(ovpn_tokens("; comment"), Vec::<String>::new());
        assert_eq!(ovpn_tokens("verb 3 # trailing"), s(&["verb", "3"]));
        assert_eq!(ovpn_tokens(r#""up" '/tmp/a b' "c\"d""#), s(&["up", "/tmp/a b", "c\"d"]));
        assert_eq!(ovpn_tokens(r"u\p x\ y"), s(&["up", "x y"]));
        assert_eq!(ovpn_tokens("a#b"), s(&["a#b"]));
        assert_eq!(ovpn_tokens(r"\ up /x"), s(&["up", "/x"]));
        assert_eq!(ovpn_tokens(r"\#up"), Vec::<String>::new());
        assert_eq!(ovpn_tokens("\tup\r"), s(&["up"]));
        assert_eq!(ovpn_quote("/a b/\"c"), r#""/a b/\"c""#);
        assert_eq!(ovpn_tokens(&ovpn_quote("/a b/\"c\\")), s(&["/a b/\"c\\"]));
    }

    #[test]
    fn accepts_typical_client_config() {
        let d = Tmp::new();
        let text = format!(
            "client\ndev tun\nproto udp\nremote vpn.example.com 1194\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\n\
             remote-cert-tls server\ncipher AES-256-GCM\nauth-user-pass\nverb 3\nsetenv opt block-outside-dns\n\
             # up /etc/evil.sh  (commented out)\n; plugin /x.so\n\
             <ca>\n{CERT}\n</ca>\n<cert>\n{CERT}\n</cert>\n<key>\nup /not/a/directive\n</key>\nkey-direction 1\n\
             <tls-auth>\n-----BEGIN OpenVPN Static key V1-----\nabc\n-----END OpenVPN Static key V1-----\n</tls-auth>\n\
             <connection>\nremote alt.example.com 443 tcp\n</connection>\ndh none\n"
        );
        let c = check(&d, &text).unwrap();
        assert!(c.files.is_empty());
        assert_eq!(c.text, text);
    }

    #[test]
    fn rejects_dangerous_directives() {
        let d = Tmp::new();
        for name in OVPN_REJECT {
            assert_rejected(check(&d, &format!("client\n{name} x\n")), &format!("`{name}`"));
        }
        for line in [
            "UP /tmp/x.sh",
            "  --up /tmp/x.sh",
            "\t--Plugin /tmp/x.so",
            "\"up\" /tmp/x.sh",
            "u\\p /tmp/x.sh",
            "\\ up /tmp/x.sh",
            "management-client-auth",
            "management-external-key",
            "setenv opt up /tmp/x.sh",
            "setenv OPT --route-up /tmp/x.sh",
            "Script-Security 2",
            "dns-updown /tmp/x.sh",
        ] {
            let r = check(&d, &format!("remote h 1194\n{line}\n"));
            assert!(matches!(r, Err(HelperError::Rejected(ref m)) if m.contains("passwordless helper")), "{line:?} → {r:?}");
        }
        // Inside a <connection> block, too.
        assert_rejected(check(&d, "<connection>\nremote h 1\nup /x\n</connection>\n"), "`up`");
        // Inline blocks for rejected / unknown options.
        assert_rejected(check(&d, "<up>\n/bin/sh\n</up>\n"), "`up`");
        assert_rejected(check(&d, "<foo>\nx\n</foo>\n"), "inline <foo>");
        assert_rejected(check(&d, "<ca>\nabc\n"), "unterminated");
        // A close tag ends the block as early as OpenVPN would (prefix, any case).
        assert_rejected(check(&d, "<ca>\nabc\n  </CA>junk\nup /x\n</ca>\n"), "`up`");
        assert_rejected(check(&d, &format!("remote {}\n", "a".repeat(300))), "longer than");
        assert_rejected(check(&d, "remote h\0\n"), "NUL");
        assert_rejected(check(&d, "crl-verify crl dir\n"), "crl-verify");
    }

    #[test]
    fn exact_message() {
        let d = Tmp::new();
        assert_eq!(
            check(&d, "up x").unwrap_err(),
            HelperError::Rejected("This config uses `up`, which DevCat's passwordless helper won't run as root. Remove it, or connect with a password prompt.".into())
        );
    }

    #[test]
    fn file_directives_are_copied_and_rewritten() {
        let d = Tmp::new();
        std::fs::create_dir(d.0.join("keys")).unwrap();
        d.write("keys/ca.crt", CERT);
        d.write("ta key", "static");
        let abs = d.write("creds.txt", "u\np\n");
        let text = format!("ca keys/ca.crt\ntls-auth \"ta key\" 1\nauth-user-pass {}\ndh none\n", abs.display());
        let c = check(&d, &text).unwrap();
        assert_eq!(c.files.len(), 3);
        assert_eq!(c.files[0], (PathBuf::from("/run/x/private/p1.f1"), CERT.as_bytes().to_vec()));
        assert_eq!(c.files[1].1, b"static");
        assert_eq!(
            c.text,
            "ca /run/x/private/p1.f1\ntls-auth /run/x/private/p1.f2 1\nauth-user-pass /run/x/private/p1.f3\ndh none\n"
        );
        // http-proxy authfile (4th token), inside <connection>.
        d.write("proxy.auth", "u\np\n");
        let c = check(&d, "<connection>\nhttp-proxy proxy 8080 proxy.auth basic\n</connection>\n").unwrap();
        assert_eq!(c.text, "<connection>\nhttp-proxy proxy 8080 /run/x/private/p1.f1 basic\n</connection>\n");
        assert!(check(&d, "http-proxy proxy 8080 auto-nct\n").unwrap().files.is_empty());
    }

    #[test]
    fn file_directives_must_be_owned_regular_files() {
        let d = Tmp::new();
        // Not owned by the caller (root-owned system file).
        assert_rejected(check(&d, "ca /etc/hosts\n"), "doesn't belong to you");
        assert_rejected(check(&d, "secret /etc/hosts\n"), "doesn't belong to you");
        // Owned by someone else from the helper's point of view (injected uid).
        d.write("ca.crt", CERT);
        assert_rejected(check_ovpn_text("ca ca.crt\n", &d.0, uid() + 1, Path::new("/x"), "p"), "doesn't belong to you");
        // Symlink as the final component.
        symlink(d.0.join("ca.crt"), d.0.join("link.crt")).unwrap();
        assert_rejected(check(&d, "ca link.crt\n"), "symbolic link");
        // Relative paths resolve against the config's dir, not the cwd.
        assert_rejected(check_ovpn_text("ca ca.crt\n", Path::new("/"), uid(), Path::new("/x"), "p"), "can't open");
        assert!(check(&d, "ca ca.crt\n").is_ok());
        // Directories and missing files.
        assert_rejected(check(&d, "cert .\n"), "not a regular file");
        assert_rejected(check(&d, "key missing.key\n"), "can't open");
        assert_rejected(check(&d, "auth-user-pass ../../../../../../../etc/master.passwd\n"), "");
    }

    #[test]
    fn config_file_checks() {
        let d = Tmp::new();
        let cfg = d.write("p.ovpn", "client\nremote h 1194\n");
        let c = check_ovpn_file(&cfg, uid(), Path::new("/x"), "p").unwrap();
        assert_eq!(c.text, "client\nremote h 1194\n");
        assert!(matches!(check_ovpn_file(&cfg, uid() + 1, Path::new("/x"), "p"), Err(HelperError::Rejected(_))));
        symlink(&cfg, d.0.join("l.ovpn")).unwrap();
        assert!(matches!(check_ovpn_file(&d.0.join("l.ovpn"), uid(), Path::new("/x"), "p"), Err(HelperError::Rejected(m)) if m.contains("symbolic link")));
        assert!(matches!(check_ovpn_file(Path::new("/etc/hosts"), uid(), Path::new("/x"), "p"), Err(HelperError::Rejected(_))));
        // A parent dir writable by others.
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir(d.0.join("open")).unwrap();
        let inner = d.write("open/p.ovpn", "client\n");
        std::fs::set_permissions(d.0.join("open"), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(check_ovpn_file(&inner, uid(), Path::new("/x"), "p"), Err(HelperError::Rejected(m)) if m.contains("other users")));
    }

    #[test]
    fn wireguard_validation() {
        let ok = "[Interface]\nPrivateKey = abc=\nAddress = 10.0.0.2/32\nDNS = 1.1.1.1\n# PostUp = evil\n[Peer]\nPublicKey = x=\nEndpoint = h:51820\nAllowedIPs = 0.0.0.0/0\n";
        assert!(check_wg_text(ok).is_ok());
        for bad in ["PostUp = sh -c x", "postup=x", "  PREUP\t= x", "PreDown = x", "PostDown=x # c", "SaveConfig = true", "Post Up = x"] {
            let r = check_wg_text(&format!("[Interface]\nPrivateKey = a\n{bad}\n"));
            assert!(matches!(r, Err(HelperError::Rejected(ref m)) if m.contains("passwordless helper")), "{bad:?} → {r:?}");
        }
        assert!(matches!(check_wg_text("PostUp = x"), Err(HelperError::Rejected(m)) if m.contains("`PostUp`")));

        assert_eq!(wg_iface_of(Path::new("/a/dc3f2b1c9e.conf")).unwrap(), "dc3f2b1c9e");
        assert!(wg_iface_of(Path::new("/a/x.ovpn")).is_err());
        assert!(wg_iface_of(Path::new("/a/this-name-is-too-long.conf")).is_err());
        assert!(wg_iface_of(Path::new("/a/a b.conf")).is_err());
        assert!(wg_iface_of(Path::new("/a/.conf")).is_err());

        let d = Tmp::new();
        let conf = d.write("dc12345678.conf", ok);
        assert_eq!(check_wg_file(&conf, uid()).unwrap().0, "dc12345678");
        assert!(check_wg_file(&conf, uid() + 1).is_err());
        let bad = d.write("dcbad.conf", "[Interface]\nPostUp = id\n");
        assert!(check_wg_file(&bad, uid()).is_err());
        symlink(&conf, d.0.join("dclink.conf")).unwrap();
        assert!(check_wg_file(&d.0.join("dclink.conf"), uid()).is_err());
    }

    #[test]
    fn argv_parsing() {
        assert_eq!(caller_uid(None), Err(rejected("DevCat's passwordless helper must be run through sudo (SUDO_UID is not set)")));
        assert!(caller_uid(Some("")).is_err());
        assert!(caller_uid(Some("0")).is_err());
        assert!(caller_uid(Some("-1")).is_err());
        assert!(caller_uid(Some("abc")).is_err());
        assert_eq!(caller_uid(Some("501")), Ok(501));

        let good = s(&["--config", "/a/p.ovpn", "--mgmt-port", "40000", "--mgmt-pass-file", "/a/p.mgmt", "--profile-id", "3f2b-1c_9E"]);
        let a = parse_start_args(&good).unwrap();
        assert_eq!(a.mgmt_port, 40000);
        assert_eq!(a.profile_id, "3f2b-1c_9E");
        assert_eq!(a.verb, None);
        let with = |k: &str, v: &str| {
            let mut g = good.clone();
            match g.iter().position(|x| x == k) {
                Some(i) => g[i + 1] = v.into(),
                None => g.extend([k.to_string(), v.to_string()]),
            }
            parse_start_args(&g)
        };
        for port in ["0", "80", "1023", "65536", "-1", "x", ""] {
            assert!(with("--mgmt-port", port).is_err(), "port {port}");
        }
        assert!(with("--mgmt-port", "1024").is_ok());
        assert!(with("--mgmt-port", "65535").is_ok());
        for id in ["", "../x", "a b", "a/b", "a.b", "x".repeat(65).as_str(), "id;rm"] {
            assert!(with("--profile-id", id).is_err(), "id {id:?}");
            assert!(parse_stop_args(&s(&["--profile-id", id])).is_err());
        }
        assert_eq!(with("--verb", "4").unwrap().verb, Some(4));
        assert!(with("--verb", "12").is_err());
        assert!(with("--config", "rel/p.ovpn").is_err());
        let mut extra = good.clone();
        extra.extend(s(&["--up", "/x"]));
        assert!(parse_start_args(&extra).is_err());
        let mut dup = good.clone();
        dup.extend(s(&["--config", "/b"]));
        assert!(parse_start_args(&dup).is_err());
        assert!(parse_start_args(&good[..good.len() - 1]).is_err());
        assert!(parse_wg_args(&s(&["--conf", "x.conf"])).is_err());
        assert!(parse_wg_args(&s(&["--conf", "/x/dc1.conf"])).is_ok());

        let ctx = Ctx { uid: 501, root_uid: 0, run_dir: "/nonexistent".into() };
        assert_eq!(dispatch(&ctx, &s(&["version"])).unwrap(), format!("{}\nprotocol {PROTOCOL}\n", version()));
        assert!(dispatch(&ctx, &s(&["version", "x"])).is_err());
        assert!(dispatch(&ctx, &s(&["sh"])).is_err());
        assert!(dispatch(&ctx, &[]).is_err());
    }

    #[test]
    fn openvpn_command_line() {
        let a = openvpn_args(Path::new("/r/p/x.ovpn"), 40000, Path::new("/r/p/x.mgmt"), Path::new("/r/x.pid"), Path::new("/r/x.log"), Some(4));
        assert_eq!(
            a,
            s(&[
                "--config", "/r/p/x.ovpn", "--script-security", "1", "--management", "127.0.0.1", "40000", "/r/p/x.mgmt",
                "--management-query-passwords", "--management-hold", "--writepid", "/r/x.pid", "--log", "/r/x.log", "--daemon",
                "--route", "remote_host", "255.255.255.255", "net_gateway", "--verb", "4",
            ])
        );
        let cfg = a.iter().position(|x| x == "--config").unwrap();
        assert!(a.iter().position(|x| x == "--script-security").unwrap() > cfg);
    }

    #[test]
    fn trusted_tools() {
        let d = Tmp::new();
        let bin = d.write("tool", "#!/bin/sh\n");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Owned by the test user, so it's untrusted for root…
        let me = uid();
        if me != 0 {
            assert!(trusted_path(&bin, 0).is_err());
        }
        // …and trusted when that uid stands in for root (macOS temp dirs aren't group-writable).
        if cfg!(target_os = "macos") {
            assert!(find_trusted(&[bin.to_str().unwrap()], me).is_ok());
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o775)).unwrap();
            assert!(find_trusted(&[bin.to_str().unwrap()], me).unwrap_err().contains("writable by group"));
        }
        assert!(find_trusted(&["/definitely/not/here"], 0).unwrap_err().contains("not found"));
        // An untrusted install listed first (Homebrew) is skipped for a trusted later one (MacPorts).
        if cfg!(target_os = "macos") {
            let good = d.write("good", "#!/bin/sh\n");
            std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();
            let found = find_trusted(&[bin.to_str().unwrap(), good.to_str().unwrap()], me).unwrap();
            assert_eq!(found, std::fs::canonicalize(&good).unwrap());
        }
    }

    #[test]
    fn helper_dirs_and_stop() {
        let d = Tmp::new();
        let ctx = Ctx { uid: 4242, root_uid: uid(), run_dir: d.0.join("run") };
        ops::prepare_dirs(&ctx).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&ctx.user_dir()), 0o755);
        assert_eq!(mode(&ctx.private_dir()), 0o700);
        std::fs::write(ctx.private_dir().join("p1.ovpn"), "x").unwrap();
        std::fs::write(ctx.private_dir().join("p10.ovpn"), "x").unwrap();
        // A pid file pointing at a non-openvpn process is not killed; copies are cleaned up.
        std::fs::write(ctx.user_dir().join("p1.pid"), format!("{}\n", std::process::id())).unwrap();
        assert_eq!(ops::openvpn_stop(&ctx, "p1").unwrap(), "no running OpenVPN for this profile");
        assert!(!ctx.private_dir().join("p1.ovpn").exists());
        assert!(ctx.private_dir().join("p10.ovpn").exists());
        // A symlinked dir in place of the helper's dir is refused.
        let ctx2 = Ctx { uid: 4243, root_uid: uid(), run_dir: d.0.join("run") };
        symlink(d.0.join("elsewhere"), ctx2.user_dir()).unwrap();
        assert!(matches!(ops::prepare_dirs(&ctx2), Err(HelperError::Failed(_))));
    }

    #[test]
    fn sudoers_and_usernames() {
        for u in ["arif", "_svc", "a.b-c_d9", "x"] {
            assert!(valid_username(u), "{u}");
        }
        for u in ["", "Arif", "9lives", "a b", "a,b", "root ALL=(ALL) ALL", "a\nb", "a:b", "-x", "a".repeat(33).as_str()] {
            assert!(!valid_username(u), "{u:?}");
        }
        assert_eq!(
            sudoers_line("arif", "/Library/PrivilegedHelperTools/dev.devcat.vpn-helper"),
            "arif ALL=(root) NOPASSWD: /Library/PrivilegedHelperTools/dev.devcat.vpn-helper --vpn-helper *"
        );
    }

    #[test]
    fn version_output_and_protocol() {
        assert_eq!(parse_version_output("0.3.4\nprotocol 2\n"), (Some("0.3.4".into()), Some(2)));
        // helpers from DevCat ≤ 0.3.3 print only the version: they need an update once
        assert_eq!(parse_version_output("0.3.3\n"), (Some("0.3.3".into()), None));
        assert_eq!(parse_version_output(""), (None, None));
        let ctx = Ctx { uid: uid(), root_uid: uid(), run_dir: std::env::temp_dir() };
        let out = dispatch(&ctx, &["version".into()]).unwrap();
        assert_eq!(parse_version_output(&out), (Some(version().to_string()), Some(PROTOCOL)));
    }

    #[test]
    fn install_script_copies_bundled_openvpn() {
        let exe = Path::new("/Applications/DevCat.app/Contents/MacOS/devcat");
        let src = Path::new("/Applications/DevCat.app/Contents/Resources/vpn/openvpn");
        let dest = "/Library/PrivilegedHelperTools/dev.devcat.vpn-openvpn";
        let s = install_script(exe, &[(src, dest)], "arif", "/Library/PrivilegedHelperTools/dev.devcat.vpn-helper", "wheel").unwrap();
        for l in [
            format!("cp /Applications/DevCat.app/Contents/Resources/vpn/openvpn {dest}.tmp"),
            format!("chown root:wheel {dest}.tmp"),
            format!("chmod 0755 {dest}.tmp"),
            format!("mv -f {dest}.tmp {dest}"),
        ] {
            assert!(s.lines().any(|x| x == l), "missing: {l}\n{s}");
        }
        // checked by visudo first; the sudoers rule goes live last, after both binaries
        let pos = |needle: &str| s.find(needle).unwrap();
        assert!(pos("visudo") < pos("cp "));
        assert!(pos(&format!("mv -f {dest}.tmp")) < pos("mv -f /etc/sudoers.d/.dev-devcat-vpn.tmp"));
        assert!(!install_script(exe, &[], "arif", "/h/x", "wheel").unwrap().contains("vpn-openvpn"));
    }

    #[test]
    fn install_script_copies_dns_script_into_its_folder() {
        let exe = Path::new("/Applications/DevCat.app/Contents/MacOS/devcat");
        let src = Path::new("/Applications/DevCat.app/Contents/Resources/vpn/dns-updown");
        let dest = "/Library/PrivilegedHelperTools/dev.devcat.vpn-scripts/dns-updown";
        let s = install_script(exe, &[(src, dest)], "arif", "/Library/PrivilegedHelperTools/dev.devcat.vpn-helper", "wheel").unwrap();
        let pos = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("missing: {needle}\n{s}"));
        assert!(pos("mkdir -p /Library/PrivilegedHelperTools/dev.devcat.vpn-scripts\n") < pos(&format!("cp {} {dest}.tmp", src.display())));
        assert!(pos(&format!("chmod 0755 {dest}.tmp")) < pos(&format!("mv -f {dest}.tmp {dest}")));
        assert!(pos(&format!("mv -f {dest}.tmp {dest}")) < pos("mv -f /etc/sudoers.d/.dev-devcat-vpn.tmp"));
    }

    #[test]
    fn install_script_text() {
        let exe = Path::new("/Applications/Dev Cat.app/Contents/MacOS/devcat");
        let s = install_script(exe, &[], "arif", "/Library/PrivilegedHelperTools/dev.devcat.vpn-helper", "wheel").unwrap();
        assert_eq!(
            s,
            "set -e\n\
             umask 022\n\
             trap 'rm -f /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp /etc/sudoers.d/.dev-devcat-vpn.tmp' EXIT\n\
             rm -f /etc/sudoers.d/.dev-devcat-vpn.tmp\n\
             printf '%s\\n' 'arif ALL=(root) NOPASSWD: /Library/PrivilegedHelperTools/dev.devcat.vpn-helper --vpn-helper *' > /etc/sudoers.d/.dev-devcat-vpn.tmp\n\
             if ! /usr/sbin/visudo -cf /etc/sudoers.d/.dev-devcat-vpn.tmp >/dev/null; then echo 'visudo rejected the generated sudoers rule' >&2; exit 1; fi\n\
             mkdir -p /Library/PrivilegedHelperTools\n\
             rm -f /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp\n\
             cp '/Applications/Dev Cat.app/Contents/MacOS/devcat' /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp\n\
             chown root:wheel /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp\n\
             chmod 0755 /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp\n\
             mv -f /Library/PrivilegedHelperTools/.dev.devcat.vpn-helper.tmp /Library/PrivilegedHelperTools/dev.devcat.vpn-helper\n\
             chown root:wheel /etc/sudoers.d/.dev-devcat-vpn.tmp\n\
             chmod 0440 /etc/sudoers.d/.dev-devcat-vpn.tmp\n\
             mv -f /etc/sudoers.d/.dev-devcat-vpn.tmp /etc/sudoers.d/dev-devcat-vpn"
        );
        // visudo runs before anything is installed.
        assert!(s.find("visudo").unwrap() < s.find("cp ").unwrap());
        let linux = install_script(Path::new("/home/me/.local/bin/devcat"), &[], "me", "/usr/local/libexec/devcat-vpn-helper", "root").unwrap();
        assert!(linux.contains("chown root:root /usr/local/libexec/.devcat-vpn-helper.tmp"));
        assert!(linux.contains("mkdir -p /usr/local/libexec\n"));
        // Hostile names never reach the script.
        assert!(install_script(exe, &[], "x ALL=(ALL) NOPASSWD: ALL\n#", "/h", "wheel").is_err());
        assert!(install_script(exe, &[], "$(id)", "/h", "wheel").is_err());
        // Exe paths with quotes stay one shell word.
        let q = install_script(Path::new("/tmp/it's/devcat"), &[], "me", "/h/x", "root").unwrap();
        assert!(q.contains(r"cp '/tmp/it'\''s/devcat' /h/.x.tmp"));
        assert!(uninstall_script("/usr/local/libexec/devcat-vpn-helper").starts_with("rm -f /etc/sudoers.d/dev-devcat-vpn /usr/local/libexec/devcat-vpn-helper"));
        if let (Some(o), Some(d)) = (HELPER_OPENVPN, HELPER_DNS_UPDOWN) {
            let u = uninstall_script(HELPER_PATH);
            assert!(u.contains(&format!(" {o} {d} && ")), "uninstall removes DevCat's OpenVPN and DNS script copies: {u}");
            assert!(u.ends_with("rmdir /Library/PrivilegedHelperTools/dev.devcat.vpn-scripts 2>/dev/null || true; }"), "{u}");
        }
    }

    #[test]
    fn app_version_source() {
        assert!(!version().is_empty());
        assert!(version().chars().next().unwrap().is_ascii_digit());
    }
}
