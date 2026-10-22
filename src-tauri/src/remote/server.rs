//! The phone-facing server: an iroh (QUIC) endpoint on a fixed UDP port, direct connections
//! only (no relay, no discovery). Each phone is identified by its iroh key; only paired keys
//! get in, and pairing needs a one-time code from the QR plus approval on the computer.
//! The app-specific parts (commands, approval dialog, audit) sit behind [`Backend`] so the
//! protocol can be tested without a running app.
use super::store::{AuditEntry, Device, RemoteStore};
use devcat_remote_proto::{ALPN, ClientMsg, DeviceInfo, HostInfo, HostMsg, read_frame, write_frame};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
const PAIR_TTL: Duration = Duration::from_secs(5 * 60);
const PAIR_MAX_ATTEMPTS: u32 = 5;
/// Terminal output queued for a slow phone before we drop it (the phone re-attaches).
const MAX_QUEUED_OUTPUT: usize = 4 * 1024 * 1024;

/// Events every phone gets.
const BROADCAST_EVENTS: &[&str] = &["pty-exit", "agent-status", "agent-gone", "agent-chat", "agent-chat-gone", "net-status", "net-traffic", "net-log", "keep-awake", "remote-pty-opened"];
/// Events only the phone that started the stream gets (matched on `streamId`).
const STREAM_EVENTS: &[&str] = &["k8s-log", "k8s-log-end", "k8s-watch"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub request_id: String,
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub addr: Option<String>,
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync + 'static {
    async fn call(&self, client: &Arc<Client>, cmd: &str, args: Value) -> Result<Value, Value>;
    /// Ask the user whether this phone may pair. `Some(access)` = allowed.
    async fn approve(&self, req: PairRequest) -> Option<String>;
    fn host_info(&self) -> HostInfo;
    /// Phones connected / disconnected / paired.
    fn changed(&self);
    /// A phone went away: stop what it started that shouldn't outlive it.
    async fn disconnected(&self, client: &Arc<Client>);
}

/// One connected phone.
pub struct Client {
    pub conn_id: u64,
    pub device: Device,
    pub since: i64,
    pub addr: Option<String>,
    tx: mpsc::UnboundedSender<HostMsg>,
    queued: Arc<AtomicUsize>,
    attached: Mutex<HashSet<String>>,
    streams: Mutex<HashSet<String>>,
    closer: Box<dyn Fn(&str) + Send + Sync>,
}

impl Client {
    pub fn readonly(&self) -> bool {
        self.device.access != "full"
    }
    pub fn attach(&self, pty: &str) {
        self.attached.lock().insert(pty.to_string());
    }
    pub fn detach(&self, pty: &str) {
        self.attached.lock().remove(pty);
    }
    pub fn own_stream(&self, id: &str) {
        self.streams.lock().insert(id.to_string());
    }
    pub fn owns_stream(&self, id: &str) -> bool {
        self.streams.lock().contains(id)
    }
    pub fn drop_stream(&self, id: &str) {
        self.streams.lock().remove(id);
    }
    pub fn streams(&self) -> Vec<String> {
        self.streams.lock().iter().cloned().collect()
    }
    pub fn close(&self, why: &str) {
        (self.closer)(why);
    }

    /// Forward an app event if this phone should see it.
    fn event(&self, event: &str, payload: &Value) {
        let field = |k: &str| payload.get(k).and_then(Value::as_str).unwrap_or("");
        let wanted = if event == "pty-output" {
            self.attached.lock().contains(field("id"))
        } else if event == "agent-event" {
            // DevCat Agent chats the phone has open (attached by session id)
            self.attached.lock().contains(field("sessionId"))
        } else if STREAM_EVENTS.contains(&event) {
            self.streams.lock().contains(field("streamId"))
        } else {
            BROADCAST_EVENTS.contains(&event)
        };
        if !wanted {
            return;
        }
        if event == "pty-output" {
            let n = field("data").len();
            if self.queued.load(Ordering::Relaxed) + n > MAX_QUEUED_OUTPUT {
                // Too slow to keep up: drop output, tell the phone to fetch the screen again.
                self.detach(field("id"));
                let _ = self.tx.send(HostMsg::Event { event: "pty-lagged".into(), payload: json!({ "id": field("id") }) });
                return;
            }
            self.queued.fetch_add(n, Ordering::Relaxed);
        }
        let _ = self.tx.send(HostMsg::Event { event: event.to_string(), payload: payload.clone() });
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub access: String,
    pub since: i64,
    pub addr: Option<String>,
}

struct Pairing {
    code: String,
    /// What the browser link carries instead of the code (never sent over the network).
    web_secret: [u8; 32],
    expires: Instant,
    attempts: u32,
}

/// One end of a phone connection, whatever the transport (QUIC for the app, WebSocket for the
/// browser): messages in…
#[async_trait::async_trait]
pub trait WireRx: Send {
    async fn recv(&mut self) -> Option<ClientMsg>;
}

/// …and messages out.
#[async_trait::async_trait]
pub trait WireTx: Send {
    async fn send(&mut self, m: &HostMsg) -> bool;
}

struct Running {
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
    /// The browser listener (TCP, same port number), if on.
    web: Option<tokio::task::JoinHandle<()>>,
}

pub struct Host {
    pub store: Arc<RemoteStore>,
    backend: Arc<dyn Backend>,
    pairing: Mutex<Option<Pairing>>,
    clients: Mutex<HashMap<u64, Arc<Client>>>,
    next_id: AtomicU64,
    running: Mutex<Option<Running>>,
    web_error: Mutex<Option<String>>,
}

/// Pairing code alphabet: no 0/O/1/I to make it easy to type.
const CODE_ALPHA: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Host {
    pub fn new(store: Arc<RemoteStore>, backend: Arc<dyn Backend>) -> Arc<Self> {
        Arc::new(Self {
            store,
            backend,
            pairing: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            running: Mutex::new(None),
            web_error: Mutex::new(None),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    pub fn endpoint_id(&self) -> Option<String> {
        self.running.lock().as_ref().map(|r| r.endpoint.id().to_string())
    }

    pub fn host_name(&self) -> String {
        self.backend.host_info().name
    }

    /// Why browser access isn't available while running (port taken), if so.
    pub fn web_error(&self) -> Option<String> {
        self.web_error.lock().clone()
    }

    pub fn web_running(&self) -> bool {
        self.running.lock().as_ref().is_some_and(|r| r.web.is_some())
    }

    /// Listen on `port`: UDP for the app (all interfaces) and, with `web`, TCP for browsers.
    /// `relay`: [`devcat_remote_proto::RELAY_PUBLIC`] (iroh's public relays, found by phones
    /// through iroh's DNS lookup), a relay URL, or `None` (direct connections only).
    pub async fn start(self: &Arc<Self>, port: u16, web: bool, relay: Option<&str>) -> Result<(), String> {
        self.stop().await;
        let key = SecretKey::from_bytes(&self.store.secret_key().map_err(|e| e.to_string())?);
        let builder = match relay {
            None => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
            Some(devcat_remote_proto::RELAY_PUBLIC) => Endpoint::builder(presets::N0),
            Some(url) => {
                let url: RelayUrl = url.parse().map_err(|_| format!("Not a relay address: {url} (it looks like https://relay.example.com)"))?;
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom([url]))
            }
        };
        let builder = builder
            .secret_key(key)
            .alpns(vec![ALPN.to_vec()])
            .bind_addr(format!("0.0.0.0:{port}").as_str())
            .map_err(|e| e.to_string())?;
        let endpoint = builder.bind().await.map_err(|e| {
            let s = e.to_string();
            if s.to_lowercase().contains("in use") { format!("Port {port} is already in use. Pick another port.") } else { format!("Couldn't listen on port {port}: {s}") }
        })?;
        if relay.is_some() {
            // Show the relay as connected once it is.
            let (ep, me) = (endpoint.clone(), self.clone());
            tokio::spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), ep.online()).await;
                me.backend.changed();
            });
        }
        let (ep, me) = (endpoint.clone(), self.clone());
        let task = tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let addr = match incoming.remote_addr() {
                    iroh::endpoint::IncomingAddr::Ip(a) => Some(a.to_string()),
                    _ => None,
                };
                let Ok(accepting) = incoming.accept() else { continue };
                let me = me.clone();
                tokio::spawn(async move {
                    if let Ok(conn) = accepting.await {
                        me.serve(conn, addr).await;
                    }
                });
            }
        });
        let web_task = if web {
            match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
                Ok(l) => {
                    *self.web_error.lock() = None;
                    Some(tokio::spawn(super::web::serve(self.clone(), l)))
                }
                Err(e) => {
                    *self.web_error.lock() = Some(format!("Browser access is off: TCP port {port} is in use ({e}). Pick another port."));
                    None
                }
            }
        } else {
            *self.web_error.lock() = None;
            None
        };
        *self.running.lock() = Some(Running { endpoint, task, web: web_task });
        self.backend.changed();
        Ok(())
    }

    /// The relay this computer is connected to right now, if any.
    pub fn home_relay(&self) -> Option<String> {
        self.running.lock().as_ref().and_then(|r| r.endpoint.addr().relay_urls().next().map(|u| u.to_string()))
    }

    pub async fn stop(&self) {
        let r = self.running.lock().take();
        if let Some(r) = r {
            for c in self.clients() {
                c.close("DevCat stopped remote access");
            }
            r.task.abort();
            if let Some(w) = r.web {
                w.abort();
            }
            r.endpoint.close().await;
            self.backend.changed();
        }
    }

    pub fn clients(&self) -> Vec<Arc<Client>> {
        self.clients.lock().values().cloned().collect()
    }

    pub fn client_infos(&self) -> Vec<ClientInfo> {
        let mut v: Vec<ClientInfo> = self
            .clients()
            .iter()
            .map(|c| ClientInfo { device_id: c.device.id.clone(), name: c.device.name.clone(), platform: c.device.platform.clone(), access: c.device.access.clone(), since: c.since, addr: c.addr.clone() })
            .collect();
        v.sort_by_key(|c| c.since);
        v
    }

    /// A new one-time pairing code and browser secret (replaces the previous ones).
    /// Returns (code, browser secret, expires at ms).
    pub fn pair_start(&self) -> (String, [u8; 32], i64) {
        let bytes: [u8; 10] = super::store::random_bytes();
        let code: String = bytes.iter().map(|b| CODE_ALPHA[(*b as usize) % CODE_ALPHA.len()] as char).collect();
        let web_secret: [u8; 32] = super::store::random_bytes();
        *self.pairing.lock() = Some(Pairing { code: code.clone(), web_secret, expires: Instant::now() + PAIR_TTL, attempts: 0 });
        (code, web_secret, now_ms() + PAIR_TTL.as_millis() as i64)
    }

    /// The browser secret of the pairing in progress, if any.
    pub(crate) fn web_secret(&self) -> Option<[u8; 32]> {
        self.pairing.lock().as_ref().filter(|p| p.expires > Instant::now()).map(|p| p.web_secret)
    }

    /// A browser proved it has the secret: the pairing is used up.
    pub(crate) fn consume_pairing(&self) {
        *self.pairing.lock() = None;
    }

    /// A browser failed to prove it: counts like a wrong code.
    pub(crate) fn pairing_attempt_failed(&self) {
        let mut p = self.pairing.lock();
        if let Some(cur) = p.as_mut() {
            cur.attempts += 1;
            if cur.attempts >= PAIR_MAX_ATTEMPTS {
                *p = None;
            }
        }
    }

    pub fn pair_cancel(&self) {
        *self.pairing.lock() = None;
    }

    pub fn pairing_expires(&self) -> Option<i64> {
        let p = self.pairing.lock();
        p.as_ref().filter(|p| p.expires > Instant::now()).map(|p| now_ms() + p.expires.saturating_duration_since(Instant::now()).as_millis() as i64)
    }

    /// Check a code from a phone; the code is used up when it matches. Too many wrong tries
    /// cancel pairing altogether.
    fn take_code(&self, code: &str) -> bool {
        let mut p = self.pairing.lock();
        let Some(cur) = p.as_mut() else { return false };
        if cur.expires <= Instant::now() {
            *p = None;
            return false;
        }
        let normalized: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
        if super::constant_time_eq(normalized.as_bytes(), cur.code.as_bytes()) {
            *p = None;
            return true;
        }
        cur.attempts += 1;
        if cur.attempts >= PAIR_MAX_ATTEMPTS {
            *p = None;
        }
        false
    }

    /// Forward an app event to the phones that want it.
    pub fn broadcast(&self, event: &str, payload: &Value) {
        for c in self.clients() {
            c.event(event, payload);
        }
    }

    /// Unpair: forget the key and drop its connections.
    pub fn revoke(&self, device_id: &str) -> bool {
        let removed = self.store.remove_device(device_id).unwrap_or(false);
        for c in self.clients().into_iter().filter(|c| c.device.id == device_id) {
            c.close("This phone was unpaired on the computer");
        }
        self.backend.changed();
        removed
    }

    /// Access changed on the computer: reconnect so the phone picks it up.
    pub fn kick(&self, device_id: &str) {
        for c in self.clients().into_iter().filter(|c| c.device.id == device_id) {
            c.close("Access changed on the computer; reconnecting");
        }
    }

    /// Ask the user on the computer, then remember the phone. `gone` resolves if the phone
    /// gives up while waiting. Returns the saved device, or the reason it wasn't.
    pub(crate) async fn approve_new(&self, mut d: Device, addr: Option<String>, gone: impl std::future::Future<Output = ()> + Send) -> Result<Device, String> {
        let req = PairRequest { request_id: uuid::Uuid::new_v4().to_string(), device_id: d.id.clone(), name: d.name.clone(), platform: d.platform.clone(), addr: addr.clone() };
        let answer = tokio::select! {
            a = self.backend.approve(req) => a,
            _ = gone => None,
        };
        let access = answer.ok_or_else(|| "Pairing was declined on the computer.".to_string())?;
        d.access = access;
        d.paired_at = now_ms();
        self.store.add_device(d.clone()).map_err(|e| format!("Couldn't save the pairing: {e}"))?;
        self.store.audit(&AuditEntry { at: now_ms(), device: d.id.clone(), device_name: d.name.clone(), action: "paired".into(), detail: addr, ok: true });
        Ok(d)
    }

    /// Clean up a name / platform a phone sent.
    pub(crate) fn tidy(name: &str, platform: &str) -> (String, String) {
        let name: String = name.chars().filter(|c| !c.is_control()).take(60).collect();
        let name = if name.trim().is_empty() { "Phone".to_string() } else { name.trim().to_string() };
        (name, platform.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect())
    }

    async fn serve(self: Arc<Self>, conn: Connection, addr: Option<String>) {
        let device_id = conn.remote_id().to_string();
        let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi()).await else {
            conn.close(1u32.into(), b"no stream");
            return;
        };
        let hello = match tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, ClientMsg>(&mut recv)).await {
            Ok(Ok(Some(ClientMsg::Hello { name, platform, app_version, pair_code }))) => (name, platform, pair_code, app_version),
            _ => {
                conn.close(1u32.into(), b"expected hello");
                return;
            }
        };
        let (name, platform) = Self::tidy(&hello.0, &hello.1);
        let pair_code = hello.2;

        let admitted: Result<Device, String> = match self.store.devices().into_iter().find(|d| d.id == device_id) {
            Some(d) => Ok(d),
            None => match pair_code {
                None => Err(format!("This phone isn't paired with {}. On the computer open DevCat → Mobile and scan the QR code.", self.backend.host_info().name)),
                Some(code) if !self.take_code(&code) => Err("The pairing code is wrong or expired. On the computer click New code and scan again.".into()),
                Some(_) => {
                    let host_name = self.backend.host_info().name;
                    if write_frame(&mut send, &HostMsg::Pairing { host_name }).await.is_err() {
                        return;
                    }
                    // The hosted browser version speaks the app protocol through the relay ("web" version).
                    let kind = if hello.3 == "web" { "browser" } else { "app" };
                    let new = Device { id: device_id.clone(), name, platform, access: String::new(), paired_at: 0, last_seen: None, kind: kind.into(), secret: None };
                    let c2 = conn.clone();
                    self.approve_new(new, addr.clone(), async move {
                        c2.closed().await;
                    })
                    .await
                }
            },
        };
        let device = match admitted {
            Ok(d) => d,
            Err(reason) => {
                let _ = write_frame(&mut send, &HostMsg::Denied { reason }).await;
                let _ = send.finish();
                conn.closed().await;
                return;
            }
        };
        let closer = {
            let conn = conn.clone();
            Box::new(move |why: &str| conn.close(0u32.into(), why.as_bytes())) as Box<dyn Fn(&str) + Send + Sync>
        };
        self.session(device, addr, Box::new(QuicRx(recv)), Box::new(QuicTx(send)), closer).await;
    }

    /// Welcome an admitted phone and serve it until it goes away (any transport).
    pub(crate) async fn session(self: Arc<Self>, device: Device, addr: Option<String>, mut rx_wire: Box<dyn WireRx>, mut tx_wire: Box<dyn WireTx>, closer: Box<dyn Fn(&str) + Send + Sync>) {
        let device = self.store.update_device(&device.id, |d| d.last_seen = Some(now_ms())).ok().flatten().unwrap_or(device);
        let (tx, mut rx) = mpsc::unbounded_channel::<HostMsg>();
        let queued = Arc::new(AtomicUsize::new(0));
        let client = Arc::new(Client {
            conn_id: self.next_id.fetch_add(1, Ordering::Relaxed),
            device: device.clone(),
            since: now_ms(),
            addr,
            tx: tx.clone(),
            queued: queued.clone(),
            attached: Mutex::new(HashSet::new()),
            streams: Mutex::new(HashSet::new()),
            closer,
        });
        let welcome = HostMsg::Welcome {
            host: self.backend.host_info(),
            device: DeviceInfo { id: device.id.clone(), name: device.name.clone(), access: device.access.clone() },
        };
        if !tx_wire.send(&welcome).await {
            return;
        }
        self.clients.lock().insert(client.conn_id, client.clone());
        self.backend.changed();

        // writer
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let output = match &msg {
                    HostMsg::Event { event, payload } if event == "pty-output" => payload.get("data").and_then(Value::as_str).map_or(0, str::len),
                    _ => 0,
                };
                let ok = tx_wire.send(&msg).await;
                if output > 0 {
                    queued.fetch_sub(output, Ordering::Relaxed);
                }
                if !ok {
                    break;
                }
            }
        });

        // reader
        while let Some(m) = rx_wire.recv().await {
            match m {
                ClientMsg::Call { id, cmd, args } => {
                    let (me, c, tx) = (self.clone(), client.clone(), tx.clone());
                    tokio::spawn(async move {
                        let msg = match me.backend.call(&c, &cmd, args).await {
                            Ok(v) => HostMsg::Result { id, ok: Some(v), err: None },
                            Err(e) => HostMsg::Result { id, ok: None, err: Some(e) },
                        };
                        let _ = tx.send(msg);
                    });
                }
                ClientMsg::Ping => {
                    let _ = tx.send(HostMsg::Pong);
                }
                ClientMsg::Hello { .. } => {}
            }
        }
        writer.abort();
        client.close("bye");
        self.clients.lock().remove(&client.conn_id);
        let _ = self.store.update_device(&device.id, |d| d.last_seen = Some(now_ms()));
        self.backend.disconnected(&client).await;
        self.backend.changed();
    }
}

struct QuicRx(iroh::endpoint::RecvStream);
struct QuicTx(iroh::endpoint::SendStream);

#[async_trait::async_trait]
impl WireRx for QuicRx {
    async fn recv(&mut self) -> Option<ClientMsg> {
        read_frame(&mut self.0).await.ok().flatten()
    }
}

#[async_trait::async_trait]
impl WireTx for QuicTx {
    async fn send(&mut self, m: &HostMsg) -> bool {
        write_frame(&mut self.0, m).await.is_ok()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use devcat_remote_proto::ClientMsg;
    use iroh::EndpointAddr;

    pub(crate) struct TestBackend {
        pub allow: bool,
        pub events: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Backend for TestBackend {
        async fn call(&self, client: &Arc<Client>, cmd: &str, args: Value) -> Result<Value, Value> {
            match cmd {
                "echo" => Ok(json!({ "args": args, "readonly": client.readonly() })),
                "attach" => {
                    client.attach(args["id"].as_str().unwrap());
                    Ok(Value::Null)
                }
                _ => Err(json!("unknown")),
            }
        }
        async fn approve(&self, req: PairRequest) -> Option<String> {
            self.events.lock().push(format!("approve {}", req.name));
            self.allow.then(|| "readonly".to_string())
        }
        fn host_info(&self) -> HostInfo {
            HostInfo { id: String::new(), name: "Test Mac".into(), os: "macos".into(), app_version: "0".into(), addrs: vec![], relay: None }
        }
        fn changed(&self) {}
        async fn disconnected(&self, _: &Arc<Client>) {
            self.events.lock().push("disconnected".into());
        }
    }

    pub(crate) fn tmp_store() -> Arc<RemoteStore> {
        let d = std::env::temp_dir().join(format!("devcat-remote-srv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        Arc::new(RemoteStore::new(&d))
    }

    /// A phone-side session over a real local QUIC connection.
    struct Phone {
        _ep: Endpoint,
        _conn: Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    }

    async fn phone(key: &SecretKey, host_id: &str, port: u16, code: Option<&str>) -> (Phone, HostMsg) {
        let ep = Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).secret_key(key.clone()).bind().await.unwrap();
        let addr = EndpointAddr::new(host_id.parse().unwrap()).with_ip_addr(format!("127.0.0.1:{port}").parse().unwrap());
        let conn = ep.connect(addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        write_frame(&mut send, &ClientMsg::Hello { name: "Test Phone".into(), platform: "ios".into(), app_version: "1".into(), pair_code: code.map(str::to_string) }).await.unwrap();
        let mut first: HostMsg = read_frame(&mut recv).await.unwrap().unwrap();
        if matches!(first, HostMsg::Pairing { .. }) {
            first = read_frame(&mut recv).await.unwrap().unwrap();
        }
        (Phone { _ep: ep, _conn: conn, send, recv }, first)
    }

    pub(crate) fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn pairing_calls_and_events() {
        let backend = Arc::new(TestBackend { allow: true, events: Mutex::new(vec![]) });
        let host = Host::new(tmp_store(), backend.clone());
        let port = free_port();
        host.start(port, false, None).await.unwrap();
        let host_id = host.endpoint_id().unwrap();
        let key = SecretKey::generate();

        // Unknown phone without a code: refused.
        let (_p, m) = phone(&key, &host_id, port, None).await;
        assert!(matches!(m, HostMsg::Denied { ref reason } if reason.contains("isn't paired")), "{m:?}");

        // Wrong code: refused.
        let (code, _, _) = host.pair_start();
        let (_p, m) = phone(&key, &host_id, port, Some("WRONGCODE1")).await;
        assert!(matches!(m, HostMsg::Denied { .. }), "{m:?}");

        // Right code (lowercase, with a dash): paired after approval, as read-only.
        let typed = format!("{}-{}", &code[..5], &code[5..]).to_lowercase();
        let (mut p, m) = phone(&key, &host_id, port, Some(&typed)).await;
        let HostMsg::Welcome { host: h, device } = m else { panic!("{m:?}") };
        assert_eq!((h.name.as_str(), device.access.as_str(), device.name.as_str()), ("Test Mac", "readonly", "Test Phone"));
        assert_eq!(host.store.devices().len(), 1);
        assert!(host.pairing_expires().is_none(), "code used up");

        // Calls answer by id.
        write_frame(&mut p.send, &ClientMsg::Call { id: 7, cmd: "echo".into(), args: json!({ "x": 1 }) }).await.unwrap();
        let r: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        assert_eq!(r, HostMsg::Result { id: 7, ok: Some(json!({ "args": { "x": 1 }, "readonly": true })), err: None });
        write_frame(&mut p.send, &ClientMsg::Ping).await.unwrap();
        assert_eq!(read_frame::<_, HostMsg>(&mut p.recv).await.unwrap().unwrap(), HostMsg::Pong);

        // Events: terminal output only for attached terminals, broadcasts for all.
        write_frame(&mut p.send, &ClientMsg::Call { id: 8, cmd: "attach".into(), args: json!({ "id": "T1" }) }).await.unwrap();
        let _: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        host.broadcast("pty-output", &json!({ "id": "T2", "data": "eA==" }));
        host.broadcast("secret-event", &json!({}));
        host.broadcast("pty-output", &json!({ "id": "T1", "data": "aGk=" }));
        host.broadcast("agent-status", &json!({ "ptyId": "T1", "status": "idle" }));
        let e1: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        assert_eq!(e1, HostMsg::Event { event: "pty-output".into(), payload: json!({ "id": "T1", "data": "aGk=" }) });
        let e2: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        assert!(matches!(e2, HostMsg::Event { ref event, .. } if event == "agent-status"));

        // DevCat Agent chats: events only for the chats the phone opened.
        write_frame(&mut p.send, &ClientMsg::Call { id: 9, cmd: "attach".into(), args: json!({ "id": "S1" }) }).await.unwrap();
        let _: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        host.broadcast("agent-event", &json!({ "sessionId": "S2", "event": { "type": "text", "text": "no" }, "seq": 1 }));
        host.broadcast("agent-event", &json!({ "sessionId": "S1", "event": { "type": "text", "text": "yes" }, "seq": 1 }));
        host.broadcast("agent-chat", &json!({ "sessionId": "S2", "busy": true }));
        let e3: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        assert!(matches!(e3, HostMsg::Event { ref event, ref payload } if event == "agent-event" && payload["sessionId"] == "S1"), "{e3:?}");
        let e4: HostMsg = read_frame(&mut p.recv).await.unwrap().unwrap();
        assert!(matches!(e4, HostMsg::Event { ref event, .. } if event == "agent-chat"), "{e4:?}");

        // Reconnect with the same key: no code needed any more.
        let (_p2, m) = phone(&key, &host_id, port, None).await;
        assert!(matches!(m, HostMsg::Welcome { .. }), "{m:?}");
        assert_eq!(host.clients().len(), 2);

        // Revoke: the connection drops and the key is refused.
        assert!(host.revoke(&key.public().to_string()));
        let end = tokio::time::timeout(Duration::from_secs(5), read_frame::<_, HostMsg>(&mut p.recv)).await.unwrap();
        assert!(!matches!(end, Ok(Some(_))), "connection closed");
        let (_p3, m) = phone(&key, &host_id, port, None).await;
        assert!(matches!(m, HostMsg::Denied { .. }), "{m:?}");
        host.stop().await;
        assert!(backend.events.lock().iter().any(|e| e == "approve Test Phone"));
    }

    #[tokio::test]
    async fn declined_and_brute_force() {
        let backend = Arc::new(TestBackend { allow: false, events: Mutex::new(vec![]) });
        let host = Host::new(tmp_store(), backend);
        let port = free_port();
        host.start(port, false, None).await.unwrap();
        let id = host.endpoint_id().unwrap();
        let (code, _, _) = host.pair_start();
        let (_p, m) = phone(&SecretKey::generate(), &id, port, Some(&code)).await;
        assert!(matches!(m, HostMsg::Denied { ref reason } if reason.contains("declined")), "{m:?}");
        assert!(host.store.devices().is_empty());

        let (code, _, _) = host.pair_start();
        for _ in 0..PAIR_MAX_ATTEMPTS {
            assert!(!host.take_code("NOPE"));
        }
        assert!(!host.take_code(&code), "too many wrong codes cancel pairing");
        host.stop().await;
    }

    #[tokio::test]
    async fn port_in_use_is_explained() {
        let a = Host::new(tmp_store(), Arc::new(TestBackend { allow: true, events: Mutex::new(vec![]) }));
        let port = free_port();
        a.start(port, false, None).await.unwrap();
        let b = Host::new(tmp_store(), Arc::new(TestBackend { allow: true, events: Mutex::new(vec![]) }));
        let err = b.start(port, false, None).await.unwrap_err();
        assert!(err.contains(&port.to_string()), "{err}");
        a.stop().await;
    }
}
