//! `devcat serve` in the background: a login service that restarts it if it crashes
//! (`devcat service install`), and a console to bring it back to a terminal (`devcat attach`).
//!
//! The console is a Unix socket in the data folder (`serve-console.sock`, owner-only, peer uid
//! checked): what `devcat serve` prints goes to every attached terminal too, and a line typed there
//! is a command for it, exactly as if typed into `devcat serve` itself (pair, status, answering a
//! pairing request…). Only the computer's own user reaches it, like the terminal `devcat serve`
//! runs in. A clean stop (`quit`, `devcat stop`) exits 0 and the service leaves it stopped; a crash
//! exits non-zero and launchd / systemd start it again.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Printing only to attached terminals (greeting one that just attached).
static CLIENTS_ONLY: AtomicBool = AtomicBool::new(false);

/// Run `f` with what it prints going only to attached terminals.
pub fn clients_only<T>(f: impl FnOnce() -> T) -> T {
    CLIENTS_ONLY.store(true, Ordering::Relaxed);
    let r = f();
    CLIENTS_ONLY.store(false, Ordering::Relaxed);
    r
}

/// Running as the background service: pairing codes are shown only on attached terminals,
/// never in the log file.
static BACKGROUND: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
static CLIENTS: Mutex<Vec<std::os::unix::net::UnixStream>> = Mutex::new(Vec::new());
#[cfg(not(unix))]
static CLIENTS: Mutex<Vec<std::fs::File>> = Mutex::new(Vec::new());

pub fn set_background(on: bool) {
    BACKGROUND.store(on, Ordering::Relaxed);
}

pub fn background() -> bool {
    BACKGROUND.load(Ordering::Relaxed)
}

/// Some terminal is attached (draw QR codes for it).
pub fn attached() -> bool {
    CLIENTS.lock().is_ok_and(|c| !c.is_empty())
}

/// Print to this process's stdout and to every attached terminal.
pub fn say(s: &str) {
    if !CLIENTS_ONLY.load(Ordering::Relaxed) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
    to_clients(s);
}

/// Like `say`, but kept out of the background service's log (pairing codes).
pub fn say_private(s: &str) {
    if !background() && !CLIENTS_ONLY.load(Ordering::Relaxed) {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
    to_clients(s);
}

fn to_clients(s: &str) {
    let Ok(mut clients) = CLIENTS.lock() else { return };
    // A terminal that stopped reading (write timeout) or went away is dropped.
    clients.retain_mut(|c| c.write_all(s.as_bytes()).and_then(|_| c.flush()).is_ok());
}

/// `<data>/serve-console.sock`, or, when that's longer than a Unix socket path may be (104
/// bytes on macOS), one in `/tmp/devcat-<uid>/`: a folder only this user may own and enter.
pub fn socket_path(dir: &Path) -> PathBuf {
    let p = dir.join("serve-console.sock");
    if p.as_os_str().len() < 100 {
        return p;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let uid = unsafe { libc::geteuid() };
        let short = PathBuf::from(format!("/tmp/devcat-{uid}"));
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&short);
        // Someone else's folder (made first to catch the console): don't use it.
        let ours = std::fs::symlink_metadata(&short).is_ok_and(|m| m.is_dir() && m.uid() == uid && m.permissions().mode() & 0o077 == 0);
        if ours {
            // FNV-1a of the data folder: the same in every DevCat version.
            let h = dir.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3));
            return short.join(format!("{h:016x}.sock"));
        }
    }
    p
}

/// Listen for `devcat attach`: each attached terminal's lines go to `line`, and `attached` is
/// called when one comes in (to greet it). The socket goes away with `close`.
#[cfg(unix)]
pub fn listen(dir: &Path, line: impl Fn(String) + Send + Sync + 'static, attached: impl Fn() + Send + Sync + 'static) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    let path = socket_path(dir);
    // A socket left behind by a crash: nobody answers on it.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        return Err("another DevCat console is already listening".into());
    }
    let _ = std::fs::remove_file(&path);
    // Owner-only from the start: no moment where another user could connect.
    let old = unsafe { libc::umask(0o077) };
    let bound = UnixListener::bind(&path);
    unsafe { libc::umask(old) };
    let listener = bound.map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let (line, attached) = (Arc::new(line), Arc::new(attached));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if !same_user(&stream) {
                continue;
            }
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));
            let Ok(reader) = stream.try_clone() else { continue };
            if let Ok(mut c) = CLIENTS.lock() {
                c.push(stream);
            }
            attached();
            let line = line.clone();
            std::thread::spawn(move || {
                for l in std::io::BufReader::new(reader).lines() {
                    match l {
                        Ok(l) => line(l),
                        Err(_) => break,
                    }
                }
            });
        }
    });
    Ok(())
}

#[cfg(unix)]
fn same_user(s: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid on a connected Unix socket we own, writing two plain integers.
    let ok = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } == 0;
    ok && uid == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
pub fn listen(_: &Path, _: impl Fn(String) + Send + Sync + 'static, _: impl Fn() + Send + Sync + 'static) -> Result<(), String> {
    Err("not on this system yet".into())
}

pub fn close(dir: &Path) {
    let _ = std::fs::remove_file(socket_path(dir));
}

/// `devcat attach`: this terminal becomes the running `devcat serve`'s console until `detach`,
/// Ctrl+D or Ctrl+C (which leave it running).
#[cfg(unix)]
pub fn attach_main(dir: &Path) -> i32 {
    try_attach(dir).unwrap_or_else(|| {
        eprintln!("No DevCat is running in the background here.\nStart it with `devcat service install` (starts at login, restarts if it crashes) or `devcat serve`.");
        1
    })
}

/// Attach to the DevCat already serving `dir`, if there is one (None: nobody's listening).
/// Ctrl+C here ends only this terminal's view (the default SIGINT ends this process, not that one).
#[cfg(unix)]
pub fn try_attach(dir: &Path) -> Option<i32> {
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(socket_path(dir)).ok()?;
    Some(attached_console(&mut s))
}

#[cfg(unix)]
fn attached_console(s: &mut std::os::unix::net::UnixStream) -> i32 {
    use std::io::BufRead;
    println!("Connected to the DevCat already running here. Type `help` for commands; `detach`, Ctrl+D or Ctrl+C leave it running.");
    let Ok(mut from) = s.try_clone() else { return 1 };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut from, &mut std::io::stdout());
        println!("\nDevCat stopped.");
        std::process::exit(0);
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if matches!(line.trim(), "detach" | "exit") {
            break;
        }
        if matches!(line.trim(), "q" | "quit" | "stop") {
            println!("(This stops DevCat; the service won't start it again until the next login or `devcat service start`.)");
        }
        if s.write_all(format!("{line}\n").as_bytes()).is_err() {
            break;
        }
    }
    println!("Detached: DevCat keeps running in the background (`devcat serve` to come back).");
    0
}

/// `try_attach` for `devcat serve`: options given there don't change the running one, so say so.
pub fn try_attach_noting(dir: &Path, options_given: bool) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let mut s = UnixStream::connect(socket_path(dir)).ok()?;
        if options_given {
            println!("DevCat is already running here with its own settings; the options you gave aren't used.\nTo change them: `devcat service install <options>` (the background service) or `devcat serve --replace <options>`.");
        }
        Some(attached_console(&mut s))
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, options_given);
        None
    }
}

#[cfg(not(unix))]
pub fn try_attach(_: &Path) -> Option<i32> {
    None
}

#[cfg(not(unix))]
pub fn attach_main(_: &Path) -> i32 {
    eprintln!("`devcat attach` isn't available on this system yet.");
    1
}

const SERVICE_HELP: &str = "\
Usage:
  devcat service install [serve options]
                           Run `devcat serve` in the background: it starts when you log in and
                           again if it crashes (a clean `quit` or `devcat stop` leaves it
                           stopped). Phones are approved from `devcat serve`, or pass
                           --approve full|readonly. Other options as for `devcat serve`.
  devcat service uninstall Stop it and don't start it at login any more
  devcat service start     Start it now (after `devcat stop` or `quit`)
  devcat service status    Installed? Running? Where's the log?
  devcat serve             Shows the running one in this terminal (pairing codes, approvals,
                           status, every command); `detach`, Ctrl+D or Ctrl+C leave it running.
                           `devcat attach` does the same, without ever starting one.
";

/// `devcat service …`.
pub fn service_main(args: &[String], dir: &Path) -> i32 {
    let rest = args.get(1..).unwrap_or(&[]);
    let r = match args.first().map(String::as_str) {
        Some("install") => install(rest, dir),
        Some("uninstall" | "remove") => uninstall(),
        Some("start") => start(),
        Some("status") => status(dir),
        None | Some("help" | "-h" | "--help") => {
            print!("{SERVICE_HELP}");
            return 0;
        }
        Some(other) => Err(format!("Unknown: service {other}\n\n{SERVICE_HELP}")),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn log_path(dir: &Path) -> PathBuf {
    dir.join("serve.log")
}

/// What the service runs: this program, `serve --background`, the user's options.
fn program(rest: &[String], dir: &Path) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Can't find this program: {e}"))?;
    let mut argv = vec![exe.to_string_lossy().into_owned(), "serve".into(), "--background".into()];
    if !rest.iter().any(|a| a == "--data-dir" || a.starts_with("--data-dir=")) {
        argv.push("--data-dir".into());
        argv.push(dir.to_string_lossy().into_owned());
    }
    argv.extend(rest.iter().cloned());
    Ok(argv)
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn installed_msg(dir: &Path) {
    println!("DevCat runs in the background now, starts when you log in and again if it crashes.");
    println!("  Console: devcat serve    (shows this one: pairing codes, approving phones, status)");
    println!("  Log:     {}", log_path(dir).display());
    println!("  Stop:    devcat stop     (stays stopped until the next login or `devcat service start`)");
    println!("  Remove:  devcat service uninstall");
}

#[cfg(target_os = "macos")]
mod sys {
    use super::*;

    pub(super) fn label() -> String {
        format!("{}.serve", crate::APP_ID)
    }

    fn plist_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir().ok_or("Can't find the home folder")?;
        Ok(home.join("Library/LaunchAgents").join(format!("{}.plist", label())))
    }

    fn domain() -> String {
        format!("gui/{}", unsafe { libc::getuid() })
    }

    fn xml(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    pub(super) fn plist(argv: &[String], log: &Path) -> String {
        let args: String = argv.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
        let log = xml(&log.to_string_lossy());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
            label = xml(&label()),
        )
    }

    pub(super) fn install(argv: &[String], log: &Path) -> Result<(), String> {
        let path = plist_path()?;
        let plist = plist(argv, log);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        // Installed before: take the old one down first (its options may differ).
        let _ = run("launchctl", &["bootout", &format!("{}/{}", domain(), label())]);
        std::fs::write(&path, plist).map_err(|e| format!("{}: {e}", path.display()))?;
        run("launchctl", &["bootstrap", &domain(), &path.to_string_lossy()]).map(|_| ())
    }

    pub(super) fn uninstall() -> Result<bool, String> {
        let path = plist_path()?;
        let _ = run("launchctl", &["bootout", &format!("{}/{}", domain(), label())]);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub(super) fn start() -> Result<(), String> {
        run("launchctl", &["kickstart", &format!("{}/{}", domain(), label())]).map(|_| ())
    }

    pub(super) fn installed() -> bool {
        plist_path().is_ok_and(|p| p.exists())
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::*;

    const UNIT: &str = "devcat-serve.service";

    fn unit_path() -> Result<PathBuf, String> {
        let cfg = dirs::config_dir().ok_or("Can't find the config folder")?;
        Ok(cfg.join("systemd/user").join(UNIT))
    }

    /// systemd's quoting: each argument in double quotes, `\`, `"`, `%` and `$` escaped.
    fn quote(a: &str) -> String {
        format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$"))
    }

    pub(super) fn install(argv: &[String], log: &Path) -> Result<(), String> {
        let path = unit_path()?;
        let exec: Vec<String> = argv.iter().map(|a| quote(a)).collect();
        let log = log.to_string_lossy().replace('%', "%%");
        let unit = format!(
            "[Unit]\nDescription=DevCat: share this computer with DevCat Mobile\nAfter=network-online.target\n\n\
             [Service]\nExecStart={}\nRestart=on-failure\nRestartSec=30\nStandardOutput=append:{log}\nStandardError=append:{log}\n\n\
             [Install]\nWantedBy=default.target\n",
            exec.join(" ")
        );
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        std::fs::write(&path, unit).map_err(|e| format!("{}: {e}", path.display()))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", UNIT])?;
        run("systemctl", &["--user", "restart", UNIT]).map(|_| ())?;
        println!("(On a server you don't stay logged in to, `loginctl enable-linger` keeps it running after you log out.)");
        Ok(())
    }

    pub(super) fn uninstall() -> Result<bool, String> {
        let path = unit_path()?;
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        let existed = match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok(existed)
    }

    pub(super) fn start() -> Result<(), String> {
        run("systemctl", &["--user", "start", UNIT]).map(|_| ())
    }

    pub(super) fn installed() -> bool {
        unit_path().is_ok_and(|p| p.exists())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use super::*;
    const NO: &str = "Running DevCat as a background service isn't available on this system yet: run `devcat serve` instead.";
    pub(super) fn install(_: &[String], _: &Path) -> Result<(), String> {
        Err(NO.into())
    }
    pub(super) fn uninstall() -> Result<bool, String> {
        Err(NO.into())
    }
    pub(super) fn start() -> Result<(), String> {
        Err(NO.into())
    }
    pub(super) fn installed() -> bool {
        false
    }
}

fn install(rest: &[String], dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let argv = program(rest, dir)?;
    // Check the options now, not in a log after the service fails to start.
    crate::cli::check_serve_args(&argv[2..])?;
    sys::install(&argv, &log_path(dir))?;
    installed_msg(dir);
    Ok(())
}

fn uninstall() -> Result<(), String> {
    if sys::uninstall()? {
        println!("Removed: DevCat no longer runs in the background or starts at login.");
    } else {
        println!("DevCat wasn't installed as a service.");
    }
    Ok(())
}

fn start() -> Result<(), String> {
    if !sys::installed() {
        return Err("DevCat isn't installed as a service: `devcat service install`.".into());
    }
    sys::start()?;
    println!("Started. `devcat serve` shows it.");
    Ok(())
}

fn status(dir: &Path) -> Result<(), String> {
    let running = {
        #[cfg(unix)]
        {
            std::os::unix::net::UnixStream::connect(socket_path(dir)).is_ok()
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    println!("Service:  {}", if sys::installed() { "installed (starts at login, restarts if it crashes)" } else { "not installed (`devcat service install`)" });
    println!("Running:  {}", if running { "yes (`devcat serve` shows it)" } else { "no" });
    println!("Log:      {}", log_path(dir).display());
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn launchd_plist_is_valid() {
        let argv = vec!["/Applications/DevCat.app/Contents/MacOS/devcat".to_string(), "serve".into(), "--background".into(), "--name".into(), "Tom & <Jerry>".into()];
        let text = sys::plist(&argv, Path::new("/Users/x/Library/Application Support/devcat/serve.log"));
        let f = std::env::temp_dir().join(format!("devcat-plist-test-{}.plist", std::process::id()));
        std::fs::write(&f, &text).unwrap();
        let out = std::process::Command::new("plutil").arg("-lint").arg(&f).output().unwrap();
        let _ = std::fs::remove_file(&f);
        assert!(out.status.success(), "{}\n{text}", String::from_utf8_lossy(&out.stdout));
        assert!(text.contains("Tom &amp; &lt;Jerry&gt;"));
        assert!(text.contains("<key>SuccessfulExit</key>\n    <false/>"));
    }

    #[test]
    fn short_socket_path() {
        let long = PathBuf::from(format!("/Users/someone/{}", "x".repeat(120)));
        assert!(socket_path(&long).as_os_str().len() < 104);
        let short = PathBuf::from("/Users/a/devcat");
        assert_eq!(socket_path(&short), short.join("serve-console.sock"));
    }
}
