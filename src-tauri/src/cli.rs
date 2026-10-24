//! DevCat without a window: `devcat serve` shares this computer with DevCat Mobile and the
//! browser version (pairing QR in the terminal, phones approved there), for servers, SSH
//! sessions or just leaving the app closed. Everything a phone can do works the same: the
//! commands, terminals, agents, files, databases, VPNs and clusters are the app's own (same
//! data folder, connections and paired phones); only the window is missing.
use crate::remote::server::PairRequest;
use crate::error::AppError;
use crate::remote::store::{RelayChoice, RemoteSettings, RemoteStore};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

const HELP: &str = "\
DevCat without a window: phones and browsers pair with this computer from the terminal.

Usage:
  devcat serve [options]   Share this computer with DevCat Mobile / the browser version
  devcat stop [options]    Stop a DevCat that's using the port (e.g. a `devcat serve` left
                           running in the background or from before an update)
  devcat keeper [options]  Keep rooms' changes for members who are offline (a blind keeper:
                           it stores them encrypted and can't read them). Run it on an
                           always-on machine, then add its id in the room (owner only)
  devcat mcp [options]     Your connections as an MCP server (stdio) for Claude Code, Codex,
                           Cursor or Gemini; Sync MCP in the app sets it up (`devcat mcp --help`)
  devcat help              This help
  devcat version           The version

Options for serve (saved for next time, also used by the app's remote access):
  --port <n>               Port phones connect to (UDP; browsers: TCP), default 47100
  --name <name>            Name phones show for this computer
  --relay <public|off|url> How phones on other networks get here (default public)
  --public-addr <host>     Public address phones outside this network can use
  --no-browser             Don't serve the browser version (http on the same port)
  --browser                Serve it again
Only for this run:
  --tailscale              Pair over Tailscale: the QR code opens the browser version at this
                           computer's tailnet name, which phones with Tailscale reach from any
                           network (pair once, stays paired). Needs Tailscale connected.
  --approve <ask|full|readonly>
                           Who answers pairing requests: you in this terminal (ask, the
                           default) or nobody (every phone with a valid pairing code is
                           allowed with that access). Without a terminal it must be set.
  --replace                Another DevCat is using the port: stop it without asking
  --data-dir <dir>         DevCat's data folder (default: the app's)

Options for stop: --port <n>, --data-dir <dir>, --force (kill it if it won't stop)

Options for keeper: --port <n> (UDP, default any), --relay <public|off|url> (default public),
  --owner <id> (only keep rooms of this owner; repeat for several), --data-dir <dir>

While it runs, type a command and press Enter:
  pair (or just Enter)     A new pairing QR code
  status                   Addresses, relay and connected phones
  devices                  Paired phones (numbered)
  access <n> full|readonly Change a phone's access
  revoke <n>               Unpair a phone
  log                      Recent activity from phones
  sync                     Computers this one shares its agents' memory with
  sync join <code>         Share memory with a computer: paste the code from its DevCat
                           (Memory → Other computers → Pair a computer); allow it there
  sync now                 Sync memory with them now
  sync remove <n>          Stop sharing memory with one
  sync log                 Recent memory sync activity
  quit                     Stop (Ctrl+C works too)
";

/// Runs a CLI command and returns the exit code, or None when `args` aren't one (start the app).
pub fn main(args: &[String]) -> Option<i32> {
    let code = match args.first().map(String::as_str)? {
        "serve" => {
            attach_console();
            serve_main(&args[1..])
        }
        "stop" => {
            attach_console();
            stop_main(&args[1..])
        }
        // no console attached: stdin / stdout are the agent's pipes
        "mcp" => crate::mcp_server::main(&args[1..]),
        "keeper" => {
            attach_console();
            keeper_main(&args[1..])
        }
        "help" | "--help" | "-h" => {
            attach_console();
            print!("{HELP}");
            0
        }
        "version" | "--version" | "-V" => {
            attach_console();
            println!("DevCat {}", env!("DEVCAT_VERSION"));
            0
        }
        _ => return None,
    };
    Some(code)
}

/// Windows release builds are GUI programs (no console of their own): print to the terminal
/// that started them.
fn attach_console() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        // SAFETY: plain Win32 call; ATTACH_PARENT_PROCESS = -1.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
}

#[derive(Default)]
struct Opts {
    port: Option<u16>,
    name: Option<String>,
    relay: Option<String>,
    public_addr: Option<String>,
    web: Option<bool>,
    approve: Option<String>,
    data_dir: Option<PathBuf>,
    tailscale: bool,
    replace: bool,
    force: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{flag} needs a value"));
        match flag {
            "--port" => o.port = Some(value()?.parse().map_err(|_| "--port: a number from 1024 to 65535".to_string())?),
            "--name" => o.name = Some(value()?),
            "--relay" => o.relay = Some(value()?),
            "--public-addr" => o.public_addr = Some(value()?),
            "--no-browser" => o.web = Some(false),
            "--browser" => o.web = Some(true),
            "--tailscale" => o.tailscale = true,
            "--replace" => o.replace = true,
            "--force" => o.force = true,
            "--approve" => {
                let v = value()?;
                if !["ask", "full", "readonly"].contains(&v.as_str()) {
                    return Err("--approve: ask, full or readonly".into());
                }
                o.approve = Some(v);
            }
            "--data-dir" => o.data_dir = Some(PathBuf::from(value()?)),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("Unknown option {other}")),
        }
    }
    Ok(o)
}

fn apply(o: &Opts, s: &mut RemoteSettings) {
    if let Some(p) = o.port {
        s.port = p;
    }
    if let Some(n) = &o.name {
        s.host_name = Some(n.trim().to_string()).filter(|n| !n.is_empty());
    }
    if let Some(a) = &o.public_addr {
        s.public_addr = Some(a.trim().to_string()).filter(|a| !a.is_empty());
    }
    if let Some(w) = o.web {
        s.web = w;
    }
    match o.relay.as_deref().map(str::trim) {
        None => {}
        Some("public" | "on" | "default") => s.relay = RelayChoice::Public,
        Some("off" | "none" | "no") => s.relay = RelayChoice::Off,
        Some(url) => {
            s.relay = RelayChoice::Custom;
            s.relay_url = Some(url.to_string());
        }
    }
}

/// The app's data folder (what Tauri's `app_data_dir` gives the app).
pub(crate) fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("dev.devcat.app"))
}

fn serve_main(args: &[String]) -> i32 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            print!("{HELP}");
            return 0;
        }
        Err(e) => {
            eprintln!("{e}\nSee `devcat help`.");
            return 2;
        }
    };
    let interactive = std::io::stdin().is_terminal();
    let approve = o.approve.clone().unwrap_or_else(|| "ask".into());
    if approve == "ask" && !interactive {
        eprintln!("There's no terminal to approve phones in: start with --approve full or --approve readonly\n(a phone still needs the current pairing code).");
        return 2;
    }
    let Some(dir) = o.data_dir.clone().or_else(default_data_dir) else {
        eprintln!("Can't find a data folder: pass --data-dir");
        return 1;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Can't create {}: {e}", dir.display());
        return 1;
    }
    crate::init_process();
    crate::init_backend(dir.clone());
    let code = crate::rt::block_on(serve(o, approve, interactive, dir.clone()));
    println!("Stopping…");
    crate::shutdown_backend();
    crate::instance::note_stopped(&dir);
    code
}

/// `devcat stop`: the DevCat holding the port (or noted as serving), stopped.
fn stop_main(args: &[String]) -> i32 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            print!("{HELP}");
            return 0;
        }
        Err(e) => {
            eprintln!("{e}\nSee `devcat help`.");
            return 2;
        }
    };
    let Some(dir) = o.data_dir.clone().or_else(default_data_dir) else {
        eprintln!("Can't find a data folder: pass --data-dir");
        return 1;
    };
    let port = o.port.unwrap_or_else(|| RemoteStore::new(&dir).settings().port);
    let Some(h) = crate::instance::find(&dir, port) else {
        println!("No DevCat is running here (port {port} is free).");
        return 0;
    };
    if !h.devcat {
        eprintln!("Port {port} is used by {}, not DevCat.", h.describe());
        return 1;
    }
    println!("Stopping {}…", h.describe());
    match crate::instance::stop(&dir, &h, o.force) {
        Ok(()) => {
            println!("Stopped.");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `devcat serve` couldn't listen: when another DevCat has the port, offer to stop it and take over.
async fn take_over(o: &Opts, dir: &std::path::Path, interactive: bool, err: AppError) -> Option<crate::remote::RemoteStatus> {
    let port = RemoteStore::new(dir).settings().port;
    let in_use = crate::instance::port_taken(&err.to_string());
    let holder = if in_use { crate::instance::find(dir, port) } else { None };
    eprintln!("Couldn't start: {err}");
    let Some(h) = holder else {
        if in_use {
            eprintln!("Is the DevCat app sharing this computer already? Turn off Remote access there, or use --port.");
        }
        return None;
    };
    if !h.devcat {
        eprintln!("It's used by {}. Pick another port with --port.", h.describe());
        return None;
    }
    eprintln!("{} is using it.", h.describe());
    let yes = o.replace || (interactive && {
        print!("Stop it and start this one ({})? [Y/n] ", env!("DEVCAT_VERSION"));
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).is_ok_and(|n| n > 0) && matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
    });
    if !yes {
        eprintln!("Stop it with `devcat stop`, or start with --replace.");
        return None;
    }
    println!("Stopping {}…", h.describe());
    let (d, force) = (dir.to_path_buf(), o.force);
    let stopped = tokio::task::spawn_blocking(move || crate::instance::stop(&d, &h, force)).await.unwrap_or_else(|e| Err(e.to_string()));
    if let Err(e) = stopped {
        eprintln!("{e}");
        return None;
    }
    match crate::remote::start_headless(|_| {}).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("Couldn't start: {e}");
            None
        }
    }
}

enum Input {
    Line(String),
    Closed,
}

async fn serve(o: Opts, approve: String, interactive: bool, dir: PathBuf) -> i32 {
    let st = match crate::remote::start_headless(|s| apply(&o, s)).await {
        Ok(s) => s,
        Err(e) => match take_over(&o, &dir, interactive, e).await {
            Some(s) => s,
            None => return 1,
        },
    };
    crate::instance::note_serving(&dir, st.settings.port);
    // Phones in other networks come in through the relay: wait a little for it.
    if st.settings.relay != RelayChoice::Off {
        for _ in 0..40 {
            if crate::remote::status_now().is_ok_and(|s| s.relay_connected.is_some()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    if o.tailscale {
        if !st.web_running {
            eprintln!("--tailscale pairs through the browser version, which isn't running{}.", st.web_error.as_deref().map(|e| format!(" ({e})")).unwrap_or_else(|| " (start with --browser)".into()));
            return 2;
        }
        match crate::remote::tailscale::tailnet_host().await {
            Ok(h) => crate::remote::prefer_addr(Some(format!("{h}:{}", st.settings.port))),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        }
    }
    println!("DevCat {} is sharing this computer (data: {})", env!("DEVCAT_VERSION"), dir.display());
    print_status();
    match crate::memory::sync::start_headless().await {
        Ok(0) => {}
        Ok(n) => println!("  Memory: shared with {n} other computer{}", if n == 1 { "" } else { "s" }),
        Err(e) => println!("  Memory sync didn't start: {e}"),
    }

    // Pairing requests (answered here or automatically) and changes to print.
    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<PairRequest>();
    crate::remote::set_pair_asker(move |r| {
        let _ = req_tx.send(r);
    });
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(String, Value)>();
    crate::events::add_tap(move |event, payload| {
        if event == "remote-status" || event == "remote-pty-opened" {
            let _ = ev_tx.send((event.to_string(), payload.clone()));
        }
    });
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Input>();
    if interactive {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if in_tx.send(Input::Line(l)).is_err() {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = in_tx.send(Input::Closed);
        });
    }

    show_pairing(interactive).await;
    if interactive {
        println!("Type `help` for commands.");
    }
    let mut asking: Vec<PairRequest> = Vec::new();
    let mut clients = client_names();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut code_out = false;
    let mut term = terminate_signal();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return 0,
            _ = term.recv() => return 0,
            Some(req) = req_rx.recv() => {
                if approve == "ask" {
                    let first = asking.is_empty();
                    asking.push(req);
                    if first {
                        ask(&asking[0]);
                    }
                } else {
                    println!("Pairing {} ({}) with {}", req.name, req.platform, access_word(&approve));
                    let _ = crate::remote::remote_pair_answer(req.request_id, Some(approve.clone())).await;
                }
            }
            Some((event, payload)) = ev_rx.recv() => {
                if event == "remote-pty-opened" {
                    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
                    let who = payload.get("origin").and_then(Value::as_str).unwrap_or("").trim_start_matches("phone:");
                    println!("{who} opened a terminal in {cwd}");
                    continue;
                }
                let now = client_names();
                for c in now.iter().filter(|c| !clients.contains(c)) {
                    println!("● {c} connected");
                }
                for c in clients.iter().filter(|c| !now.contains(c)) {
                    println!("○ {c} disconnected");
                }
                clients = now;
                // Requests answered elsewhere or timed out.
                let pending: Vec<String> = payload.get("pending").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.get("requestId").and_then(Value::as_str).map(String::from)).collect()).unwrap_or_default();
                if let Some(first) = asking.first().cloned() {
                    asking.retain(|r| pending.contains(&r.request_id));
                    if asking.first().map(|r| &r.request_id) != Some(&first.request_id) {
                        println!("{}'s request timed out", first.name);
                        if let Some(r) = asking.first() {
                            ask(r);
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if crate::instance::stop_requested(&dir) {
                    println!("`devcat stop` asked this one to stop.");
                    return 0;
                }
                // The code ran out (or was used): say so once; without a terminal make a new one.
                let live = crate::remote::status_now().ok().and_then(|s| s.pairing_expires).is_some();
                if !live && !code_out && asking.is_empty() {
                    code_out = true;
                    if interactive {
                        println!("The pairing code is used up or expired. Press Enter for a new one.");
                    } else {
                        show_pairing(false).await;
                        code_out = false;
                    }
                }
                if live {
                    code_out = false;
                }
            }
            Some(input) = in_rx.recv() => {
                let line = match input {
                    Input::Line(l) => l,
                    // stdin closed (e.g. `devcat serve < /dev/null &`): keep serving, Ctrl+C / kill stops.
                    Input::Closed => continue,
                };
                let line = line.trim().to_string();
                if let Some(req) = asking.first().cloned() {
                    let access = match line.to_lowercase().as_str() {
                        "f" | "full" | "y" | "yes" => Some("full"),
                        "r" | "read" | "readonly" | "read-only" => Some("readonly"),
                        "n" | "no" | "d" | "deny" => None,
                        _ => {
                            println!("Answer f (full access), r (read-only) or n (no).");
                            continue;
                        }
                    };
                    let _ = crate::remote::remote_pair_answer(req.request_id.clone(), access.map(String::from)).await;
                    match access {
                        Some(a) => println!("✓ {} paired ({})", req.name, access_word(a)),
                        None => println!("✗ {} wasn't allowed", req.name),
                    }
                    asking.remove(0);
                    if let Some(r) = asking.first() {
                        ask(r);
                    }
                    continue;
                }
                if command(&line).await {
                    return 0;
                }
            }
        }
    }
}

/// SIGTERM (systemd, `kill`) stops like Ctrl+C.
fn terminate_signal() -> mpsc::UnboundedReceiver<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(unix)]
    crate::rt::spawn(async move {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
            let _ = tx.send(());
        }
    });
    #[cfg(not(unix))]
    std::mem::forget(tx);
    rx
}

fn access_word(a: &str) -> &'static str {
    if a == "full" { "full access" } else { "read-only" }
}

fn ask(r: &PairRequest) {
    let from = r.addr.as_deref().map(|a| format!(" from {a}")).unwrap_or_default();
    println!("\n{} ({}){from} wants to pair. Allow it? [f]ull access / [r]ead-only / [n]o", r.name, r.platform);
}

fn client_names() -> Vec<String> {
    crate::remote::status_now().map(|s| s.clients.into_iter().map(|c| c.name).collect()).unwrap_or_default()
}

fn print_status() {
    let Ok(s) = crate::remote::status_now() else { return };
    println!("  Name:     {}", s.host_name);
    if let Some(id) = &s.id {
        println!("  ID:       {}…", &id[..id.len().min(16)]);
    }
    println!("  Port:     {}", s.settings.port);
    if !s.addrs.is_empty() {
        println!("  Same network: {}", s.addrs.join(", "));
    }
    match (s.settings.relay, &s.relay_connected) {
        (RelayChoice::Off, _) => println!("  Other networks: off (relay turned off)"),
        (_, Some(r)) => println!("  Other networks: through {r}"),
        (_, None) => println!("  Other networks: relay not reached yet (check the internet connection)"),
    }
    let web = if !s.settings.web {
        "off"
    } else if s.web_running {
        "on"
    } else if !s.web_available {
        "not in this build"
    } else {
        "couldn't start"
    };
    println!("  Browser version: {web}{}", s.web_error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default());
    if s.clients.is_empty() {
        println!("  Connected: nobody");
    } else {
        println!("  Connected: {}", s.clients.iter().map(|c| format!("{} ({})", c.name, access_word(&c.access))).collect::<Vec<_>>().join(", "));
    }
}

/// A new pairing code: QR + links (only the links when there's no terminal to draw in).
async fn show_pairing(qr: bool) {
    let p = match crate::remote::remote_pair_start(None).await {
        Ok(p) => p,
        Err(e) => {
            println!("Couldn't make a pairing code: {e}");
            return;
        }
    };
    let mins = ((p.expires_at - chrono::Utc::now().timestamp_millis() + 59_999) / 60_000).max(1);
    println!();
    if qr {
        match qr_text(&p.link) {
            Some(q) => print!("{q}"),
            None => println!("(The link is too long for a QR code here.)"),
        }
        println!("Scan with the phone's camera (or DevCat Mobile → Add computer). Valid for {mins} min.");
    } else {
        println!("New pairing code (valid {mins} min):");
    }
    println!("  Code:     {}", p.code);
    let tailnet = crate::remote::preferred_addr().is_some();
    if let (true, Some(w)) = (tailnet, &p.web_link) {
        println!("  Browser (Tailscale): {w}");
    }
    if let Some(h) = &p.hosted_link {
        println!("  Browser (any network): {h}");
    }
    if let (false, Some(w)) = (tailnet, &p.web_link) {
        println!("  Browser (same network): {w}");
    }
    println!("  DevCat Mobile: {}", p.app_link);
    let _ = std::io::stdout().flush();
}

/// The QR code as text: black on white whatever the terminal's colors, two rows per line.
fn qr_text(text: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::L).ok()?;
    let art = code.render::<Dense1x2>().dark_color(Dense1x2::Dark).light_color(Dense1x2::Light).quiet_zone(true).build();
    let mut out = String::new();
    for line in art.lines() {
        out.push_str("\x1b[38;5;16;48;5;231m");
        out.push_str(line);
        out.push_str("\x1b[0m\n");
    }
    Some(out)
}

/// Run a typed command; true = quit.
async fn command(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let cmd = words.next().unwrap_or("").to_lowercase();
    let rest: Vec<&str> = words.collect();
    match cmd.as_str() {
        "" | "p" | "pair" | "qr" => show_pairing(true).await,
        "s" | "status" => print_status(),
        "d" | "devices" | "phones" => list_devices().await,
        "revoke" | "unpair" => match device_at(rest.first()).await {
            Some(d) => {
                let _ = crate::remote::remote_device_revoke(d.id).await;
                println!("Unpaired {}", d.name);
            }
            None => println!("Usage: revoke <n> (the number from `devices`)"),
        },
        "access" => match (device_at(rest.first()).await, rest.get(1).copied()) {
            (Some(d), Some(a @ ("full" | "readonly"))) => {
                let _ = crate::remote::remote_device_update(d.id, Some(a.into()), None).await;
                println!("{} now has {}", d.name, access_word(a));
            }
            _ => println!("Usage: access <n> full|readonly"),
        },
        "log" | "audit" => {
            let entries = crate::remote::remote_audit(Some(20)).await.unwrap_or_default();
            if entries.is_empty() {
                println!("No activity yet");
            }
            for e in entries.iter().rev() {
                let at = chrono::DateTime::from_timestamp_millis(e.at).map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
                println!("  {at}  {}  {}{}{}", e.device_name, e.action, e.detail.as_deref().map(|d| format!("  {d}")).unwrap_or_default(), if e.ok { "" } else { "  (failed)" });
            }
        }
        "sync" => memory_sync(&rest).await,
        "h" | "help" | "?" => print!("{}", HELP.split("While it runs").nth(1).map(|h| format!("While it runs{h}")).unwrap_or_default()),
        "q" | "quit" | "exit" | "stop" => return true,
        other => println!("Unknown command {other}: type `help`"),
    }
    let _ = std::io::stdout().flush();
    false
}

/// `sync …`: memory shared with the user's other computers (memory/sync).
async fn memory_sync(rest: &[&str]) {
    use crate::memory::sync as ms;
    match rest.first().copied().unwrap_or("") {
        "" | "list" | "status" => {
            let Ok(s) = ms::memory_sync_status().await else { return };
            if s.peers.is_empty() {
                println!("Memory isn't shared with another computer yet: `sync join <code>` with the code from its DevCat (Memory → Other computers).");
            }
            for (i, p) in s.peers.iter().enumerate() {
                let when = p.peer.last_sync.and_then(chrono::DateTime::from_timestamp_millis).map(|t| format!("synced {}", t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"))).unwrap_or_else(|| "not synced yet".into());
                let err = p.peer.last_error.as_deref().map(|e| format!(" — {e}")).unwrap_or_default();
                println!("  {}. {} ({}) — {when}{err}", i + 1, p.peer.name, p.peer.os);
            }
        }
        "join" | "pair" => match rest.get(1) {
            Some(code) => {
                println!("Asking the other computer; allow it there…");
                match ms::memory_sync_join(code.to_string()).await {
                    Ok(p) => println!("✓ Memory is shared with {} now", p.name),
                    Err(e) => println!("✗ {e}"),
                }
            }
            None => println!("Usage: sync join <code> (devcat://memory-sync?…)"),
        },
        "now" => match ms::memory_sync_now(None).await {
            Ok(_) => println!("Syncing memory…"),
            Err(e) => println!("{e}"),
        },
        "remove" | "unpair" => {
            let peers = ms::memory_sync_status().await.map(|s| s.peers).unwrap_or_default();
            match rest.get(1).and_then(|n| n.parse::<usize>().ok()).and_then(|n| peers.get(n.wrapping_sub(1))) {
                Some(p) => {
                    let _ = ms::memory_sync_remove(p.peer.id.clone()).await;
                    println!("Memory isn't shared with {} any more", p.peer.name);
                }
                None => println!("Usage: sync remove <n> (the number from `sync`)"),
            }
        }
        "log" => {
            for e in ms::memory_sync_activity(Some(20)).await.unwrap_or_default().iter().rev() {
                let at = chrono::DateTime::from_timestamp_millis(e.at).map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
                println!("  {at}  {}  {}{}", e.device_name, e.action, e.detail.as_deref().map(|d| format!("  {d}")).unwrap_or_default());
            }
        }
        other => println!("Unknown: sync {other} (try `sync`, `sync join <code>`, `sync now`, `sync remove <n>`, `sync log`)"),
    }
}

async fn list_devices() {
    let devices = crate::remote::remote_devices().await.unwrap_or_default();
    if devices.is_empty() {
        println!("No paired phones yet");
    }
    let online: Vec<String> = crate::remote::status_now().map(|s| s.clients.into_iter().map(|c| c.device_id).collect()).unwrap_or_default();
    for (i, d) in devices.iter().enumerate() {
        let seen = if online.contains(&d.id) {
            "connected".to_string()
        } else {
            d.last_seen.and_then(chrono::DateTime::from_timestamp_millis).map(|t| format!("last seen {}", t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"))).unwrap_or_else(|| "never connected".into())
        };
        println!("  {}. {} ({}, {}) — {}, {seen}", i + 1, d.name, d.platform, d.kind, access_word(&d.access));
    }
}

async fn device_at(n: Option<&&str>) -> Option<crate::remote::store::Device> {
    let i: usize = n?.parse().ok()?;
    crate::remote::remote_devices().await.ok()?.into_iter().nth(i.checked_sub(1)?)
}

/// `devcat keeper`: a blind keeper for rooms (crates/rooms/src/keeper.rs).
fn keeper_main(args: &[String]) -> i32 {
    let (mut port, mut relay, mut owners, mut data_dir) = (0u16, devcat_rooms::RelayConfig::Public, Vec::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned());
        match (flag, value()) {
            ("--port", Some(v)) => match v.parse() {
                Ok(p) => port = p,
                Err(_) => {
                    eprintln!("--port: a number");
                    return 2;
                }
            },
            ("--relay", Some(v)) => {
                relay = match v.trim() {
                    "public" | "on" | "default" => devcat_rooms::RelayConfig::Public,
                    "off" | "none" | "no" => devcat_rooms::RelayConfig::Off,
                    url => devcat_rooms::RelayConfig::Custom(url.to_string()),
                }
            }
            ("--owner", Some(v)) => owners.push(v.trim().to_lowercase()),
            ("--data-dir", Some(v)) => data_dir = Some(PathBuf::from(v)),
            (other, _) => {
                eprintln!("Unknown option or missing value: {other}\nSee `devcat help`.");
                return 2;
            }
        }
    }
    let Some(dir) = data_dir.or_else(default_data_dir) else {
        eprintln!("Can't find a data folder: pass --data-dir");
        return 1;
    };
    crate::init_process();
    crate::rt::block_on(async move {
        let cfg = devcat_rooms::Config { dir: dir.join("rooms-keeper"), relay, port, keeper: true, keeper_owners: owners.clone() };
        let node = match devcat_rooms::Node::start(cfg, std::sync::Arc::new(KeeperHost)).await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        node.online(Duration::from_secs(15)).await;
        let h = node.hints();
        println!("DevCat keeper is running.\n\nKeeper id (the room's owner adds it in the room's settings):\n  {}\n", node.id());
        if let Some(r) = h.relay {
            println!("Relay: {r}");
        }
        if !h.addrs.is_empty() {
            println!("Addresses: {}", h.addrs.join(", "));
        }
        println!("{}\nCtrl+C stops it.", if owners.is_empty() { "Keeps any room that adds it.".to_string() } else { format!("Keeps rooms of: {}", owners.join(", ")) });
        let _ = tokio::signal::ctrl_c().await;
        node.shutdown().await;
        0
    })
}

/// A keeper has no rooms of its own: nobody joins, nothing to show.
struct KeeperHost;

#[async_trait::async_trait]
impl devcat_rooms::Host for KeeperHost {
    async fn approve_join(&self, _: devcat_rooms::JoinRequest) -> Option<devcat_rooms::Role> {
        None
    }
    fn changed(&self, _: &str, _: devcat_rooms::Change) {}
    fn message(&self, _: &str, _: &devcat_rooms::doc::Message) {}
}
