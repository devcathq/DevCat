//! Network tools: the user's VPNs (OpenVPN, WireGuard), SSH tunnels and Kubernetes port-forwards,
//! as DevCat runs them. Each profile has an AI access like a connection
//! (`NetProfile::mcp_access`): off hides it from agents (a connection through it then works only
//! once the user connected it), read (the default) shows its status and logs and lets agents
//! connect it (what DevCat does by itself when a connection needs it), and full also lets them
//! disconnect, edit and delete it. New profiles an agent adds start at read; agents never
//! change a profile's access.
//!
//! Networks live in the open DevCat (the app or `devcat serve`), so over MCP these tools run
//! there (`agent_bridge.rs`); one DevCat connected works for every program on the computer. VPN
//! configs an agent writes may not use directives that run programs or write files with admin
//! rights (`network::agent_config_problem`), whichever way DevCat connects them later.
use super::*;
use crate::network::{self as net, NetKind, NetProfile, NetStatus};

/// Log lines `net_logs` returns unless asked for more; and the most.
const DEFAULT_LOG_LINES: usize = 100;
const MAX_LOG_LINES: usize = 2000;
/// How long `net_connect` may run (VPN auth dialogs and UAC prompts take a while).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(100);

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>) {
    out.push(Arc::new(NetStatusTool(s.clone())));
    let any = !profiles(s).is_empty();
    if any {
        out.push(Arc::new(NetLogs(s.clone())));
        out.push(Arc::new(NetConnect(s.clone())));
    }
    if s.unattended {
        return;
    }
    out.push(Arc::new(NetSave(s.clone())));
    if any {
        out.push(Arc::new(NetDisconnect(s.clone())));
        out.push(Arc::new(NetDelete(s.clone())));
    }
}

/// The profiles an agent working in `scope` sees.
fn profiles(scope: &Scope) -> Vec<NetProfile> {
    let v = view(scope);
    net::profiles().into_iter().filter(|p| p.agent_access() != McpAccess::Off && in_reach(p.project_id.as_deref(), &v.projects, v.project.as_ref())).collect()
}

fn network_id(c: &ConnectionConfig) -> Option<&str> {
    c.options.get("networkId").map(|s| s.trim()).filter(|s| !s.is_empty())
}

/// The name of the network profile `c` goes through, if any.
pub(super) fn via(c: &ConnectionConfig) -> Option<String> {
    let id = network_id(c)?;
    Some(net::profile(id).map(|p| p.name).unwrap_or_else(|| format!("{id} (missing)")))
}

/// Whether an agent may use `c` given the network it goes through: DevCat connects that network
/// when a connection needs it, unless the profile's AI access is off (then it's the user's to
/// connect).
pub(super) fn usable(_scope: &Scope, c: &ConnectionConfig) -> Result<(), ToolOutput> {
    let Some(id) = network_id(c) else { return Ok(()) };
    let Some(p) = net::profile(id) else { return Ok(()) };
    // `devcat mcp` never starts networks: the connection works only if it's connected anyway
    if p.agent_access() != McpAccess::Off || net::passive() || net::status_of(id).state == "connected" {
        return Ok(());
    }
    Err(ToolOutput::err(format!(
        "{} goes through the network profile {}, which isn't connected, and the user keeps agents from connecting it (its AI access is off). The user connects it in DevCat's Network panel.",
        c.name, p.name
    )))
}

/// The profile the model means: by id, else by name (any case).
fn find_profile(scope: &Scope, input: &Value) -> Result<NetProfile, ToolOutput> {
    let Some(want) = arg(input, "profile") else {
        return Err(ToolOutput::err("Missing `profile`: the name or id of a DevCat network profile (see `net_status`)."));
    };
    let all = profiles(scope);
    if let Some(p) = all.iter().find(|p| p.id == want) {
        return Ok(p.clone());
    }
    let named: Vec<&NetProfile> = all.iter().filter(|p| p.name.eq_ignore_ascii_case(&want)).collect();
    match named.as_slice() {
        [p] => Ok((*p).clone()),
        [] => Err(ToolOutput::err(format!(
            "No DevCat network profile is called \"{want}\". Profiles: {}.",
            if all.is_empty() { "none".to_string() } else { all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ") }
        ))),
        _ => Err(ToolOutput::err(format!("Several network profiles are called \"{want}\"; use the id: {}.", named.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", ")))),
    }
}

/// The id of the profile `want` (name or id) a new connection may go through.
pub(super) fn find_for_connection(scope: &Scope, want: &str) -> Result<String, ToolOutput> {
    find_profile(scope, &json!({ "profile": want })).map(|p| p.id)
}

/// VPNs and tunnels run in the open DevCat; `devcat mcp` with DevCat closed can't drive them.
fn needs_app() -> Result<(), ToolOutput> {
    if net::passive() {
        return Err(ToolOutput::err("DevCat isn't open, and VPNs and tunnels run in DevCat: the user opens DevCat (or runs `devcat serve`) and asks again."));
    }
    Ok(())
}

/// Over MCP, disconnecting, editing and deleting need the profile's AI access at Full.
fn may_manage(scope: &Scope, p: &NetProfile) -> Result<(), ToolOutput> {
    if scope.mcp && p.agent_access() != McpAccess::Full {
        return Err(ToolOutput::err(format!(
            "Agents over MCP may see and connect {} but not disconnect, edit or delete it. To allow that, the user sets the profile's AI / MCP access to \"Full\" in DevCat's Network panel (edit the profile).",
            p.name
        )));
    }
    Ok(())
}

fn kind_label(k: NetKind) -> &'static str {
    match k {
        NetKind::Openvpn => "OpenVPN",
        NetKind::Wireguard => "WireGuard",
        NetKind::Ssh => "SSH tunnel",
        NetKind::K8s => "Kubernetes port-forward",
    }
}

fn access_label(a: McpAccess) -> &'static str {
    match a {
        McpAccess::Full => "agents may connect, disconnect and edit it",
        _ => "agents may connect it",
    }
}

/// "connected since 10:02 · 10.8.0.6 via utun5 · local ports 5433" style status.
fn status_line(st: &NetStatus) -> String {
    let mut parts = vec![st.state.clone()];
    if let Some(since) = st.since.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        parts[0] = format!("{} since {}", st.state, since.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"));
    }
    if let Some(d) = st.detail.as_deref().filter(|d| !d.is_empty()) {
        parts.push(d.to_string());
    }
    if !st.local_ports.is_empty() {
        parts.push(format!("local ports {}", st.local_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(e) = st.error.as_deref().filter(|e| !e.is_empty()) {
        parts.push(format!("error: {e}"));
    }
    parts.join(" · ")
}

fn describe_profile(scope: &Scope, p: &NetProfile) -> String {
    let mut l = format!("- {} ({}", p.name, kind_label(p.kind));
    if let Some(s) = p.summary.as_deref().filter(|s| !s.is_empty()) {
        l.push_str(&format!(", {s}"));
    }
    if p.auto_connect {
        l.push_str(", connects when DevCat starts");
    }
    l.push_str(&format!(") id {}", p.id));
    l.push_str(&format!("\n  {}", if net::passive() { "state unknown: DevCat isn't open".to_string() } else { status_line(&net::status_of(&p.id)) }));
    let users: Vec<String> = view(scope).connections.into_iter().filter(|c| network_id(c) == Some(p.id.as_str())).map(|c| c.name).collect();
    if !users.is_empty() {
        l.push_str(&format!("\n  connections through it: {}", users.join(", ")));
    }
    l.push_str(&format!("\n  {}", access_label(p.agent_access())));
    l
}

// ---------- net_status

struct NetStatusTool(Arc<Scope>);

#[async_trait]
impl Tool for NetStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_status".into(),
            description: "List the user's DevCat network profiles (OpenVPN and WireGuard VPNs, SSH tunnels, Kubernetes port-forwards) with their live state: connected or not since when, the VPN address or local ports, the last error, which DevCat connections go through each, and what agents may do with it. `profile` shows one. Use it before `net_connect`, or when a connection fails to reach its server.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "Profile name or id (all when left out)" } }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        arg(input, "profile").unwrap_or_default()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        if arg(&input, "profile").is_some() {
            return match find_profile(&self.0, &input) {
                Ok(p) => ToolOutput::ok(describe_profile(&self.0, &p)),
                Err(e) => e,
            };
        }
        let all = profiles(&self.0);
        if all.is_empty() {
            return ToolOutput::ok("The user has no DevCat network profiles for this folder. `net_profile_save` adds one (from a .ovpn / WireGuard .conf file, or an SSH tunnel / Kubernetes port-forward).");
        }
        ToolOutput::ok(all.iter().map(|p| describe_profile(&self.0, p)).collect::<Vec<_>>().join("\n"))
    }
}

// ---------- net_logs

struct NetLogs(Arc<Scope>);

#[async_trait]
impl Tool for NetLogs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_logs".into(),
            description: "Read the recent log of a DevCat network profile (what OpenVPN, WireGuard, ssh or the port-forward printed while connecting and since): the place to look when `net_connect` fails. The last `lines` lines (default 100).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile name or id" },
                    "lines": { "type": "integer" }
                },
                "required": ["profile"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        arg(input, "profile").unwrap_or_default()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let p = match find_profile(&self.0, &input).and_then(|p| needs_app().map(|_| p)) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let n = input.get("lines").and_then(Value::as_u64).map(|n| (n as usize).clamp(1, MAX_LOG_LINES)).unwrap_or(DEFAULT_LOG_LINES);
        match net::net_logs(p.id.clone()).await {
            Ok(lines) if lines.is_empty() => ToolOutput::ok(format!("{} has no log yet (it hasn't connected since DevCat started).", p.name)),
            Ok(lines) => {
                let from = lines.len().saturating_sub(n);
                ToolOutput::ok(lines[from..].join("\n"))
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

// ---------- net_connect

struct NetConnect(Arc<Scope>);

#[async_trait]
impl Tool for NetConnect {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_connect".into(),
            description: "Connect one of the user's DevCat network profiles (VPN, SSH tunnel, Kubernetes port-forward) and wait until it's up (up to a minute; the user may have to approve an admin prompt). Connections through it then reach their servers, and so does everything else on the computer. You don't need it before using a DevCat connection: DevCat connects the profile a connection needs by itself. `password_prompt: true` asks for the admin password even when DevCat's passwordless helper is set up (after the helper refused a config).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile name or id" },
                    "password_prompt": { "type": "boolean" }
                },
                "required": ["profile"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("connect {}", arg(input, "profile").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        CONNECT_TIMEOUT
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let p = match find_profile(&self.0, &input).and_then(|p| needs_app().map(|_| p)) {
            Ok(p) => p,
            Err(e) => return e,
        };
        connect(&p, arg_bool(&input, "password_prompt")).await
    }
}

async fn connect(p: &NetProfile, password_prompt: Option<bool>) -> ToolOutput {
    let res = net::net_connect(p.id.clone(), password_prompt).await;
    let st = net::status_of(&p.id);
    match res {
        Ok(()) if st.state == "connected" => ToolOutput::ok(format!("{} is connected: {}", p.name, status_line(&st))),
        Ok(()) => ToolOutput::ok(format!("{} is still connecting ({}); check again with `net_status`, and `net_logs` if it doesn't come up.", p.name, status_line(&st))),
        Err(e) => ToolOutput::err(format!("Couldn't connect {}: {e}. `net_logs` shows what happened.", p.name)),
    }
}

// ---------- net_disconnect

struct NetDisconnect(Arc<Scope>);

#[async_trait]
impl Tool for NetDisconnect {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_disconnect".into(),
            description: "Disconnect one of the user's DevCat network profiles. Everything going through it stops reaching its servers, also the user's own tabs: disconnect only what you connected, or when the user asks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "Profile name or id" } },
                "required": ["profile"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("disconnect {}", arg(input, "profile").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let p = match find_profile(&self.0, &input).and_then(|p| may_manage(&self.0, &p).map(|_| p)).and_then(|p| needs_app().map(|_| p)) {
            Ok(p) => p,
            Err(e) => return e,
        };
        match net::net_disconnect(p.id.clone()).await {
            Ok(()) => ToolOutput::ok(format!("{} is disconnected.", p.name)),
            Err(e) => ToolOutput::err(format!("Couldn't disconnect {}: {e}", p.name)),
        }
    }
}

// ---------- net_profile_save

struct NetSave(Arc<Scope>);

#[async_trait]
impl Tool for NetSave {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_profile_save".into(),
            description: "Add a network profile to DevCat, or change one (`profile`), and optionally connect it. A VPN comes from a config file (`config_file`: a .ovpn or WireGuard .conf, its referenced cert / key files inlined) or its text (`config_text`), with `username` / `password` for OpenVPN logins; an SSH tunnel needs `ssh` and `forwards` (kind local: bind_port → remote_host:remote_port, or dynamic: a SOCKS proxy); a Kubernetes port-forward needs `kubernetes_connection` and `forwards` (kind service or pod, remote_host = its name, remote_port, namespace). bind_port 0 picks a free port. Configs that run scripts or programs (OpenVPN up / down / plugin / script-security, WireGuard PostUp…) are refused. Point a database connection at it with `devcat_connection_save` (`network`).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "An existing profile to change (name or id); leave out to add one" },
                    "name": { "type": "string" },
                    "kind": { "type": "string", "enum": ["openvpn", "wireguard", "ssh", "k8s"], "description": "Not needed with config_file" },
                    "config_file": { "type": "string", "description": "Path to a .ovpn / .conf file" },
                    "config_text": { "type": "string", "description": "The OpenVPN / WireGuard config itself" },
                    "username": { "type": "string", "description": "OpenVPN auth-user-pass user" },
                    "password": { "type": "string", "description": "OpenVPN password or private key passphrase" },
                    "ssh": {
                        "type": "object",
                        "description": "SSH tunnel: the server",
                        "properties": {
                            "host": { "type": "string" },
                            "port": { "type": "integer" },
                            "user": { "type": "string" },
                            "key_path": { "type": "string", "description": "Private key file (else password)" },
                            "password": { "type": "string" }
                        },
                        "required": ["host"]
                    },
                    "forwards": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "enum": ["local", "dynamic", "service", "pod"] },
                                "bind_port": { "type": "integer" },
                                "remote_host": { "type": "string", "description": "Host (SSH), or the service / pod name (Kubernetes)" },
                                "remote_port": { "type": "integer" },
                                "namespace": { "type": "string", "description": "Kubernetes namespace" }
                            },
                            "required": ["kind"]
                        }
                    },
                    "kubernetes_connection": { "type": "string", "description": "Port-forward: the DevCat Kubernetes connection (name or id)" },
                    "project": { "type": "string", "description": "DevCat project (default: this folder's)" },
                    "auto_connect": { "type": "boolean", "description": "Connect when DevCat starts" },
                    "connect": { "type": "boolean", "description": "Connect it after saving" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        match (arg(input, "profile"), arg(input, "name").or_else(|| arg(input, "config_file"))) {
            (Some(p), _) => format!("change {p}"),
            (None, Some(n)) => format!("add {n}"),
            (None, None) => "add a profile".into(),
        }
    }
    fn timeout(&self, input: &Value) -> Duration {
        if arg_bool(input, "connect") == Some(true) { CONNECT_TIMEOUT + Duration::from_secs(20) } else { Duration::from_secs(30) }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        match save(&self.0, &input, ctx).await {
            Ok(out) => out,
            Err(e) => e,
        }
    }
}

async fn save(scope: &Scope, input: &Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolOutput> {
    let v = view(scope);
    let fail = |m: String| ToolOutput::err(m);
    // the profile to start from, and the config text to write (None: keep the saved one)
    let (mut p, mut config_text, existing) = match arg(input, "profile") {
        Some(_) => {
            let old = find_profile(scope, input)?;
            may_manage(scope, &old)?;
            let full = net::net_get(old.id.clone()).await.map_err(|e| fail(e.to_string()))?;
            (full.profile, None, true)
        }
        None => match arg(input, "config_file") {
            Some(path) => {
                let draft = net::net_import(local_path(ctx, &path)).await.map_err(|e| fail(format!("Couldn't read {path}: {e}")))?;
                (draft.profile, draft.config_text, false)
            }
            None => {
                let Some(kind) = arg(input, "kind") else {
                    return Err(fail("Say what to add: `config_file` (a .ovpn / .conf), or `kind` (openvpn / wireguard with `config_text`, ssh, k8s).".into()));
                };
                let kind: NetKind = serde_json::from_value(json!(kind)).map_err(|_| fail(format!("Unknown kind \"{kind}\": openvpn, wireguard, ssh or k8s.")))?;
                let blank = json!({ "id": "", "name": "", "kind": kind });
                (serde_json::from_value::<NetProfile>(blank).map_err(|e| fail(e.to_string()))?, None, false)
            }
        },
    };
    if existing && let Some(path) = arg(input, "config_file") {
        let draft = net::net_import(local_path(ctx, &path)).await.map_err(|e| fail(format!("Couldn't read {path}: {e}")))?;
        if draft.profile.kind != p.kind {
            return Err(fail(format!("{path} is a {} config, and {} is {}.", kind_label(draft.profile.kind), p.name, kind_label(p.kind))));
        }
        config_text = draft.config_text;
        p.summary = draft.profile.summary;
    }
    if let Some(t) = input.get("config_text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        config_text = Some(t.to_string());
    }
    if matches!(p.kind, NetKind::Openvpn | NetKind::Wireguard) {
        match &config_text {
            Some(t) => {
                if let Some(problem) = net::agent_config_problem(p.kind, t) {
                    return Err(fail(problem));
                }
            }
            None if !existing => return Err(fail(format!("A {} profile needs its config: `config_file` or `config_text`.", kind_label(p.kind)))),
            None => {}
        }
    } else if config_text.is_some() {
        return Err(fail(format!("{} profiles have no config text: use `ssh` / `forwards`.", kind_label(p.kind))));
    }
    if let Some(n) = arg(input, "name") {
        p.name = n;
    }
    if p.name.trim().is_empty() {
        return Err(fail("Give the profile a `name`.".into()));
    }
    if let Some(u) = arg(input, "username") {
        p.vpn_user = Some(u);
    }
    if let Some(pw) = input.get("password").and_then(Value::as_str) {
        p.vpn_password = Some(pw.to_string());
    }
    if let Some(a) = arg_bool(input, "auto_connect") {
        p.auto_connect = a;
    }
    if let Some(ssh) = input.get("ssh").filter(|s| s.is_object()) {
        let mut cfg = p.ssh.clone().unwrap_or_default();
        cfg.enabled = true;
        if let Some(h) = arg(ssh, "host") {
            cfg.host = h;
        }
        cfg.port = ssh.get("port").and_then(Value::as_u64).map(|n| n as u16).unwrap_or(if cfg.port == 0 { 22 } else { cfg.port });
        if let Some(u) = arg(ssh, "user") {
            cfg.user = u;
        }
        if let Some(k) = arg(ssh, "key_path") {
            cfg.key_path = Some(k);
            cfg.auth = "key".into();
        }
        if let Some(pw) = ssh.get("password").and_then(Value::as_str) {
            cfg.password = Some(pw.to_string());
            if cfg.key_path.is_none() {
                cfg.auth = "password".into();
            }
        }
        if cfg.auth.is_empty() {
            cfg.auth = if cfg.key_path.is_some() { "key" } else { "password" }.into();
        }
        p.ssh = Some(cfg);
    }
    if let Some(list) = input.get("forwards").and_then(Value::as_array) {
        p.forwards = list
            .iter()
            .map(|f| net::PortForward {
                kind: arg(f, "kind").unwrap_or_else(|| "local".into()),
                bind_port: f.get("bind_port").and_then(Value::as_u64).unwrap_or(0) as u16,
                remote_host: arg(f, "remote_host").unwrap_or_default(),
                remote_port: f.get("remote_port").and_then(Value::as_u64).unwrap_or(0) as u16,
                namespace: arg(f, "namespace").unwrap_or_default(),
            })
            .collect();
    }
    if let Some(k) = arg(input, "kubernetes_connection") {
        let c = conn_of(scope, &json!({ "connection": k }), ConnectionKind::Kubernetes)?;
        p.options.insert("k8sConnId".into(), c.id);
    }
    if let Some(name) = arg(input, "project") {
        p.project_id = project_named(&v, &name)?;
    } else if !existing {
        p.project_id = v.project.as_ref().map(|p| p.id.clone());
    }
    let saved = net::net_save(p, config_text).await.map_err(|e| fail(format!("Couldn't save the profile: {e}")))?;
    net::profiles_changed();
    let mut text = format!("{} the network profile {} ({}) id {}.", if existing { "Changed" } else { "Added" }, saved.name, kind_label(saved.kind), saved.id);
    if arg_bool(input, "connect") == Some(true) {
        needs_app()?;
        let out = connect(&saved, None).await;
        text = format!("{text}\n{}", out.content);
        return Ok(if out.is_error { ToolOutput::err(text) } else { ToolOutput::ok(text) });
    }
    Ok(ToolOutput::ok(text))
}

// ---------- net_profile_delete

struct NetDelete(Arc<Scope>);

#[async_trait]
impl Tool for NetDelete {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "net_profile_delete".into(),
            description: "Delete one of the user's DevCat network profiles (it's disconnected first). Connections that went through it then connect directly. Only when the user asks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "Profile name or id" } },
                "required": ["profile"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("delete {}", arg(input, "profile").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let p = match find_profile(&self.0, &input).and_then(|p| may_manage(&self.0, &p).map(|_| p)) {
            Ok(p) => p,
            Err(e) => return e,
        };
        match net::net_delete(p.id.clone()).await {
            Ok(()) => {
                net::profiles_changed();
                ToolOutput::ok(format!("Deleted the network profile {}.", p.name))
            }
            Err(e) => ToolOutput::err(format!("Couldn't delete {}: {e}", p.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_configs_cant_run_programs() {
        let ok = "client\ndev tun\nremote vpn.example.com 1194\n<ca>\nup this is cert data\n</ca>\nauth-user-pass\n";
        assert_eq!(net::agent_config_problem(NetKind::Openvpn, ok), None);
        for bad in ["client\nup /tmp/x.sh\n", "client\n--script-security 2\n", "setenv opt plugin /x.so\n", "<connection>\nremote a 1\nup /x\n</connection>\n", "<script>\n</script>\n", "<ca>\nno end\n"] {
            assert!(net::agent_config_problem(NetKind::Openvpn, bad).is_some(), "{bad}");
        }
        assert_eq!(net::agent_config_problem(NetKind::Wireguard, "[Interface]\nPrivateKey = a=\n[Peer]\nEndpoint = h:51820\n"), None);
        assert!(net::agent_config_problem(NetKind::Wireguard, "[Interface]\nPostUp = curl x | sh\n").is_some());
    }

    #[test]
    fn profiles_default_to_read_and_off_hides_them() {
        let p = |v: Value| -> NetProfile { serde_json::from_value(v).unwrap() };
        assert_eq!(p(json!({ "id": "a", "name": "a", "kind": "openvpn" })).agent_access(), McpAccess::Read);
        assert_eq!(p(json!({ "id": "a", "name": "a", "kind": "openvpn", "mcpAccess": "full" })).agent_access(), McpAccess::Full);
        let off = p(json!({ "id": "a", "name": "a", "kind": "ssh", "mcpAccess": "off" }));
        assert_eq!(off.agent_access(), McpAccess::Off);
        assert!(may_manage(&Scope { mcp: true, ..Scope::default() }, &p(json!({ "id": "a", "name": "VPN", "kind": "openvpn" }))).is_err(), "read: no disconnect over MCP");
        assert!(may_manage(&Scope::default(), &p(json!({ "id": "a", "name": "VPN", "kind": "openvpn" }))).is_ok(), "the DevCat Agent asks instead");
    }
}
