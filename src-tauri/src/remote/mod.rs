//! Remote access for the DevCat mobile app. Phones connect directly to this computer (same
//! network, or a public address) and drive DevCat through an allowlist of commands. No DevCat
//! server and no account: pairing, keys and the audit log live in `<app data>/remote/`.
//!
//! Desktop events: `remote-status` (RemoteStatus), `remote-pair-request` (PairRequest),
//! `remote-pair-resolved` ({ requestId }), `remote-pty-opened` (PtyInfo, a phone started a terminal).
pub mod dispatch;
pub mod paths;
pub mod readonly;
pub mod server;
pub mod store;
pub mod tailscale;
pub mod transfer;
pub mod web;

use crate::error::{AppError, AppResult};
use devcat_remote_proto::{HostInfo, PairLink};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use server::{Backend, Client, ClientInfo, Host, PairRequest, now_ms};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use store::{AuditEntry, Device, RelayChoice, RemoteSettings, RemoteStore};
use tokio::sync::oneshot;

const APPROVE_TIMEOUT: Duration = Duration::from_secs(120);

/// App events mirrored to phones (the host filters per phone).
const FORWARDED: &[&str] = &[
    "pty-output", "pty-exit", "agent-status", "agent-gone", "agent-event", "agent-chat", "agent-chat-gone", "net-status", "net-traffic", "net-log", "keep-awake", "k8s-log",
    "k8s-log-end", "k8s-watch", "remote-pty-opened",
];

struct Remote {
    host: Arc<Host>,
    error: Mutex<Option<String>>,
    approvals: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
    pending: Mutex<Vec<PairRequest>>,
}

static REMOTE: OnceCell<Remote> = OnceCell::new();

fn remote() -> AppResult<&'static Remote> {
    REMOTE.get().ok_or_else(|| AppError::msg("Remote access isn't initialized"))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// This computer's id (public key) while remote access runs.
pub fn host_id() -> Option<String> {
    REMOTE.get().and_then(|r| r.host.endpoint_id())
}

/// The computer's name as people know it ("Arif's MacBook Pro").
fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(o) = std::process::Command::new("/usr/sbin/scutil").arg("--get").arg("ComputerName").output() {
        let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && !n.is_empty() {
            return n;
        }
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let n = String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == 0).unwrap_or(0)]).to_string();
            let n = n.trim_end_matches(".local").to_string();
            if !n.is_empty() {
                return n;
            }
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "DevCat computer".into())
}

fn host_name(s: &RemoteSettings) -> String {
    s.host_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(computer_name)
}

/// This computer's name as DevCat shows it (the name set in DevCat → Mobile, else the computer's).
pub fn this_computer() -> String {
    host_name(&settings())
}

/// Is this interface a VPN / virtual one (its address isn't reachable from the Wi-Fi)?
fn virtual_iface(name: &str) -> bool {
    ["utun", "tun", "tap", "wg", "ppp", "ipsec", "docker", "br-", "veth", "vmnet", "vboxnet", "bridge", "awdl", "llw", "anpi", "gif", "stf"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// The address the default route goes out of (no packet is sent).
fn primary_ipv4() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

/// Where phones can reach this computer: the main LAN address first, then other physical
/// interfaces, then the public address the user configured.
pub fn reachable_addrs(port: u16, public: Option<&str>) -> Vec<String> {
    let mut ips: Vec<std::net::IpAddr> = Vec::new();
    let ifaces = if_addrs::get_if_addrs().unwrap_or_default();
    let primary = primary_ipv4().filter(|ip| ifaces.iter().any(|i| i.ip() == *ip && !virtual_iface(&i.name)));
    if let Some(p) = primary {
        ips.push(p);
    }
    for i in ifaces.iter().filter(|i| !i.is_loopback() && !virtual_iface(&i.name)) {
        let ip = i.ip();
        if ip.is_ipv4() && !ips.contains(&ip) && !ip.to_string().starts_with("169.254.") {
            ips.push(ip);
        }
    }
    // Tailscale's interface is a virtual one, but phones on the same tailnet reach it anywhere.
    for i in ifaces.iter().filter(|i| virtual_iface(&i.name)) {
        if let std::net::IpAddr::V4(v4) = i.ip()
            && tailscale::is_tailnet_ip(v4)
            && !ips.contains(&i.ip())
        {
            ips.push(i.ip());
        }
    }
    let mut out: Vec<String> = ips.into_iter().map(|ip| format!("{ip}:{port}")).collect();
    if let Some(p) = public.map(str::trim).filter(|p| !p.is_empty()) {
        out.push(if p.contains(':') && !p.starts_with('[') && p.rsplit(':').next().is_some_and(|x| x.parse::<u16>().is_ok()) { p.to_string() } else { format!("{p}:{port}") });
    }
    out
}

static PREFERRED_ADDR: Mutex<Option<String>> = Mutex::new(None);

/// Make `addr` ("host:port", e.g. the tailnet name) the address phones get first, and the
/// pairing QR open the browser version there instead of the hosted page (`devcat serve --tailscale`).
pub fn prefer_addr(addr: Option<String>) {
    *PREFERRED_ADDR.lock() = addr;
}

pub fn preferred_addr() -> Option<String> {
    PREFERRED_ADDR.lock().clone()
}

/// [`reachable_addrs`] with the preferred address first.
fn phone_addrs(s: &RemoteSettings) -> Vec<String> {
    let mut addrs = reachable_addrs(s.port, s.public_addr.as_deref());
    if let Some(p) = PREFERRED_ADDR.lock().clone() {
        addrs.retain(|a| a != &p);
        addrs.insert(0, p);
    }
    addrs
}

/// What the QR code opens: the browser link at the preferred address when there is one, else
/// the hosted page (relays on), else the browser link, else the app link.
fn qr_link(preferred: bool, hosted: Option<&String>, web: Option<&String>, app: &str) -> String {
    let first = if preferred { web.or(hosted) } else { hosted.or(web) };
    first.cloned().unwrap_or_else(|| app.to_string())
}

/// The browser version on GitHub Pages: reaches the computer through its relay, from any network.
pub const HOSTED_PAGE: &str = "https://devcathq.github.io/DevCat/";

fn settings() -> RemoteSettings {
    REMOTE.get().map(|r| r.host.store.settings()).unwrap_or_default()
}

/// The relay choice (DevCat → Mobile → Reach from other networks), which rooms use too:
/// [`devcat_remote_proto::RELAY_PUBLIC`], a relay URL, or none.
pub fn relay_value() -> Option<String> {
    settings().relay_value()
}

fn host_info_now() -> HostInfo {
    let s = settings();
    HostInfo {
        id: host_id().unwrap_or_default(),
        name: host_name(&s),
        os: std::env::consts::OS.to_string(),
        app_version: env!("DEVCAT_VERSION").to_string(),
        addrs: phone_addrs(&s),
        relay: s.relay_value(),
        home: REMOTE.get().and_then(|r| r.host.home_relay()),
    }
}

/// The activity log entries of one phone (newest first).
pub fn audit_for(device_id: &str, limit: usize) -> Vec<AuditEntry> {
    REMOTE.get().map(|r| r.host.store.audit_tail(2000).into_iter().filter(|e| e.device == device_id).take(limit.min(500)).collect()).unwrap_or_default()
}

/// What the phone's "More" screen shows about this computer.
pub fn host_summary() -> Value {
    json!(host_info_now())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub settings: RemoteSettings,
    pub running: bool,
    pub error: Option<String>,
    pub id: Option<String>,
    pub host_name: String,
    pub addrs: Vec<String>,
    pub clients: Vec<ClientInfo>,
    pub pairing_expires: Option<i64>,
    pub pending: Vec<PairRequest>,
    /// Browser access is serving (TCP).
    pub web_running: bool,
    pub web_error: Option<String>,
    /// This build includes the browser version of DevCat Mobile.
    pub web_available: bool,
    /// The relay this computer is connected to (phones on other networks come in through it).
    pub relay_connected: Option<String>,
}

pub(crate) fn status_now() -> AppResult<RemoteStatus> {
    let r = remote()?;
    let s = r.host.store.settings();
    Ok(RemoteStatus {
        running: r.host.is_running(),
        error: r.error.lock().clone(),
        id: r.host.endpoint_id(),
        host_name: host_name(&s),
        addrs: phone_addrs(&s),
        clients: r.host.client_infos(),
        pairing_expires: r.host.pairing_expires(),
        pending: r.pending.lock().clone(),
        web_running: r.host.web_running(),
        web_error: r.host.web_error(),
        web_available: web::has_assets(),
        relay_connected: r.host.home_relay(),
        settings: s,
    })
}

fn push_status() {
    if let Ok(s) = status_now() {
        crate::events::emit("remote-status", s);
    }
}

/// Who answers pairing requests without a window (`devcat serve`): handed each request, it
/// calls `remote_pair_answer` (or never does: the request times out = denied).
static ASK: OnceCell<Box<dyn Fn(PairRequest) + Send + Sync>> = OnceCell::new();

pub fn set_pair_asker(f: impl Fn(PairRequest) + Send + Sync + 'static) {
    let _ = ASK.set(Box::new(f));
}

struct AppBackend;

#[async_trait::async_trait]
impl Backend for AppBackend {
    async fn call(&self, client: &Arc<Client>, cmd: &str, args: Value) -> Result<Value, Value> {
        let r = dispatch::call(client, cmd, &args).await;
        if !dispatch::NOT_AUDITED.contains(&cmd) {
            if let Some(rm) = REMOTE.get() {
                let reads = ["fs_list", "list_connections", "list_projects", "open_connection_ids", "host_info", "claude_projects", "claude_sessions", "ai_providers_list", "keep_awake_status", "net_list", "net_status", "db_children", "db_list_databases", "saved_queries_list", "history_list", "k8s_connect", "k8s_namespaces", "k8s_list", "k8s_cluster_overview", "k8s_metrics", "db_table_structure"];
                // Log changes and data reads, not the background refreshes.
                if (!reads.contains(&cmd) && !dispatch::audit_skip(cmd, &args)) || r.is_err() {
                    rm.host.store.audit(&AuditEntry {
                        at: now_ms(),
                        device: client.device.id.clone(),
                        device_name: client.device.name.clone(),
                        action: cmd.to_string(),
                        detail: match &r {
                            Err(e) => Some(format!("{} — {}", dispatch::audit_detail(cmd, &args).unwrap_or_default(), e.as_str().unwrap_or("error"))),
                            Ok(_) => dispatch::audit_detail(cmd, &args),
                        },
                        ok: r.is_ok(),
                    });
                }
            }
        }
        r
    }

    async fn approve(&self, req: PairRequest) -> Option<String> {
        if test_mode().is_some() {
            return Some("full".into());
        }
        let rm = REMOTE.get()?;
        let (tx, rx) = oneshot::channel();
        rm.approvals.lock().insert(req.request_id.clone(), tx);
        rm.pending.lock().push(req.clone());
        crate::events::emit("remote-pair-request", &req);
        push_status();
        // Bring DevCat to the front so the question is seen.
        #[cfg(feature = "gui")]
        if let Some(w) = crate::events::app().and_then(|a| tauri::Manager::webview_windows(a).into_values().next()) {
            let _ = w.request_user_attention(Some(tauri::UserAttentionType::Critical));
        }
        if let Some(ask) = ASK.get() {
            ask(req.clone());
        }
        let answer = tokio::time::timeout(APPROVE_TIMEOUT, rx).await.ok().and_then(Result::ok).flatten();
        rm.approvals.lock().remove(&req.request_id);
        rm.pending.lock().retain(|p| p.request_id != req.request_id);
        crate::events::emit("remote-pair-resolved", json!({ "requestId": req.request_id }));
        push_status();
        answer
    }

    fn host_info(&self) -> HostInfo {
        host_info_now()
    }

    fn changed(&self) {
        push_status();
    }

    async fn disconnected(&self, client: &Arc<Client>) {
        // Log / watch streams follow the phone; terminals and agents keep running.
        for s in client.streams() {
            let _ = crate::infra::k8s::streams::k8s_stream_stop(s).await;
        }
    }
}

/// Debug builds only: `DEVCAT_REMOTE_TEST=<port>` turns remote access on at that port, approves
/// pairing by itself and writes a pairing link to `<data>/remote/test-pair-link.txt`, so the
/// real host can be tested end to end without clicking. Ignored in release builds.
fn test_mode() -> Option<u16> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("DEVCAT_REMOTE_TEST").ok().and_then(|p| p.parse().ok())
}

/// Set up remote access; starts the server if it's turned on (`devcat serve` starts it itself).
pub fn init(data_dir: std::path::PathBuf) {
    paths::init(&data_dir);
    let store = Arc::new(RemoteStore::new(&data_dir));
    let host = Host::new(store, Arc::new(AppBackend));
    let _ = REMOTE.set(Remote { host, error: Mutex::new(None), approvals: Mutex::new(HashMap::new()), pending: Mutex::new(Vec::new()) });
    crate::events::add_tap(|event, payload| {
        let Some(r) = REMOTE.get() else { return };
        if FORWARDED.contains(&event) && !r.host.clients().is_empty() {
            r.host.broadcast(event, payload);
        }
    });
    if crate::events::headless() {
        return;
    }
    if let Some(port) = test_mode() {
        let r = REMOTE.get().expect("just set");
        let _ = r.host.store.save_settings(&RemoteSettings { enabled: true, port, ..r.host.store.settings() });
        let dir = data_dir.join("remote");
        crate::rt::spawn(async move {
            apply().await;
            loop {
                if let Ok(p) = remote_pair_start(None).await {
                    let _ = std::fs::write(dir.join("test-pair-link.txt"), &p.link);
                }
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
        });
        return;
    }
    if settings().enabled {
        crate::rt::spawn(async {
            apply().await;
        });
    }
}

/// Start / stop the server to match the settings.
async fn apply() {
    let Some(r) = REMOTE.get() else { return };
    let s = r.host.store.settings();
    if s.enabled {
        let res = r.host.start(s.port, s.web, s.relay_value().as_deref()).await;
        *r.error.lock() = res.err();
    } else {
        r.host.stop().await;
        r.host.pair_cancel();
        *r.error.lock() = None;
    }
    push_status();
}

/// `devcat serve`: save the changed settings and start serving (whether or not remote access
/// is turned on for the app).
pub async fn start_headless(change: impl FnOnce(&mut RemoteSettings)) -> AppResult<RemoteStatus> {
    let r = remote()?;
    let old = r.host.store.settings();
    let mut s = old.clone();
    change(&mut s);
    if s.port < 1024 {
        return Err(AppError::msg("Use a port from 1024 to 65535"));
    }
    if s.relay == RelayChoice::Custom {
        let url = s.relay_url.as_deref().unwrap_or_default();
        url.parse::<iroh::RelayUrl>().map_err(|_| AppError::msg(format!("Not a relay address: {url} (it looks like https://relay.example.com)")))?;
    }
    if serde_json::to_value(&old)? != serde_json::to_value(&s)? {
        r.host.store.save_settings(&s)?;
    }
    r.host.start(s.port, s.web, s.relay_value().as_deref()).await.map_err(AppError::msg)?;
    status_now()
}

pub async fn shutdown() {
    if let Some(r) = REMOTE.get() {
        r.host.stop().await;
    }
}

// ---------------------------------------------------------------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_status() -> AppResult<RemoteStatus> {
    status_now()
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_save(settings: RemoteSettings) -> AppResult<RemoteStatus> {
    let r = remote()?;
    let old = r.host.store.settings();
    let mut s = settings;
    if s.port < 1024 {
        return Err(AppError::msg("Use a port from 1024 to 65535"));
    }
    s.host_name = s.host_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    s.public_addr = s.public_addr.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    s.relay_url = s.relay_url.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if s.relay == RelayChoice::Custom {
        let url = s.relay_url.as_deref().ok_or_else(|| AppError::msg("Enter your relay's address (https://…)"))?;
        url.parse::<iroh::RelayUrl>().map_err(|_| AppError::msg(format!("Not a relay address: {url} (it looks like https://relay.example.com)")))?;
    }
    r.host.store.save_settings(&s)?;
    let relay_changed = old.relay_value() != s.relay_value();
    if old.enabled != s.enabled || old.port != s.port || old.web != s.web || relay_changed || (s.enabled && !r.host.is_running()) {
        apply().await;
    } else {
        push_status();
    }
    status_now()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInfo {
    /// What the QR code holds: see [`qr_link`].
    pub link: String,
    /// `devcat://pair?…` for the app (paste).
    pub app_link: String,
    /// The browser link (`http://…/#…`), when browser access is on.
    pub web_link: Option<String>,
    /// The hosted browser link (GitHub Pages, through the relay), when relays and browsers are on.
    pub hosted_link: Option<String>,
    pub code: String,
    /// QR code of `link` as an SVG document.
    pub svg: String,
    pub expires_at: i64,
}

/// A new one-time pairing code and its QR (remote access must be on).
#[cfg_attr(feature = "gui", tauri::command)]
/// `addr`: the address the browser link points at (one of the status `addrs`); default the first.
pub async fn remote_pair_start(addr: Option<String>) -> AppResult<PairInfo> {
    let r = remote()?;
    let id = r.host.endpoint_id().ok_or_else(|| AppError::msg("Turn on remote access first"))?;
    let s = r.host.store.settings();
    let (code, web_secret, expires_at) = r.host.pair_start();
    let mut addrs = phone_addrs(&s);
    if let Some(a) = addr.filter(|a| addrs.contains(a)) {
        addrs.retain(|x| x != &a);
        addrs.insert(0, a);
    }
    let pl = PairLink { id, code: code.clone(), name: host_name(&s), addrs, relay: s.relay_value(), home: r.host.home_relay() };
    let app_link = pl.to_url();
    let web_link = (s.web && r.host.web_running() && web::has_assets()).then(|| pl.to_web_url(&devcat_remote_proto::web::b64url(&web_secret)));
    // With a relay the QR opens the hosted page, which works on any network (and hands over to the app).
    let hosted_link = (s.web && pl.relay.is_some()).then(|| pl.to_hosted_url(HOSTED_PAGE));
    let link = qr_link(PREFERRED_ADDR.lock().is_some(), hosted_link.as_ref(), web_link.as_ref(), &app_link);
    let svg = qr_svg(&link)?;
    push_status();
    Ok(PairInfo { link, app_link, web_link, hosted_link, code, svg, expires_at })
}

fn qr_svg(text: &str) -> AppResult<String> {
    Ok(qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|e| AppError::msg(format!("QR code: {e}")))?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build())
}

/// A QR code (SVG) for any short text, e.g. the ntfy subscribe link.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_qr(text: String) -> AppResult<String> {
    qr_svg(&text)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_pair_cancel() -> AppResult<()> {
    remote()?.host.pair_cancel();
    push_status();
    Ok(())
}

/// Answer a pairing request: `access` "full" | "readonly", or None to deny.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_pair_answer(request_id: String, access: Option<String>) -> AppResult<()> {
    let r = remote()?;
    let access = access.filter(|a| a == "full" || a == "readonly");
    if let Some(tx) = r.approvals.lock().remove(&request_id) {
        let _ = tx.send(access);
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_devices() -> AppResult<Vec<Device>> {
    Ok(remote()?.host.store.devices().into_iter().map(Device::public).collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_device_update(id: String, access: Option<String>, name: Option<String>) -> AppResult<Vec<Device>> {
    let r = remote()?;
    let changed_access = access.as_deref().is_some_and(|a| a == "full" || a == "readonly");
    r.host.store.update_device(&id, |d| {
        if let Some(a) = access.filter(|a| a == "full" || a == "readonly") {
            d.access = a;
        }
        if let Some(n) = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
            d.name = n;
        }
    })?;
    if changed_access {
        r.host.kick(&id);
    }
    push_status();
    Ok(r.host.store.devices().into_iter().map(Device::public).collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_device_revoke(id: String) -> AppResult<Vec<Device>> {
    let r = remote()?;
    if let Some(d) = r.host.store.devices().into_iter().find(|d| d.id == id) {
        r.host.store.audit(&AuditEntry { at: now_ms(), device: d.id.clone(), device_name: d.name, action: "unpaired".into(), detail: None, ok: true });
    }
    r.host.revoke(&id);
    Ok(r.host.store.devices().into_iter().map(Device::public).collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_audit(limit: Option<usize>) -> AppResult<Vec<AuditEntry>> {
    Ok(remote()?.host.store.audit_tail(limit.unwrap_or(300).min(2000)))
}

/// New identity for this computer: every phone must pair again.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_reset_identity() -> AppResult<RemoteStatus> {
    let r = remote()?;
    for d in r.host.store.devices() {
        r.host.revoke(&d.id);
    }
    r.host.stop().await;
    r.host.store.reset_identity()?;
    apply().await;
    status_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a = reachable_addrs(47100, Some("devbox.example.com"));
        assert!(a.iter().all(|x| x.ends_with(":47100")), "{a:?}");
        assert_eq!(a.last().unwrap(), "devbox.example.com:47100");
        assert_eq!(reachable_addrs(1, Some("1.2.3.4:5000")).last().unwrap(), "1.2.3.4:5000");
        assert!(virtual_iface("utun4") && virtual_iface("bridge100") && !virtual_iface("en0"));
        let (h, w) = ("https://hosted/#".to_string(), "http://mini.ts.net:47100/#".to_string());
        assert_eq!(qr_link(false, Some(&h), Some(&w), "devcat://"), h);
        assert_eq!(qr_link(true, Some(&h), Some(&w), "devcat://"), w);
        assert_eq!(qr_link(true, Some(&h), None, "devcat://"), h);
        assert_eq!(qr_link(false, None, None, "devcat://"), "devcat://");
        assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd") && !constant_time_eq(b"ab", b"abc"));
    }
}
