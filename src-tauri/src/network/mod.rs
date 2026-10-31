//! Network profiles: VPNs (OpenVPN, WireGuard) and standalone SSH tunnels.
//!
//! Profiles are stored in `<app data>/network.json`; VPN config files live in
//! `<app data>/network/<id>.ovpn|.conf` (0600); passwords go to the OS keychain.
//!
//! Live state is pushed to the frontend with two events:
//!   `net-status` → `NetStatus`             (on every state change)
//!   `net-log`    → `{ id, lines: [..] }`   (batched log lines)
mod elevate;
pub mod installers;
pub(crate) mod helper;
mod kube_forward;
mod openvpn;
mod parse;
mod registry;
mod ssh_tunnel;
pub(crate) mod store;
mod tools;
pub mod traffic;
mod wireguard;

use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, McpAccess, SshConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use store::{NetStore, check_id};

pub(crate) use crate::db::ssh::expand_home;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetKind {
    /// Standalone SSH tunnel with one or more port forwards.
    Ssh,
    Openvpn,
    Wireguard,
    /// Kubernetes port-forward(s) through one of DevCat's Kubernetes connections.
    K8s,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    /// "local" = -L bind_port:remote_host:remote_port ; "dynamic" = -D bind_port (SOCKS5)
    pub kind: String,
    /// Local port on 127.0.0.1 (0 = pick a free port).
    pub bind_port: u16,
    #[serde(default)]
    pub remote_host: String,
    #[serde(default)]
    pub remote_port: u16,
    /// Kubernetes forwards: namespace of the service / pod ("" = the connection's default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetProfile {
    pub id: String,
    pub name: String,
    pub kind: NetKind,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    /// Connect automatically when DevCat starts.
    #[serde(default)]
    pub auto_connect: bool,
    /// SSH tunnel: the SSH server (password/passphrase kept in the keychain).
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub forwards: Vec<PortForward>,
    /// VPN: username for `auth-user-pass` (OpenVPN).
    #[serde(default)]
    pub vpn_user: Option<String>,
    /// VPN password / private-key passphrase. Never written to disk (keychain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vpn_password: Option<String>,
    /// Short human description derived from the config, e.g. "udp vpn.example.com:1194".
    #[serde(default)]
    pub summary: Option<String>,
    /// Extra options (e.g. "dns": "true" for WireGuard, "ovpnArgs": extra CLI args).
    #[serde(default)]
    pub options: HashMap<String, String>,
    /// What agents may do with it (see `agent_tools/network.rs`): off hides it, read (the
    /// default) shows its status and logs and lets them connect it, full also disconnect and edit
    /// it (deleting is the DevCat Agent's, confirmed by typing the name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_access: Option<McpAccess>,
}

impl NetProfile {
    /// What agents may do with this profile.
    pub fn agent_access(&self) -> McpAccess {
        self.mcp_access.unwrap_or(McpAccess::Read)
    }
}

/// Profile plus its VPN config text (for editing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetProfileFull {
    pub profile: NetProfile,
    pub config_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetStatus {
    pub id: String,
    /// "disconnected" | "connecting" | "connected" | "disconnecting" | "error"
    pub state: String,
    /// RFC3339 time the current state was entered.
    pub since: Option<String>,
    /// e.g. "10.8.0.6 via utun5" or "127.0.0.1:5433 → db:5432"
    pub detail: Option<String>,
    pub error: Option<String>,
    /// Bound local ports for SSH tunnels (same order as `forwards`).
    #[serde(default)]
    pub local_ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetTools {
    pub openvpn: Option<String>,
    pub wireguard: Option<String>,
    /// How elevation is obtained: "osascript" (macOS) | "pkexec" (Linux) | "uac" (Windows) | "none".
    pub elevation: String,
    /// Install hints per tool for the current OS, e.g. {"openvpn": "brew install openvpn"}.
    pub install_hints: HashMap<String, String>,
    /// Tools DevCat can install itself (Windows), with the version it would install.
    #[serde(default)]
    pub installable: HashMap<String, String>,
}

/// A host from ~/.ssh/config, for "Import from SSH config".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHost {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}

static STORE: once_cell::sync::OnceCell<NetStore> = once_cell::sync::OnceCell::new();

/// How long `ensure_connected` waits (VPN auth dialogs take a while).
const ENSURE_TIMEOUT: Duration = Duration::from_secs(60);

fn store() -> AppResult<&'static NetStore> {
    STORE.get().ok_or_else(|| AppError::msg("network store not initialised"))
}

/// Connections never start a VPN / tunnel in this process (`devcat mcp`: the app owns them, and
/// one it already connected works for every process on the computer).
static PASSIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_passive() {
    PASSIVE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process leaves networks to the app (`devcat mcp`): its own status of them says
/// nothing about what's connected.
pub fn passive() -> bool {
    PASSIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Every saved profile (without secrets); none before `init`.
pub fn profiles() -> Vec<NetProfile> {
    store().and_then(|s| s.load_all()).unwrap_or_default()
}

/// One saved profile (without secrets).
pub fn profile(id: &str) -> Option<NetProfile> {
    profiles().into_iter().find(|p| p.id == id)
}

/// Tell the windows the saved profiles changed (an agent added or edited one).
pub fn profiles_changed() {
    crate::events::emit("net-profiles", serde_json::json!({}));
}

/// The live status of one profile (idle when it never connected).
pub fn status_of(id: &str) -> NetStatus {
    registry::status(id)
}

/// What in VPN config text an agent wrote would run programs, load code or write files with
/// admin rights (what the passwordless helper refuses), if anything: agents' configs are held to
/// that whichever way DevCat later connects them (helper, password prompt or UAC).
pub fn agent_config_problem(kind: NetKind, text: &str) -> Option<String> {
    helper::agent_config_problem(kind == NetKind::Openvpn, text)
}

/// Called once from `lib.rs` setup.
pub fn init(data_dir: PathBuf) {
    let _ = STORE.set(NetStore::new(data_dir, Box::new(store::Keyring)));
}

/// Called from `lib.rs` setup after `init`: enables events and connects `auto_connect` profiles.
pub fn start() {
    registry::spawn_log_flusher();
    traffic::spawn_sampler();
    crate::rt::spawn(async {
        let Ok(profiles) = store().and_then(|s| s.load_all()) else { return };
        // OpenVPN daemons a previous DevCat process left running (crash, update restart):
        // take them over so they keep working (and get "hold release" after a reconnect).
        for p in profiles.iter().filter(|p| p.kind == NetKind::Openvpn && openvpn::has_session(&p.id)) {
            if let Ok(full) = store().and_then(|s| s.get(&p.id)) {
                if openvpn::adopt(p.id.clone(), full.profile).await {
                    registry::log(&p.id, "Taken over from the previous DevCat session");
                }
            }
        }
        for p in profiles.into_iter().filter(|p| p.auto_connect) {
            if let Err(e) = start_connect(&p.id, false).await {
                registry::log(&p.id, &format!("Auto-connect failed: {e}"));
            }
        }
    });
}

/// Disconnect everything (app exit), so no openvpn / wg interface outlives DevCat.
pub fn shutdown() {
    crate::rt::block_on(shutdown_async());
}

/// `shutdown` for async callers (e.g. before an update restart): `block_on` would panic
/// inside the runtime.
pub async fn shutdown_async() {
    let all = futures::future::join_all(registry::ids().into_iter().map(|id| async move {
        let _ = disconnect(&id).await;
    }));
    let _ = tokio::time::timeout(Duration::from_secs(20), all).await;
}

/// Connect the network profile a connection depends on (`options.networkId`), if any.
pub(crate) async fn ensure_for(cfg: &ConnectionConfig) -> AppResult<()> {
    if PASSIVE.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    let Some(id) = cfg.options.get("networkId").map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    ensure(id).await
}

/// Connect a VPN / tunnel profile (if it isn't yet) and wait for it.
pub(crate) async fn ensure(id: &str) -> AppResult<()> {
    check_id(id)?;
    let profile = store()?
        .find(id)
        .map_err(|_| AppError::msg(format!("The network profile ({id}) required by this connection no longer exists")))?;
    if registry::status(id).state == registry::CONNECTED {
        return Ok(());
    }
    start_connect(id, false).await?;
    match wait_connected(id, ENSURE_TIMEOUT).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::msg(format!("Timed out waiting for network \"{}\" to connect", profile.name))),
        Err(e) => Err(AppError::msg(format!("Network \"{}\": {e}", profile.name))),
    }
}

/// Begin connecting in the background (no-op if already connecting/connected).
/// `prompt` skips the passwordless helper and asks for the admin password.
async fn start_connect(id: &str, prompt: bool) -> AppResult<()> {
    check_id(id)?;
    let full = store()?.get(id)?;
    let Some(generation) = registry::begin_connect(id) else { return Ok(()) };
    let id = id.to_string();
    tokio::spawn(async move {
        let profile = full.profile;
        let res = match store() {
            Err(e) => Err(e),
            Ok(st) => match profile.kind {
                NetKind::Ssh => ssh_tunnel::connect(id.clone(), generation, profile).await,
                NetKind::Openvpn => openvpn::connect(id.clone(), generation, profile, st, prompt).await,
                NetKind::Wireguard => wireguard::connect(id.clone(), generation, profile, st, prompt).await,
                NetKind::K8s => kube_forward::connect_boxed(id.clone(), generation, profile).await,
            },
        };
        if let Err(e) = res {
            registry::set_error(&id, generation, e.to_string());
        }
    });
    Ok(())
}

/// `Ok(true)` once connected, `Ok(false)` on timeout, `Err` if the attempt failed.
async fn wait_connected(id: &str, timeout: Duration) -> AppResult<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let st = registry::status(id);
        match st.state.as_str() {
            registry::CONNECTED => return Ok(true),
            registry::ERROR => return Err(AppError::msg(st.error.unwrap_or_else(|| "connection failed".into()))),
            registry::DISCONNECTED => return Err(AppError::msg("disconnected")),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn disconnect(id: &str) -> AppResult<()> {
    let Some((generation, run)) = registry::begin_disconnect(id) else { return Ok(()) };
    let res = match run {
        None => Ok(()),
        Some(registry::Run::Ssh(r)) => {
            registry::log(id, "Disconnecting");
            drop(r);
            Ok(())
        }
        Some(registry::Run::Openvpn(r)) => r.stop(id).await,
        Some(registry::Run::Wireguard(r)) => r.stop(id).await,
        Some(registry::Run::Kube(r)) => {
            registry::log(id, "Disconnecting");
            drop(r);
            Ok(())
        }
    };
    match &res {
        Ok(()) => {
            registry::log(id, "Disconnected");
            registry::set_disconnected(id, generation);
        }
        Err(e) => {
            registry::set_error(id, generation, e.to_string());
        }
    }
    res
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_list() -> AppResult<Vec<NetProfile>> {
    store()?.load_all()
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_get(id: String) -> AppResult<NetProfileFull> {
    store()?.get(&id)
}

/// Create (empty id) or update. `config_text` = full .ovpn / WireGuard .conf for VPN kinds.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_save(profile: NetProfile, config_text: Option<String>) -> AppResult<NetProfile> {
    store()?.save(profile, config_text)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_delete(id: String) -> AppResult<()> {
    check_id(&id)?;
    let _ = disconnect(&id).await;
    store()?.delete(&id)?;
    registry::remove(&id);
    traffic::remove(&id);
    Ok(())
}

/// Read a .ovpn / .conf file and return an unsaved draft (id "") with the config text.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_import(path: String) -> AppResult<NetProfileFull> {
    let path = expand_home(&path);
    let meta = std::fs::metadata(&path)?;
    if meta.len() > 2 * 1024 * 1024 {
        return Err(AppError::msg("File is too large for a VPN configuration"));
    }
    let text = std::fs::read_to_string(&path)?;
    let kind = parse::detect_kind(&path, &text)
        .ok_or_else(|| AppError::msg("Unrecognized file: expected an OpenVPN (.ovpn) or WireGuard (.conf) configuration"))?;
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "VPN".into());
    let mut profile = NetProfile {
        id: String::new(),
        name,
        kind,
        project_id: None,
        color: None,
        auto_connect: false,
        ssh: None,
        forwards: vec![],
        vpn_user: None,
        vpn_password: None,
        summary: None,
        options: HashMap::new(),
        mcp_access: None,
    };
    let config_text = match kind {
        NetKind::Openvpn => {
            let base = path.parent().map(PathBuf::from).unwrap_or_default();
            let (text, creds, _warnings) = parse::inline_ovpn_files(&text, &base);
            if let Some((user, pass)) = creds {
                profile.vpn_user = Some(user);
                profile.vpn_password = pass;
            }
            profile.summary = Some(parse::parse_ovpn(&text).summary);
            text
        }
        _ => {
            profile.summary = Some(parse::parse_wireguard(&text).summary);
            text
        }
    };
    Ok(NetProfileFull { profile, config_text: Some(config_text) })
}

/// Parse config text (kind "openvpn" | "wireguard") into a summary + detected fields, for live preview.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_parse(kind: NetKind, text: String) -> AppResult<serde_json::Value> {
    match kind {
        NetKind::Openvpn => Ok(serde_json::to_value(parse::parse_ovpn(&text))?),
        NetKind::Wireguard => Ok(serde_json::to_value(parse::parse_wireguard(&text))?),
        NetKind::Ssh | NetKind::K8s => Err(AppError::msg("Tunnels have no config text to parse")),
    }
}

/// `password_prompt`: connect with the admin password prompt even when the passwordless
/// helper is set up (e.g. after the helper refused the config).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_connect(id: String, password_prompt: Option<bool>) -> AppResult<()> {
    start_connect(&id, password_prompt.unwrap_or(false)).await?;
    // Returns early (Ok) if still connecting after the timeout; `net-status` follows.
    wait_connected(&id, ENSURE_TIMEOUT).await.map(|_| ())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_disconnect(id: String) -> AppResult<()> {
    check_id(&id)?;
    disconnect(&id).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_status() -> AppResult<Vec<NetStatus>> {
    let live = registry::statuses();
    Ok(store()?
        .load_all()?
        .into_iter()
        .map(|p| live.get(&p.id).cloned().unwrap_or_else(|| registry::idle(&p.id)))
        .collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_logs(id: String) -> AppResult<Vec<String>> {
    Ok(registry::logs(&id))
}

/// Windows: install OpenVPN / WireGuard with the official installer DevCat ships (one UAC
/// prompt), then report the tools again. `restartNeeded` is set when Windows asks for a reboot.
#[cfg(feature = "gui")]
#[tauri::command]
pub async fn net_install_tool(app: tauri::AppHandle, tool: String) -> AppResult<serde_json::Value> {
    if tool != "openvpn" && tool != "wireguard" {
        return Err(AppError::msg(format!("Unknown tool {tool}")));
    }
    let restart_needed = installers::install(&app, &tool).await?;
    let tools = net_tools().await?;
    Ok(serde_json::json!({ "tools": tools, "restartNeeded": restart_needed }))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_tools() -> AppResult<NetTools> {
    crate::rt::spawn_blocking(|| NetTools {
        openvpn: tools::find_openvpn().map(|p| p.to_string_lossy().to_string()),
        wireguard: tools::find_wireguard().map(|p| p.to_string_lossy().to_string()),
        elevation: elevate::mechanism().into(),
        install_hints: HashMap::from([
            ("openvpn".to_string(), tools::openvpn_hint()),
            ("wireguard".to_string(), tools::wireguard_hint()),
        ]),
        installable: installers::available(),
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_ssh_config_hosts() -> AppResult<Vec<SshConfigHost>> {
    crate::rt::spawn_blocking(parse::read_ssh_config).await.map_err(|e| AppError::msg(e.to_string()))
}

/// Recent traffic samples (1/s, up to 5 minutes) for a profile; live updates arrive as
/// `net-traffic` events with the same `TrafficSample` shape.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_traffic(id: String) -> AppResult<Vec<traffic::TrafficSample>> {
    Ok(traffic::history(&id))
}

/// Passwordless VPN helper: installed / up to date, and whether it can run the VPN tools here.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_helper_status() -> AppResult<helper::HelperStatus> {
    crate::rt::spawn_blocking(helper::status).await.map_err(|e| AppError::msg(e.to_string()))
}

/// Install or update the helper and its sudoers rule (one administrator prompt).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_helper_install() -> AppResult<helper::HelperStatus> {
    helper::install().await
}

/// Remove the helper and its sudoers rule (one administrator prompt).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn net_helper_uninstall() -> AppResult<helper::HelperStatus> {
    helper::uninstall().await
}
