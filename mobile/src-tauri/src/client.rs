//! Connections from this phone to paired computers: one QUIC connection per computer, made
//! directly to its address and, if it has one, through its relay. Calls are multiplexed over one stream and answered by id;
//! events from the computer are handed to the app. Connections are opened on demand and
//! dropped when the network changes; the app reconnects by calling again.
use crate::store::{Machine, Store, merged_addrs, now_ms};
use devcat_remote_proto::{ALPN, ClientMsg, ConnPath, DeviceInfo, HostInfo, HostMsg, PairLink, RELAY_PUBLIC, read_frame, write_frame};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use std::collections::BTreeSet;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Through a relay (mobile data, another network) connecting takes longer: the relay, then
/// the handshake, then trying to go direct.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the computer's user has to click Allow.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(150);
const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PING_EVERY: Duration = Duration::from_secs(15);
const PONG_DEADLINE_MS: i64 = 45_000;

/// Something for the UI: `m-conn` (connection state) or `m-event` (an event from a computer)
/// or `m-pair` (pairing progress).
pub type Sink = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub name: String,
    pub platform: String,
    pub app_version: String,
}

pub struct Session {
    conn: Connection,
    tx: mpsc::UnboundedSender<ClientMsg>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Value>>>>>,
    next: AtomicU64,
    pub host: HostInfo,
    pub device: DeviceInfo,
}

impl Session {
    async fn call(&self, cmd: &str, args: Value) -> Result<Value, Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        if self.tx.send(ClientMsg::Call { id, cmd: cmd.to_string(), args }).is_err() {
            self.pending.lock().remove(&id);
            return Err(json!("Disconnected from the computer"));
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(json!("Disconnected from the computer")),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(json!("The computer didn't answer in time"))
            }
        }
    }
}

pub struct Client {
    key: SecretKey,
    store: Arc<Store>,
    sink: Sink,
    hello: Mutex<Hello>,
    /// The endpoint and the relays it was made with.
    endpoint: tokio::sync::Mutex<Option<(Endpoint, BTreeSet<String>)>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// One connect at a time per computer.
    connecting: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// "ip:port" / "host:port" → socket addresses (host names resolved; bad entries skipped).
async fn resolve(addrs: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for a in addrs {
        if let Ok(sa) = a.parse::<SocketAddr>() {
            out.push(sa);
        } else if let Ok(Ok(found)) = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(a.as_str())).await {
            out.extend(found);
        }
    }
    out.dedup();
    out
}

/// The path a connection uses right now (the selected one).
fn conn_path(conn: &Connection) -> Option<ConnPath> {
    let paths = conn.paths();
    let p = paths.iter().find(|p| p.is_selected())?;
    let relay = match p.remote_addr() {
        iroh::TransportAddr::Relay(url) => Some(url.to_string()),
        _ => None,
    };
    Some(ConnPath { direct: p.is_ip(), rtt_ms: p.rtt().as_millis() as u64, relay })
}

fn friendly(e: impl std::fmt::Display, name: &str) -> String {
    let s = e.to_string();
    let low = s.to_lowercase();
    if low.contains("timed out") || low.contains("timeout") || low.contains("no addressing") || low.contains("unreachable") || low.contains("no address") {
        format!("Couldn't reach {name}. Check that DevCat is open there with Remote access on, and that this phone has internet (or is on the same Wi-Fi when relays are off).")
    } else {
        format!("Couldn't connect to {name}: {s}")
    }
}

impl Client {
    pub fn new(store: Arc<Store>, sink: Sink, hello: Hello) -> Result<Arc<Self>, String> {
        let key = SecretKey::from_bytes(&store.secret_key()?);
        Ok(Arc::new(Self {
            key,
            store,
            sink,
            hello: Mutex::new(hello),
            endpoint: tokio::sync::Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashMap::new()),
        }))
    }

    pub fn device_id(&self) -> String {
        self.key.public().to_string()
    }

    pub fn set_name(&self, name: String) {
        self.hello.lock().name = name;
    }

    pub fn hello(&self) -> Hello {
        self.hello.lock().clone()
    }

    /// The phone's endpoint, able to use the relays of every paired computer (and `extra`, the
    /// relay of a computer being paired). Made again when a new relay is needed.
    async fn endpoint(&self, extra: Option<&str>) -> Result<Endpoint, String> {
        let mut want: BTreeSet<String> = self.store.machines().into_iter().filter_map(|m| m.relay).collect();
        want.extend(extra.map(str::to_string));
        let mut ep = self.endpoint.lock().await;
        if let Some((e, has)) = ep.as_ref() {
            if want.is_subset(has) {
                return Ok(e.clone());
            }
        }
        if let Some((old, _)) = ep.take() {
            let dropped: Vec<String> = self.sessions.lock().drain().map(|(id, _)| id).collect();
            old.close().await;
            for id in dropped {
                self.state(&id, "offline", None);
            }
        }
        let customs: Vec<RelayUrl> = want.iter().filter(|r| r.as_str() != RELAY_PUBLIC).filter_map(|r| r.parse().ok()).collect();
        let builder = if want.contains(RELAY_PUBLIC) {
            // iroh's public relays, plus its DNS lookup to find which one the computer is on.
            let map = RelayMode::Default.relay_map();
            map.extend(&RelayMode::custom(customs).relay_map());
            Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(map))
        } else if !customs.is_empty() {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(customs))
        } else {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        };
        let e = builder.secret_key(self.key.clone()).bind().await.map_err(|e| format!("Couldn't start networking: {e}"))?;
        *ep = Some((e.clone(), want));
        Ok(e)
    }

    fn state(&self, id: &str, state: &str, error: Option<String>) {
        (self.sink)("m-conn", json!({ "id": id, "state": state, "error": error }));
    }

    pub fn online(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }

    /// Open a connection (used for both pairing and normal use).
    /// `relay`: the computer's relay ([`RELAY_PUBLIC`]: found through iroh's DNS lookup).
    /// `home`: the relay server it was last seen on, tried straight away.
    async fn open(self: &Arc<Self>, machine_id: &str, name: &str, addrs: &[String], relay: Option<&str>, home: Option<&str>, pair_code: Option<String>) -> Result<Arc<Session>, String> {
        let ep = self.endpoint(relay).await?;
        let id = machine_id.parse().map_err(|_| "Invalid computer id".to_string())?;
        let sock = resolve(addrs).await;
        if sock.is_empty() && relay.is_none() {
            return Err(format!("{name} has no address this phone can use. Add one in the computer's settings on this phone, or turn on relays on the computer."));
        }
        let mut addr = EndpointAddr::new(id);
        for s in sock {
            addr = addr.with_ip_addr(s);
        }
        let urls = [relay.filter(|r| *r != RELAY_PUBLIC), relay.and(home)];
        for url in urls.into_iter().flatten().filter_map(|r| r.parse::<RelayUrl>().ok()) {
            addr = addr.with_relay_url(url);
        }
        let wait = if relay.is_some() { RELAY_CONNECT_TIMEOUT } else { CONNECT_TIMEOUT };
        let conn = tokio::time::timeout(wait, ep.connect(addr, ALPN))
            .await
            .map_err(|_| friendly("timed out", name))?
            .map_err(|e| friendly(e, name))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| friendly(e, name))?;
        let h = self.hello();
        let pairing = pair_code.is_some();
        write_frame(&mut send, &ClientMsg::Hello { name: h.name, platform: h.platform, app_version: h.app_version, pair_code }).await.map_err(|e| friendly(e, name))?;
        let first_wait = if pairing { CONNECT_TIMEOUT * 2 } else { CONNECT_TIMEOUT };
        let mut first: Option<HostMsg> = tokio::time::timeout(first_wait, read_frame(&mut recv)).await.map_err(|_| friendly("timed out", name))?.map_err(|e| friendly(e, name))?;
        if let Some(HostMsg::Pairing { host_name }) = &first {
            (self.sink)("m-pair", json!({ "stage": "waiting", "hostName": host_name }));
            first = tokio::time::timeout(APPROVAL_TIMEOUT, read_frame(&mut recv))
                .await
                .map_err(|_| "Nobody approved this phone on the computer in time. Try again.".to_string())?
                .map_err(|e| friendly(e, name))?;
        }
        let (host, device) = match first {
            Some(HostMsg::Welcome { host, device }) => (host, device),
            Some(HostMsg::Denied { reason }) => {
                conn.close(0u32.into(), b"denied");
                return Err(reason);
            }
            _ => return Err(format!("{name} closed the connection")),
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Value>>>>> = Arc::default();
        let session = Arc::new(Session { conn: conn.clone(), tx: tx.clone(), pending: pending.clone(), next: AtomicU64::new(1), host, device });

        // writer
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if write_frame(&mut send, &m).await.is_err() {
                    break;
                }
            }
        });
        // keep-alive
        let last_pong = Arc::new(AtomicI64::new(now_ms()));
        let (lp, ping_tx, ping_conn) = (last_pong.clone(), tx.clone(), conn.clone());
        let pinger = tokio::spawn(async move {
            loop {
                tokio::time::sleep(PING_EVERY).await;
                if now_ms() - lp.load(Ordering::Relaxed) > PONG_DEADLINE_MS {
                    ping_conn.close(0u32.into(), b"no answer");
                    break;
                }
                if ping_tx.send(ClientMsg::Ping).is_err() {
                    break;
                }
            }
        });
        // relay → direct (and back), as it happens
        let (me, mid, path_conn) = (self.clone(), machine_id.to_string(), conn.clone());
        let paths = tokio::spawn(async move {
            let mut events = path_conn.path_events();
            loop {
                (me.sink)("m-path", json!({ "id": mid, "path": conn_path(&path_conn) }));
                if n0_future::StreamExt::next(&mut events).await.is_none() {
                    break;
                }
            }
        });
        // reader
        let (me, mid, sess) = (self.clone(), machine_id.to_string(), session.clone());
        tokio::spawn(async move {
            loop {
                match read_frame::<_, HostMsg>(&mut recv).await {
                    Ok(Some(HostMsg::Result { id, ok, err })) => {
                        if let Some(w) = pending.lock().remove(&id) {
                            let _ = w.send(match err {
                                Some(e) => Err(e),
                                None => Ok(ok.unwrap_or(Value::Null)),
                            });
                        }
                    }
                    Ok(Some(HostMsg::Event { event, payload })) => (me.sink)("m-event", json!({ "id": mid, "event": event, "payload": payload })),
                    Ok(Some(HostMsg::Pong)) => last_pong.store(now_ms(), Ordering::Relaxed),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            writer.abort();
            pinger.abort();
            paths.abort();
            for (_, w) in pending.lock().drain() {
                let _ = w.send(Err(json!("Disconnected from the computer")));
            }
            let reason = match sess.conn.close_reason() {
                Some(iroh::endpoint::ConnectionError::ApplicationClosed(c)) => Some(String::from_utf8_lossy(&c.reason).into_owned()).filter(|r| !r.is_empty()),
                _ => None,
            };
            let removed = {
                let mut s = me.sessions.lock();
                if s.get(&mid).is_some_and(|x| Arc::ptr_eq(x, &sess)) { s.remove(&mid).is_some() } else { false }
            };
            if removed {
                me.state(&mid, "offline", reason);
            }
        });
        Ok(session)
    }

    /// Connected session for a paired computer, connecting if needed.
    pub async fn session(self: &Arc<Self>, machine_id: &str) -> Result<Arc<Session>, String> {
        if let Some(s) = self.sessions.lock().get(machine_id).cloned() {
            return Ok(s);
        }
        let lock = self.connecting.lock().entry(machine_id.to_string()).or_default().clone();
        let _guard = lock.lock().await;
        if let Some(s) = self.sessions.lock().get(machine_id).cloned() {
            return Ok(s);
        }
        let m = self.store.machine(machine_id).ok_or_else(|| "This computer isn't paired any more".to_string())?;
        self.state(machine_id, "connecting", None);
        match self.open(&m.id, &m.name, &m.addrs, m.relay.as_deref(), m.home.as_deref(), None).await {
            Ok(s) => {
                self.remember(&m, &s);
                self.sessions.lock().insert(m.id.clone(), s.clone());
                self.state(machine_id, "online", None);
                Ok(s)
            }
            Err(e) => {
                self.state(machine_id, "offline", Some(e.clone()));
                Err(e)
            }
        }
    }

    /// Keep what the computer told us (name, addresses, access).
    fn remember(&self, m: &Machine, s: &Session) {
        let _ = self.store.update(&m.id, |x| {
            x.name = s.host.name.clone();
            x.os = s.host.os.clone();
            x.app_version = s.host.app_version.clone();
            x.access = s.device.access.clone();
            x.addrs = merged_addrs(&x.manual_addrs, if s.host.addrs.is_empty() { &m.addrs } else { &s.host.addrs });
            x.relay = s.host.relay.clone();
            x.home = s.host.home.clone().or(x.home.take());
            x.last_seen = Some(now_ms());
        });
    }

    pub async fn call(self: &Arc<Self>, machine_id: &str, cmd: &str, args: Value) -> Result<Value, Value> {
        let s = self.session(machine_id).await.map_err(Value::String)?;
        s.call(cmd, args).await
    }

    /// Pair with the computer in a pairing link (QR code). `extra_addr`: an address the user
    /// typed, tried first.
    pub async fn pair(self: &Arc<Self>, link: &str, extra_addr: Option<String>) -> Result<Machine, String> {
        let l = PairLink::parse(link)?;
        let manual: Vec<String> = extra_addr.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).into_iter().collect();
        let addrs = merged_addrs(&manual, &l.addrs);
        (self.sink)("m-pair", json!({ "stage": "connecting", "hostName": l.name }));
        let s = self.open(&l.id, &l.name, &addrs, l.relay.as_deref(), l.home.as_deref(), Some(l.code.clone())).await?;
        let m = Machine {
            id: l.id.clone(),
            name: s.host.name.clone(),
            addrs: merged_addrs(&manual, if s.host.addrs.is_empty() { &addrs } else { &s.host.addrs }),
            os: s.host.os.clone(),
            app_version: s.host.app_version.clone(),
            access: s.device.access.clone(),
            added_at: now_ms(),
            last_seen: Some(now_ms()),
            manual_addrs: manual,
            relay: s.host.relay.clone().or(l.relay.clone()),
            home: s.host.home.clone().or(l.home.clone()),
        };
        self.store.upsert(m.clone())?;
        if let Some(old) = self.sessions.lock().insert(m.id.clone(), s) {
            old.conn.close(0u32.into(), b"replaced");
        }
        self.state(&m.id, "online", None);
        (self.sink)("m-pair", json!({ "stage": "done", "hostName": m.name }));
        Ok(m)
    }

    pub fn disconnect(&self, machine_id: &str) {
        if let Some(s) = self.sessions.lock().remove(machine_id) {
            s.conn.close(0u32.into(), b"bye");
            self.state(machine_id, "offline", None);
        }
    }

    /// The network changed (Wi-Fi ↔ cellular, app back from background): drop connections and
    /// rebind so the next call reconnects cleanly.
    pub async fn network_changed(&self) {
        let ids: Vec<String> = self.sessions.lock().keys().cloned().collect();
        for id in ids {
            self.disconnect(&id);
        }
        if let Some((ep, _)) = self.endpoint.lock().await.as_ref() {
            ep.network_change().await;
        }
    }

    pub fn forget(&self, machine_id: &str) -> Result<(), String> {
        self.disconnect(machine_id);
        self.store.remove(machine_id)
    }
}
