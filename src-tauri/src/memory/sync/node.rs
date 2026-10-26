//! Memory sync on the network: this computer's sync endpoint (its own iroh key, separate from
//! phone pairing and rooms), pairing with another computer, and the sync sessions. The app parts
//! (asking the user, the computer's name, the projects, events) sit behind [`Host`] so two nodes
//! can be tested in one process.
//!
//! While at least one computer is paired, the node syncs with each of them when it starts, when a
//! shared file changes here (checked every few seconds), every minute (to catch up after being
//! offline), and on demand. Either side may start a session; the files are only touched under the
//! index lock, and every change is applied only if the file is still the version that was compared.
use super::plan::{self, Action, Index, Item, Places};
use super::store::{Peer, Store};
use super::wire::{self, Ask, File, FileKey, Hello, Hints, Put, Reply};
use crate::models::Project;
use crate::remote::store::AuditEntry;
use async_trait::async_trait;
use devcat_remote_proto::{PAIR_SCHEME, PairLink, RELAY_PUBLIC, read_frame, write_frame};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

const CONNECT: Duration = Duration::from_secs(20);
const ANSWER: Duration = Duration::from_secs(30);
/// How long the other computer's user has to answer a pairing request.
pub const APPROVAL: Duration = Duration::from_secs(3 * 60);
const PAIR_TTL: Duration = Duration::from_secs(10 * 60);
const PAIR_MAX_ATTEMPTS: u32 = 5;
/// A whole sync session (both directions).
const SESSION: Duration = Duration::from_secs(120);
/// How often the shared files are checked for changes.
const TICK: Duration = Duration::from_secs(5);
/// Sync with every computer at least this often (catch up after being offline).
const ROUND_EVERY: Duration = Duration::from_secs(60);

/// Pairing code alphabet: no 0/O/1/I to make it easy to type.
const CODE_ALPHA: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub struct Config {
    /// Sync's own files (`store.rs`).
    pub dir: PathBuf,
    /// The memory folder.
    pub memory: PathBuf,
    /// [`RELAY_PUBLIC`], a relay URL, or none (direct connections only).
    pub relay: Option<String>,
    /// UDP port (0: any free one).
    pub port: u16,
    /// Sync by itself (start, changes, every minute); off in tests.
    pub auto: bool,
}

/// Another computer with this computer's code asks to pair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub request_id: String,
    pub peer_id: String,
    pub name: String,
    pub os: String,
}

/// What the app provides.
#[async_trait]
pub trait Host: Send + Sync + 'static {
    /// Ask the user whether this computer may pair (no answer within [`APPROVAL`]: no).
    async fn approve(&self, req: PairRequest) -> bool;
    /// This computer's name, as the other computers show it.
    fn name(&self) -> String;
    fn projects(&self) -> Vec<Project>;
    /// Peers, pairing or a sync's state changed.
    fn changed(&self);
    /// Memories arrived from another computer (or were deleted there).
    fn memories_changed(&self);
}

/// What one sync moved.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub received: usize,
    pub sent: usize,
    pub deleted: usize,
    /// Conflicts, each kept as a copy next to the winner.
    pub conflicts: usize,
    /// Changed again meanwhile, or refused (too big…): the next sync tries again.
    pub skipped: usize,
    /// Memories of projects this computer doesn't have.
    pub elsewhere: usize,
}

impl Report {
    fn add(&mut self, o: &Report) {
        self.received += o.received;
        self.sent += o.sent;
        self.deleted += o.deleted;
        self.conflicts += o.conflicts;
        self.skipped += o.skipped;
        self.elsewhere = self.elsewhere.max(o.elsewhere);
    }

    pub fn moved(&self) -> bool {
        self.received + self.sent + self.deleted + self.conflicts > 0
    }

    /// "2 received, 1 sent, 1 conflict kept"
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let mut part = |n: usize, what: &str| {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        };
        part(self.received, "received");
        part(self.sent, "sent");
        part(self.deleted, "deleted");
        part(self.conflicts, if self.conflicts == 1 { "conflict kept" } else { "conflicts kept" });
        if parts.is_empty() { "up to date".into() } else { parts.join(", ") }
    }
}

struct Pairing {
    code: String,
    expires: Instant,
    attempts: u32,
}

pub struct Node {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: Config,
    store: Store,
    endpoint: Endpoint,
    router: Mutex<Option<Router>>,
    host: Arc<dyn Host>,
    /// The index, and the lock for touching shared files.
    index: Mutex<Index>,
    pairing: Mutex<Option<Pairing>>,
    /// Computers this one is syncing with right now (as the caller).
    busy: Mutex<HashSet<String>>,
    kick: Notify,
    task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Debug, Clone)]
struct Proto(Weak<Inner>);

impl ProtocolHandler for Proto {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if let Some(inner) = self.0.upgrade() {
            inner.serve(conn).await;
        }
        Ok(())
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn clean_name(n: &str) -> String {
    let n: String = n.chars().filter(|c| !c.is_control()).take(60).collect();
    if n.trim().is_empty() { "Computer".into() } else { n.trim().to_string() }
}

fn clean_os(os: &str) -> String {
    os.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect()
}

/// Read a ticket (`devcat://memory-sync?…`, see [`wire::SCHEME`]).
pub fn parse_ticket(s: &str) -> Result<PairLink> {
    let rest = s.trim().strip_prefix(wire::SCHEME).ok_or("That isn't a memory sync code: it starts with devcat://memory-sync (DevCat → Memory → Other computers on the other computer).")?;
    PairLink::parse(&format!("{PAIR_SCHEME}{rest}"))
}

fn addr_of(id: EndpointId, h: &Hints) -> EndpointAddr {
    let mut a = EndpointAddr::new(id);
    if let Some(u) = h.relay.as_deref().and_then(|u| u.parse::<RelayUrl>().ok()) {
        a = a.with_relay_url(u);
    }
    for s in h.addrs.iter().filter_map(|s| s.parse::<SocketAddr>().ok()) {
        a = a.with_ip_addr(s);
    }
    a
}

async fn send(s: &mut SendStream, r: &impl Serialize) -> Result<()> {
    write_frame(s, r).await.map_err(|e| e.to_string())
}

async fn recv<T: for<'de> serde::Deserialize<'de>>(r: &mut RecvStream, wait: Duration) -> Result<T> {
    match tokio::time::timeout(wait, read_frame::<_, T>(r)).await {
        Ok(Ok(Some(m))) => Ok(m),
        Ok(Ok(None)) => Err("the other computer hung up".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the other computer stopped answering".into()),
    }
}

impl Node {
    pub async fn start(cfg: Config, host: Arc<dyn Host>) -> Result<Node> {
        let store = Store::new(&cfg.dir);
        let secret = SecretKey::from_bytes(&store.secret_key().map_err(|e| e.to_string())?);
        let builder = match cfg.relay.as_deref() {
            None => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
            Some(RELAY_PUBLIC) => Endpoint::builder(presets::N0),
            Some(url) => {
                let url: RelayUrl = url.parse().map_err(|_| format!("Not a relay address: {url}"))?;
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom([url]))
            }
        };
        let endpoint = builder
            .secret_key(secret)
            .bind_addr(format!("0.0.0.0:{}", cfg.port).as_str())
            .map_err(|e| e.to_string())?
            .bind()
            .await
            .map_err(|e| format!("Couldn't start memory sync: {e}"))?;
        let index = store.index();
        let inner = Arc::new(Inner {
            cfg,
            store,
            endpoint: endpoint.clone(),
            router: Mutex::new(None),
            host,
            index: Mutex::new(index),
            pairing: Mutex::new(None),
            busy: Mutex::new(HashSet::new()),
            kick: Notify::new(),
            task: Mutex::new(None),
        });
        let router = Router::builder(endpoint).accept(wire::ALPN, Proto(Arc::downgrade(&inner))).spawn();
        *inner.router.lock() = Some(router);
        if inner.cfg.auto {
            let me = inner.clone();
            *inner.task.lock() = Some(tokio::spawn(async move { me.run().await }));
        }
        Ok(Node { inner })
    }

    /// This computer's sync id.
    pub fn id(&self) -> String {
        self.inner.endpoint.id().to_string()
    }

    pub async fn shutdown(&self) {
        if let Some(t) = self.inner.task.lock().take() {
            t.abort();
        }
        let router = self.inner.router.lock().take();
        if let Some(r) = router {
            let _ = r.shutdown().await;
        }
    }

    #[cfg(test)]
    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn syncing(&self) -> Vec<String> {
        self.inner.busy.lock().iter().cloned().collect()
    }

    /// A new one-time code, as a ticket to paste on the other computer: (ticket, code, expires ms).
    pub async fn pair_start(&self) -> (String, String, i64) {
        if self.inner.cfg.relay.is_some() {
            // the ticket carries the relay: wait briefly for it after a cold start
            let _ = tokio::time::timeout(Duration::from_secs(8), self.inner.endpoint.online()).await;
        }
        let bytes: [u8; 10] = crate::remote::store::random_bytes();
        let code: String = bytes.iter().map(|b| CODE_ALPHA[(*b as usize) % CODE_ALPHA.len()] as char).collect();
        *self.inner.pairing.lock() = Some(Pairing { code: code.clone(), expires: Instant::now() + PAIR_TTL, attempts: 0 });
        let h = self.inner.my_hints();
        let link = PairLink { id: self.id(), code: code.clone(), name: self.inner.host.name(), addrs: h.addrs, relay: self.inner.cfg.relay.clone(), home: h.relay };
        let ticket = link.to_url().replacen(PAIR_SCHEME, wire::SCHEME, 1);
        (ticket, code, now_ms() + PAIR_TTL.as_millis() as i64)
    }

    pub fn pair_cancel(&self) {
        *self.inner.pairing.lock() = None;
    }

    pub fn pairing_live(&self) -> bool {
        self.inner.pairing.lock().as_ref().is_some_and(|p| p.expires > Instant::now())
    }

    /// Pair with the computer that showed `ticket`; its user must allow it. Then sync with it.
    pub async fn join(&self, ticket: &str) -> Result<Peer> {
        let link = parse_ticket(ticket)?;
        if link.id == self.id() {
            return Err("That's this computer's own code: paste it on the other computer.".into());
        }
        let inner = &self.inner;
        let hints = Hints { relay: link.home.clone().or(link.relay.clone().filter(|r| r != RELAY_PUBLIC)), addrs: link.addrs.clone() };
        let conn = inner.dial(&link.id, &hints).await.map_err(|e| format!("Couldn't reach {}: {e}. DevCat needs to be open there.", link.name))?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        send(&mut tx, &Hello::Pair { code: link.code.clone(), name: inner.host.name(), os: std::env::consts::OS.into(), hints: inner.my_hints() }).await?;
        let answer = loop {
            match recv::<Reply>(&mut rx, APPROVAL + ANSWER).await? {
                Reply::Waiting => continue,
                r => break r,
            }
        };
        conn.close(0u32.into(), b"done");
        match answer {
            Reply::Paired { name, os } => {
                let peer = Peer { id: link.id.clone(), name: clean_name(&name), os: clean_os(&os), paired_at: now_ms(), last_sync: None, last_result: None, last_error: None, hints };
                inner.store.add_peer(peer.clone()).map_err(|e| e.to_string())?;
                inner.audit(&peer.id, &peer.name, "paired", None, true);
                inner.host.changed();
                inner.kick.notify_one();
                Ok(peer)
            }
            Reply::Denied { reason } => Err(reason),
            _ => Err("The other computer gave an unexpected answer".into()),
        }
    }

    /// Unpair a computer (it can't sync with this one any more).
    pub fn remove(&self, id: &str) -> bool {
        let Some(p) = self.inner.store.remove_peer(id) else { return false };
        self.inner.audit(&p.id, &p.name, "unpaired", None, true);
        self.inner.host.changed();
        true
    }

    /// Sync with one computer now.
    pub async fn sync_peer(&self, id: &str) -> Result<Report> {
        self.inner.clone().sync_peer(id).await
    }

    /// Sync with every computer soon (in the background).
    pub fn sync_all(&self) {
        self.inner.sync_all();
    }
}

/// Removes a computer from `busy` when its sync ends, however it ends.
struct Busy<'a>(&'a Inner, String);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.lock().remove(&self.1);
        self.0.host.changed();
    }
}

impl Inner {
    fn my_hints(&self) -> Hints {
        let a = self.endpoint.addr();
        Hints { relay: a.relay_urls().next().map(|u| u.to_string()), addrs: a.ip_addrs().map(|s| s.to_string()).collect() }
    }

    async fn dial(&self, peer: &str, hints: &Hints) -> Result<Connection> {
        let id: EndpointId = peer.parse().map_err(|_| "bad computer id".to_string())?;
        tokio::time::timeout(CONNECT, self.endpoint.connect(addr_of(id, hints), wire::ALPN)).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())
    }

    fn audit(&self, id: &str, name: &str, action: &str, detail: Option<String>, ok: bool) {
        self.store.audit(&AuditEntry { at: now_ms(), device: id.to_string(), device_name: name.to_string(), action: action.to_string(), detail, ok });
    }

    /// Check a pairing code; it's used up when it matches. Too many wrong tries cancel pairing.
    fn take_code(&self, code: &str) -> bool {
        let mut p = self.pairing.lock();
        let Some(cur) = p.as_mut() else { return false };
        if cur.expires <= Instant::now() {
            *p = None;
            return false;
        }
        let typed: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
        if crate::remote::constant_time_eq(typed.as_bytes(), cur.code.as_bytes()) {
            *p = None;
            return true;
        }
        cur.attempts += 1;
        if cur.attempts >= PAIR_MAX_ATTEMPTS {
            *p = None;
        }
        false
    }

    /// Scan the shared files and list them by place key.
    fn local_manifest(&self) -> (Vec<Item>, Places) {
        let projects = self.host.projects();
        let places = Places::new(&self.cfg.memory, &projects);
        let mut idx = self.index.lock();
        if plan::scan(&self.cfg.memory, &mut idx, now_ms()) {
            let _ = self.store.save_index(&idx);
        }
        (plan::manifest(&idx, &places), places)
    }

    async fn run(self: Arc<Self>) {
        let mut last_fp = String::new();
        let mut last_round: Option<Instant> = None;
        loop {
            let forced = tokio::select! {
                _ = tokio::time::sleep(TICK) => false,
                _ = self.kick.notified() => true,
            };
            if self.store.peers().is_empty() {
                continue;
            }
            let fp = plan::fingerprint(&self.cfg.memory);
            if forced || fp != last_fp || last_round.is_none_or(|t| t.elapsed() >= ROUND_EVERY) {
                last_fp = fp;
                last_round = Some(Instant::now());
                self.sync_all();
            }
        }
    }

    fn sync_all(self: &Arc<Self>) {
        for p in self.store.peers() {
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.sync_peer(&p.id).await;
            });
        }
    }

    async fn sync_peer(self: Arc<Self>, id: &str) -> Result<Report> {
        let peer = self.store.peer(id).ok_or("That computer isn't paired")?;
        if !self.busy.lock().insert(id.to_string()) {
            return Ok(Report::default());
        }
        let _busy = Busy(&self, id.to_string());
        self.host.changed();
        let mut total = Report::default();
        let mut result = Ok(());
        // a conflict leaves a copy here that the other computer doesn't have yet: one more round
        for _ in 0..2 {
            match tokio::time::timeout(SESSION, self.session(&peer)).await {
                Ok(Ok(r)) => {
                    total.add(&r);
                    if r.conflicts == 0 {
                        break;
                    }
                }
                Ok(Err(e)) => {
                    result = Err(e);
                    break;
                }
                Err(_) => {
                    result = Err("the sync took too long".to_string());
                    break;
                }
            }
        }
        match &result {
            Ok(()) => {
                self.store.update_peer(id, |p| {
                    p.last_sync = Some(now_ms());
                    p.last_error = None;
                    if total.moved() {
                        p.last_result = Some(total.summary());
                    }
                });
                if total.moved() {
                    self.audit(id, &peer.name, "synced", Some(total.summary()), true);
                }
            }
            Err(e) => {
                // log a failure once, not every minute while the other computer is off
                if peer.last_error.as_deref() != Some(e.as_str()) {
                    self.audit(id, &peer.name, "sync failed", Some(e.clone()), false);
                }
                self.store.update_peer(id, |p| p.last_error = Some(e.clone()));
            }
        }
        if total.received + total.deleted + total.conflicts > 0 {
            self.host.memories_changed();
        }
        result.map(|_| total)
    }

    /// One sync session with `peer`, as the caller.
    async fn session(self: &Arc<Self>, peer: &Peer) -> Result<Report> {
        let (mine, places) = self.local_manifest();
        let conn = self.dial(&peer.id, &peer.hints).await.map_err(|e| format!("Couldn't reach {}: {e}", peer.name))?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let res = self.session_on(&mut tx, &mut rx, peer, mine, &places).await;
        let _ = send(&mut tx, &Ask::Bye).await;
        let _ = tx.finish();
        conn.close(0u32.into(), b"done");
        res
    }

    async fn session_on(&self, tx: &mut SendStream, rx: &mut RecvStream, peer: &Peer, mine: Vec<Item>, places: &Places) -> Result<Report> {
        send(tx, &Hello::Sync { name: self.host.name(), hints: self.my_hints() }).await?;
        let (peer_name, theirs) = match recv::<Reply>(rx, ANSWER).await? {
            Reply::Manifest { name, hints, manifest } => {
                let name = clean_name(&name);
                self.store.update_peer(&peer.id, |p| {
                    p.name = name.clone();
                    if hints.relay.is_some() || !hints.addrs.is_empty() {
                        p.hints = hints;
                    }
                });
                (name, manifest)
            }
            Reply::Denied { reason } => return Err(reason),
            _ => return Err("The other computer gave an unexpected answer".into()),
        };
        let key = |i: &Item| FileKey { place: i.place.clone(), path: i.path.clone() };
        let local: HashMap<FileKey, Item> = mine.into_iter().map(|i| (key(&i), i)).collect();
        let remote: HashMap<FileKey, Item> = theirs.into_iter().filter(plan::valid_item).map(|i| (key(&i), i)).collect();
        let all: BTreeSet<FileKey> = local.keys().chain(remote.keys()).cloned().collect();
        let mut report = Report::default();
        let mut todo: Vec<(FileKey, Action)> = Vec::new();
        for k in all {
            let a = plan::decide(local.get(&k), remote.get(&k));
            if a == Action::Same {
                continue;
            }
            // only files of places this computer has (a file here always has its place)
            if places.resolve(&k.place).is_none() {
                report.elsewhere += 1;
                continue;
            }
            todo.push((k, a));
        }

        // the other computer's texts this one needs
        let want: Vec<FileKey> = todo.iter().filter(|(_, a)| matches!(a, Action::Pull | Action::Conflict { .. })).map(|(k, _)| k.clone()).collect();
        let mut texts: HashMap<FileKey, String> = HashMap::new();
        for chunk in want.chunks(wire::BATCH) {
            send(tx, &Ask::Get { files: chunk.to_vec() }).await?;
            match recv::<Reply>(rx, ANSWER).await? {
                Reply::Files { files } => {
                    for f in files {
                        if let Some(t) = f.text {
                            texts.insert(FileKey { place: f.place, path: f.path }, t);
                        }
                    }
                }
                _ => return Err("The other computer gave an unexpected answer".into()),
            }
        }

        // apply here, and collect what the other computer takes
        let root = &self.cfg.memory;
        let me = self.host.name();
        let mut puts: Vec<Put> = Vec::new();
        {
            let mut idx = self.index.lock();
            let mut touched = false;
            for (k, a) in &todo {
                let (dir, place) = places.resolve(&k.place).cloned().expect("checked above");
                let (l, r) = (local.get(k), remote.get(k));
                let have = l.and_then(|i| i.hash.clone());
                let theirs_hash = r.and_then(|i| i.hash.clone());
                let file = root.join(&dir).join(&k.path);
                // this computer's text, if the file is still the version compared
                let mine_now = || std::fs::read_to_string(&file).ok().filter(|t| Some(super::super::hash(t.as_bytes())) == have);
                let mut take = |idx: &mut Index, text: Option<&str>, report: &mut Report| {
                    let item = r.expect("the other computer has a version");
                    match plan::apply(root, idx, &dir, &place, &k.path, have.as_deref(), item, text) {
                        Ok(true) => {
                            touched = true;
                            if item.hash.is_some() { report.received += 1 } else { report.deleted += 1 }
                        }
                        Ok(false) => report.skipped += 1,
                        Err(e) => {
                            report.skipped += 1;
                            eprintln!("memory sync: {}/{}: {e}", k.place, k.path);
                        }
                    }
                };
                match *a {
                    Action::Same => {}
                    Action::Pull => match texts.get(k) {
                        Some(t) => take(&mut idx, Some(t), &mut report),
                        None => report.skipped += 1,
                    },
                    Action::DeleteLocal => take(&mut idx, None, &mut report),
                    Action::Push | Action::DeleteRemote => {
                        let item = l.expect("this computer has a version").clone();
                        let text = if item.hash.is_some() {
                            match mine_now() {
                                Some(t) => Some(super::super::scrub(&t)),
                                None => {
                                    report.skipped += 1;
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        puts.push(Put { item, expect: theirs_hash, text });
                    }
                    Action::Conflict { local_wins } => {
                        let (Some(rtext), Some(ltext)) = (texts.get(k), mine_now()) else {
                            report.skipped += 1;
                            continue;
                        };
                        let item = l.expect("both have it").clone();
                        if plan::same_but_updated(&ltext, rtext) {
                            // only the save time differs: the newer one, without a copy
                            if local_wins {
                                puts.push(Put { item, expect: theirs_hash, text: Some(super::super::scrub(&ltext)) });
                            } else {
                                take(&mut idx, Some(rtext), &mut report);
                            }
                            continue;
                        }
                        report.conflicts += 1;
                        if local_wins {
                            // keep theirs here as a copy (it reaches them next round), they take mine
                            if let Err(e) = plan::write_conflict_copy(root, &dir, &place, &k.path, rtext, &peer_name) {
                                eprintln!("memory sync: conflict copy of {}: {e}", k.path);
                                continue;
                            }
                            puts.push(Put { item, expect: theirs_hash, text: Some(super::super::scrub(&ltext)) });
                        } else {
                            if let Err(e) = plan::write_conflict_copy(root, &dir, &place, &k.path, &ltext, &me) {
                                eprintln!("memory sync: conflict copy of {}: {e}", k.path);
                                continue;
                            }
                            take(&mut idx, Some(rtext), &mut report);
                        }
                    }
                }
            }
            if touched {
                let _ = self.store.save_index(&idx);
            }
        }

        for chunk in puts.chunks(wire::BATCH) {
            send(tx, &Ask::Put { changes: chunk.to_vec() }).await?;
            match recv::<Reply>(rx, ANSWER).await? {
                Reply::Done { applied, skipped } => {
                    report.sent += applied;
                    report.skipped += skipped;
                }
                _ => return Err("The other computer gave an unexpected answer".into()),
            }
        }
        Ok(report)
    }

    // ------------------------------------------------------------ incoming

    async fn serve(self: Arc<Self>, conn: Connection) {
        let peer = conn.remote_id().to_string();
        let Ok(Ok((mut tx, mut rx))) = tokio::time::timeout(ANSWER, conn.accept_bi()).await else { return };
        let Ok(hello) = recv::<Hello>(&mut rx, ANSWER).await else { return };
        match hello {
            Hello::Pair { code, name, os, hints } => self.serve_pair(&conn, &mut tx, &peer, &code, &name, &os, hints).await,
            Hello::Sync { name, hints } => self.serve_sync(&mut tx, &mut rx, &peer, &name, hints).await,
        }
        let _ = tx.finish();
        let _ = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn serve_pair(&self, conn: &Connection, tx: &mut SendStream, peer: &str, code: &str, name: &str, os: &str, hints: Hints) {
        let me = self.host.name();
        if !self.take_code(code) {
            let _ = send(tx, &Reply::Denied { reason: format!("The code is wrong or expired. Make a new one on {me}.") }).await;
            return;
        }
        if send(tx, &Reply::Waiting).await.is_err() {
            return;
        }
        let req = PairRequest { request_id: uuid::Uuid::new_v4().to_string(), peer_id: peer.to_string(), name: clean_name(name), os: clean_os(os) };
        let allowed = tokio::select! {
            a = tokio::time::timeout(APPROVAL, self.host.approve(req.clone())) => a.unwrap_or(false),
            _ = conn.closed() => false,
        };
        if !allowed {
            let _ = send(tx, &Reply::Denied { reason: format!("Pairing was declined on {me}.") }).await;
            return;
        }
        let p = Peer { id: peer.to_string(), name: req.name.clone(), os: req.os.clone(), paired_at: now_ms(), last_sync: None, last_result: None, last_error: None, hints };
        if let Err(e) = self.store.add_peer(p) {
            let _ = send(tx, &Reply::Denied { reason: format!("{me} couldn't save the pairing: {e}") }).await;
            return;
        }
        self.audit(peer, &req.name, "paired", None, true);
        let _ = send(tx, &Reply::Paired { name: me, os: std::env::consts::OS.into() }).await;
        self.host.changed();
    }

    async fn serve_sync(&self, tx: &mut SendStream, rx: &mut RecvStream, peer: &str, name: &str, hints: Hints) {
        let Some(known) = self.store.peer(peer) else {
            let reason = format!("{} doesn't know this computer (it was unpaired there). Pair them again.", self.host.name());
            let _ = send(tx, &Reply::Denied { reason }).await;
            return;
        };
        let peer_name = clean_name(name);
        self.store.update_peer(peer, |p| {
            p.name = peer_name.clone();
            if hints.relay.is_some() || !hints.addrs.is_empty() {
                p.hints = hints;
            }
        });
        let (mine, places) = self.local_manifest();
        if send(tx, &Reply::Manifest { name: self.host.name(), hints: self.my_hints(), manifest: mine }).await.is_err() {
            return;
        }
        let mut report = Report::default();
        while let Ok(ask) = recv::<Ask>(rx, ANSWER).await {
            let reply = match ask {
                Ask::Get { files } => Reply::Files { files: self.read_files(&files, &places) },
                Ask::Put { changes } => {
                    let (applied, skipped) = self.take_puts(changes, &places, &mut report);
                    Reply::Done { applied, skipped }
                }
                Ask::Bye => break,
            };
            if send(tx, &reply).await.is_err() {
                break;
            }
        }
        self.store.update_peer(peer, |p| {
            p.last_sync = Some(now_ms());
            p.last_error = None;
            if report.moved() {
                p.last_result = Some(report.summary());
            }
        });
        if report.moved() {
            self.audit(peer, &known.name, "synced", Some(report.summary()), true);
            self.host.memories_changed();
        }
        self.host.changed();
    }

    /// The texts the other computer asked for (scrubbed; none when not here or too big).
    fn read_files(&self, keys: &[FileKey], places: &Places) -> Vec<File> {
        keys.iter()
            .take(wire::BATCH)
            .map(|k| {
                let text = places
                    .resolve(&k.place)
                    .filter(|_| plan::shared_path(&k.path))
                    .and_then(|(dir, _)| std::fs::read_to_string(self.cfg.memory.join(dir).join(&k.path)).ok())
                    .filter(|t| t.len() <= plan::MAX_FILE)
                    .map(|t| super::super::scrub(&t));
                File { place: k.place.clone(), path: k.path.clone(), text }
            })
            .collect()
    }

    /// Take the changes the other computer gives: (applied, skipped).
    fn take_puts(&self, changes: Vec<Put>, places: &Places, report: &mut Report) -> (usize, usize) {
        let (mut applied, mut skipped) = (0, 0);
        let mut idx = self.index.lock();
        for c in changes.into_iter().take(wire::BATCH) {
            let Some((dir, place)) = places.resolve(&c.item.place).filter(|_| plan::valid_item(&c.item)) else {
                skipped += 1;
                continue;
            };
            match plan::apply(&self.cfg.memory, &mut idx, dir, place, &c.item.path, c.expect.as_deref(), &c.item, c.text.as_deref()) {
                Ok(true) => {
                    applied += 1;
                    if c.item.hash.is_some() { report.received += 1 } else { report.deleted += 1 }
                }
                Ok(false) => skipped += 1,
                Err(e) => {
                    skipped += 1;
                    eprintln!("memory sync: {}/{}: {e}", c.item.place, c.item.path);
                }
            }
        }
        if applied > 0 {
            let _ = self.store.save_index(&idx);
        }
        (applied, skipped)
    }
}
