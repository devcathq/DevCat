//! Another DevCat on this computer holding the remote-access port: typically a `devcat serve`
//! started before an update, in a terminal that's gone or in the background, which nothing can
//! reach to type `quit`. `devcat serve` notes itself in `remote/serve.json` (for `devcat stop`)
//! and watches `remote/serve.stop`; DevCats from before that are found by the port they hold.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunFile {
    pid: u32,
    version: String,
    port: u16,
}

/// A process holding the port (or noted in the run file).
#[derive(Debug, Clone)]
pub struct Holder {
    pub pid: u32,
    /// The executable's name.
    pub name: String,
    /// DevCat (the app or `devcat serve`), not something else.
    pub devcat: bool,
    /// `devcat serve` (else the app, when `devcat`).
    pub serve: bool,
    /// From the run file; DevCats before it don't say.
    pub version: Option<String>,
}

impl Holder {
    /// "DevCat 0.9.0 (`devcat serve`, process 123)".
    pub fn describe(&self) -> String {
        if !self.devcat {
            return format!("{} (process {})", self.name, self.pid);
        }
        let v = self.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        let what = if self.serve { "`devcat serve`" } else { "the app" };
        format!("DevCat{v} ({what}, process {})", self.pid)
    }
}

/// A bind error saying the port is taken (Windows: "Only one usage of each socket address", 10048).
pub fn port_taken(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("in use") || e.contains("only one usage") || e.contains("10048")
}

fn run_file(dir: &Path) -> PathBuf {
    dir.join("remote").join("serve.json")
}

fn stop_file(dir: &Path) -> PathBuf {
    dir.join("remote").join("serve.stop")
}

/// `devcat serve` is running here (call once it's listening).
pub fn note_serving(dir: &Path, port: u16) {
    let _ = std::fs::remove_file(stop_file(dir));
    let r = RunFile { pid: std::process::id(), version: env!("DEVCAT_VERSION").into(), port };
    if let Ok(b) = serde_json::to_vec(&r) {
        let _ = std::fs::write(run_file(dir), b);
    }
}

/// `devcat serve` stopped.
pub fn note_stopped(dir: &Path) {
    if read_run_file(dir).is_some_and(|r| r.pid == std::process::id()) {
        let _ = std::fs::remove_file(run_file(dir));
    }
    let _ = std::fs::remove_file(stop_file(dir));
}

/// `devcat stop` asked this `devcat serve` to stop.
pub fn stop_requested(dir: &Path) -> bool {
    let f = stop_file(dir);
    std::fs::read_to_string(&f).is_ok_and(|pid| pid.trim() == std::process::id().to_string())
}

fn read_run_file(dir: &Path) -> Option<RunFile> {
    serde_json::from_slice(&std::fs::read(run_file(dir)).ok()?).ok()
}

/// Who holds `port`, or failing that the `devcat serve` noted in `dir` (another port).
pub fn find(dir: &Path, port: u16) -> Option<Holder> {
    let me = std::process::id();
    let noted = read_run_file(dir).filter(|r| r.pid != me);
    let mut pids = port_pids(port);
    pids.retain(|p| *p != me);
    // Prefer a DevCat when several processes share the port.
    let mut found: Vec<Holder> = pids.into_iter().filter_map(process).collect();
    found.sort_by_key(|h| !h.devcat);
    let mut h = found.into_iter().next().or_else(|| noted.as_ref().and_then(|r| process(r.pid)).filter(|h| h.devcat))?;
    if let Some(r) = noted.filter(|r| r.pid == h.pid) {
        h.version = Some(r.version);
        h.serve = true;
    }
    Some(h)
}

/// Stop a DevCat: politely (it stops its VPNs, terminals and agents first), then, with `force`
/// or when there's no polite way, kill it.
pub fn stop(dir: &Path, h: &Holder, force: bool) -> Result<(), String> {
    if !h.devcat {
        return Err(format!("{} isn't DevCat: not stopping it", h.describe()));
    }
    // Noted in the run file: a `devcat serve` that watches the stop file (Windows has no SIGTERM).
    let watches = h.version.is_some();
    if watches {
        let _ = std::fs::write(stop_file(dir), h.pid.to_string());
    }
    let polite = terminate(h.pid, false) || watches;
    if polite && wait_gone(h.pid, Duration::from_secs(15)) {
        return Ok(());
    }
    if polite && !force {
        return Err(format!("{} didn't stop within 15 seconds. `devcat stop --force` kills it.", h.describe()));
    }
    terminate(h.pid, true);
    if wait_gone(h.pid, Duration::from_secs(5)) { Ok(()) } else { Err(format!("Couldn't stop {}", h.describe())) }
}

fn wait_gone(pid: u32, max: Duration) -> bool {
    let start = Instant::now();
    while alive(pid) {
        if start.elapsed() > max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn is_devcat(name: &str) -> bool {
    let n = name.rsplit(['/', '\\']).next().unwrap_or(name).to_lowercase();
    n == "devcat" || n == "devcat.exe"
}

// ---------------------------------------------------------------- unix

#[cfg(unix)]
fn port_pids(port: u16) -> Vec<u32> {
    let mut pids: Vec<u32> = Vec::new();
    // Phones: UDP; the browser version: TCP on the same port.
    for spec in [format!("-iUDP:{port}"), format!("-iTCP:{port}")] {
        let args = ["-nP", "-t", "-sTCP:LISTEN", spec.as_str()];
        if let Some(out) = output("lsof", &args) {
            pids.extend(out.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
    }
    if pids.is_empty() {
        // Linux without lsof.
        if let Some(out) = output("ss", &["-Hlnutp"]) {
            pids.extend(ss_pids(&out, port));
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// `ss -lnutp` lines: `udp UNCONN 0 0 0.0.0.0:47100 0.0.0.0:* users:(("devcat",pid=123,fd=9))`.
#[cfg_attr(not(unix), allow(dead_code))]
fn ss_pids(out: &str, port: u16) -> Vec<u32> {
    let suffix = format!(":{port}");
    out.lines()
        .filter(|l| l.split_whitespace().nth(4).is_some_and(|local| local.ends_with(&suffix)))
        .flat_map(|l| l.split("pid=").skip(1).filter_map(|s| s.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).collect::<Vec<u32>>())
        .collect()
}

#[cfg(unix)]
fn process(pid: u32) -> Option<Holder> {
    let p = pid.to_string();
    let name = output("ps", &["-p", &p, "-o", "comm="])?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let args = output("ps", &["-p", &p, "-o", "args="]).unwrap_or_default();
    let devcat = is_devcat(&name);
    let serve = devcat && args.split_whitespace().skip(1).any(|a| a == "serve");
    Some(Holder { pid, name: name.rsplit('/').next().unwrap_or(&name).to_string(), devcat, serve, version: None })
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// true = asked it to stop (it may take a moment), or killed it with `force`.
#[cfg(unix)]
fn terminate(pid: u32, force: bool) -> bool {
    let pid = pid as libc::pid_t;
    // SAFETY: plain signals to a process we found holding the port. SIGCONT: a `devcat serve`
    // suspended with Ctrl+Z only acts on SIGTERM once it runs again.
    unsafe {
        let ok = libc::kill(pid, if force { libc::SIGKILL } else { libc::SIGTERM }) == 0;
        libc::kill(pid, libc::SIGCONT);
        ok
    }
}

// ---------------------------------------------------------------- windows

/// `netstat -ano` lines: `  UDP    0.0.0.0:47100    *:*    1234`.
#[cfg_attr(not(windows), allow(dead_code))]
fn netstat_pids(out: &str, port: u16) -> Vec<u32> {
    let suffix = format!(":{port}");
    out.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            let listening = match w.first()?.to_uppercase().as_str() {
                "UDP" => true,
                "TCP" => w.get(3).is_some_and(|s| s.eq_ignore_ascii_case("LISTENING")),
                _ => false,
            };
            (listening && w.get(1)?.ends_with(&suffix)).then(|| w.last()?.parse().ok())?
        })
        .collect()
}

#[cfg(windows)]
fn port_pids(port: u16) -> Vec<u32> {
    let mut pids = output("netstat", &["-ano"]).map(|o| netstat_pids(&o, port)).unwrap_or_default();
    pids.sort_unstable();
    pids.dedup();
    pids.retain(|p| *p != 0);
    pids
}

#[cfg(windows)]
fn process(pid: u32) -> Option<Holder> {
    // `"devcat.exe","1234","Console","1","12,345 K"`
    let filter = format!("PID eq {pid}");
    let out = output("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"])?;
    let name = out.lines().find(|l| l.contains(&format!("\"{pid}\"")))?.split("\",\"").next()?.trim_start_matches('"').to_string();
    let devcat = is_devcat(&name);
    // tasklist doesn't show arguments; the run file says when it's `devcat serve`.
    Some(Holder { pid, name, devcat, serve: false, version: None })
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    process(pid).is_some()
}

#[cfg(windows)]
fn terminate(pid: u32, force: bool) -> bool {
    let p = pid.to_string();
    let mut args = vec!["/PID", p.as_str()];
    if force {
        args.push("/F");
    }
    output("taskkill", &args).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ss() {
        let out = "udp UNCONN 0 0 0.0.0.0:47100 0.0.0.0:* users:((\"devcat\",pid=4321,fd=9))\n\
                   tcp LISTEN 0 128 0.0.0.0:47100 0.0.0.0:* users:((\"devcat\",pid=4321,fd=10))\n\
                   tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\n\
                   udp UNCONN 0 0 10.0.0.2:5353 0.0.0.0:47100 users:((\"other\",pid=77,fd=3))\n";
        assert_eq!(ss_pids(out, 47100), vec![4321, 4321]);
        assert!(ss_pids(out, 8080).is_empty());
    }

    #[test]
    fn reads_netstat() {
        let out = "\r\nActive Connections\r\n\r\n  Proto  Local Address  Foreign Address  State  PID\r\n\
                   \x20 TCP    0.0.0.0:47100   0.0.0.0:0   LISTENING   5000\r\n\
                   \x20 TCP    10.0.0.2:52000  1.2.3.4:47100  ESTABLISHED  6000\r\n\
                   \x20 UDP    0.0.0.0:47100   *:*   5000\r\n\
                   \x20 UDP    [::]:147100   *:*   7000\r\n";
        assert_eq!(netstat_pids(out, 47100), vec![5000, 5000]);
    }

    #[test]
    fn knows_devcat() {
        assert!(is_devcat("/Applications/DevCat.app/Contents/MacOS/devcat"));
        assert!(is_devcat("DevCat.exe"));
        assert!(is_devcat("devcat"));
        assert!(!is_devcat("devcat-helper"));
        assert!(!is_devcat("nginx"));
    }

    #[test]
    fn describes() {
        let h = Holder { pid: 9, name: "devcat".into(), devcat: true, serve: true, version: Some("0.9.0".into()) };
        assert_eq!(h.describe(), "DevCat 0.9.0 (`devcat serve`, process 9)");
        let h = Holder { pid: 9, name: "nginx".into(), devcat: false, serve: false, version: None };
        assert_eq!(h.describe(), "nginx (process 9)");
    }
}
