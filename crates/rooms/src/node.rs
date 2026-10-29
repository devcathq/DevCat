//! A DevCat's rooms on the network: its own iroh endpoint (separate from phone pairing), the room
//! protocol and gossip behind one [`Router`], and one [`Room`] per room it is in.
//!
//! How copies stay in step:
//! - **Live:** every local change is sealed with the room key and gossiped to the room's topic.
//!   Changes too big for gossip are announced (`Have`) and fetched directly.
//! - **Catch-up:** when a member comes into view (gossip neighbour, start-up, every few minutes)
//!   the two DevCats `Sync`: each sends exactly what the other lacks (Loro version vectors).
//! - **Members:** the owner signs every member list; anyone relays it, everyone checks it.
//!   Removing a member starts a new epoch with a new key and topic, which members fetch from
//!   someone who has it (a sync hands keys only to current members).
use crate::crypto::RoomKey;
use crate::doc::{Author, BlobRef, Message, RoomDoc};
use crate::model::{Invite, Member, Membership, Role, RoomSettings, Ticket, INVITE_TTL_MS};
use crate::store::{Hints, Prefs, RoomState, Store};
use crate::wire::{self, b64, unb64, Envelope, Gossip, Hello, Push, Reply};
use crate::now_ms;
use async_trait::async_trait;
use devcat_remote_proto::{read_frame, write_frame, ConnPath};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{presets, Connection};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use iroh_gossip::api::{Event as GossipEvent, GossipSender};
use iroh_gossip::{Gossip as GossipNet, TopicId};
use n0_future::StreamExt;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const CONNECT: Duration = Duration::from_secs(25);
const ANSWER: Duration = Duration::from_secs(30);
/// How long the owner has to answer a join request.
pub const APPROVAL: Duration = Duration::from_secs(5 * 60);
const PRESENCE_EVERY: Duration = Duration::from_secs(15);
/// Presence older than this: not here.
const PRESENCE_FRESH_MS: i64 = 45_000;
const SAVE_EVERY: Duration = Duration::from_secs(5);
const RESYNC_EVERY: Duration = Duration::from_secs(120);
const KEEPER_EVERY: Duration = Duration::from_secs(20);
/// Don't sync with the same member more often than this (gossip can announce them repeatedly).
const SYNC_GAP_MS: i64 = 5_000;
/// Largest shared binary file.
pub const MAX_BLOB: usize = 25 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayConfig {
    /// iroh's public relays (and its DNS lookup).
    Public,
    /// Your own relay.
    Custom(String),
    /// Direct connections only.
    Off,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dir: PathBuf,
    pub relay: RelayConfig,
    /// UDP port (0: any free one).
    pub port: u16,
    /// Run as a blind keeper: keep encrypted changes for rooms that list this DevCat as keeper.
    pub keeper: bool,
    /// A keeper only keeps rooms owned by these DevCats (empty: any).
    pub keeper_owners: Vec<String>,
}

/// Someone with an invite asks to join (the owner's DevCat asks its user).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub request_id: String,
    pub room: String,
    pub room_name: String,
    pub member: String,
    pub name: String,
    pub role: Role,
}

/// What changed in a room, for the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    Doc,
    Members,
    Presence,
    /// This DevCat was removed from the room.
    Removed,
}

/// What the app provides: answers join requests, hears about changes and new messages.
#[async_trait]
pub trait Host: Send + Sync + 'static {
    /// The owner's answer: the role to admit with, or `None` to refuse.
    async fn approve_join(&self, req: JoinRequest) -> Option<Role>;
    fn changed(&self, room: &str, what: Change);
    /// A thread message this DevCat hasn't seen before (written here or arrived).
    fn message(&self, room: &str, msg: &Message);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSummary {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub members: usize,
    pub online: usize,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub me: bool,
    pub online: bool,
    pub file: Option<String>,
    pub typing: bool,
    /// How the last direct connection with them travelled.
    pub path: Option<ConnPath>,
}

#[derive(Clone)]
struct Presence {
    file: Option<String>,
    typing: bool,
    at: i64,
}

pub struct Node {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    secret: SecretKey,
    endpoint: Endpoint,
    router: Mutex<Option<Router>>,
    gossip: GossipNet,
    lookup: MemoryLookup,
    host: Arc<dyn Host>,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    relay: RelayConfig,
    keeper: Option<crate::keeper::Keeper>,
    /// Join tokens being answered (one request per token at a time).
    pending_tokens: Mutex<HashSet<String>>,
    /// Rooms deleted because this DevCat was removed from them, not yet told to its user.
    gone: Mutex<Vec<Gone>>,
}

/// A room this DevCat was removed from, and so deleted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gone {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
struct RoomProto(Weak<Inner>);

impl ProtocolHandler for RoomProto {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if let Some(inner) = self.0.upgrade() {
            inner.serve(conn).await;
        }
        Ok(())
    }
}

impl Node {
    pub async fn start(cfg: Config, host: Arc<dyn Host>) -> Result<Node> {
        let store = Store::new(&cfg.dir).map_err(|e| format!("Couldn't open the rooms folder: {e}"))?;
        let secret = SecretKey::from_bytes(&store.secret_key().map_err(|e| e.to_string())?);
        let lookup = MemoryLookup::new();
        let builder = match &cfg.relay {
            RelayConfig::Public => Endpoint::builder(presets::N0),
            RelayConfig::Custom(url) => {
                let url: RelayUrl = url.parse().map_err(|_| format!("Not a relay address: {url}"))?;
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom([url]))
            }
            RelayConfig::Off => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
        };
        let endpoint = builder
            .secret_key(secret.clone())
            .address_lookup(lookup.clone())
            .bind_addr(format!("0.0.0.0:{}", cfg.port).as_str())
            .map_err(|e| e.to_string())?
            .bind()
            .await
            .map_err(|e| format!("Couldn't start rooms networking: {e}"))?;
        let gossip = GossipNet::builder().max_message_size(wire::MAX_GOSSIP + 1024).spawn(endpoint.clone());
        let keeper = cfg.keeper.then(|| crate::keeper::Keeper::new(cfg.dir.join("keep"), cfg.keeper_owners.clone(), endpoint.id().to_string()));
        let inner = Arc::new(Inner {
            store,
            secret,
            endpoint: endpoint.clone(),
            router: Mutex::new(None),
            gossip: gossip.clone(),
            lookup,
            host,
            rooms: Mutex::new(HashMap::new()),
            relay: cfg.relay.clone(),
            keeper,
            pending_tokens: Mutex::new(HashSet::new()),
            gone: Mutex::new(Vec::new()),
        });
        let router = Router::builder(endpoint).accept(wire::ALPN, RoomProto(Arc::downgrade(&inner))).accept(iroh_gossip::ALPN, gossip).spawn();
        *inner.router.lock() = Some(router);
        if !cfg.keeper {
            let me = inner.me();
            for id in inner.store.room_ids() {
                match inner.store.load(&id) {
                    // removed on a list the owner signed (while this DevCat kept rooms it was
                    // removed from): delete it now
                    Ok((state, _)) if state.membership.member(&me).is_none() && state.membership.verify(&state.id) => {
                        if inner.store.delete(&id).is_ok() {
                            inner.gone.lock().push(Gone { id: id.clone(), name: state.membership.settings.name.clone() });
                        }
                    }
                    Ok((state, doc)) => {
                        let doc = doc.and_then(|d| RoomDoc::load(&d).ok()).unwrap_or_default();
                        inner.open_room(state, doc);
                    }
                    Err(e) => tracing::warn!("room {id}: {e}"),
                }
            }
        }
        Ok(Node { inner })
    }

    /// This DevCat's id in rooms.
    pub fn id(&self) -> String {
        self.inner.endpoint.id().to_string()
    }

    /// How others can reach this DevCat right now.
    pub fn hints(&self) -> Hints {
        self.inner.my_hints()
    }

    /// Wait (up to `max`) until the relay is connected, so invites carry it.
    pub async fn online(&self, max: Duration) {
        if self.inner.relay != RelayConfig::Off {
            let _ = tokio::time::timeout(max, self.inner.endpoint.online()).await;
        }
    }

    pub async fn shutdown(&self) {
        let rooms: Vec<Arc<Room>> = self.inner.rooms.lock().drain().map(|(_, r)| r).collect();
        for r in rooms {
            r.stop();
            r.save();
        }
        let router = self.inner.router.lock().take();
        if let Some(r) = router {
            let _ = r.shutdown().await;
        }
    }

    /// Rooms deleted because this DevCat was removed from them, since this was last asked.
    pub fn take_gone(&self) -> Vec<Gone> {
        std::mem::take(&mut *self.inner.gone.lock())
    }

    pub fn rooms(&self) -> Vec<RoomSummary> {
        let mut v: Vec<RoomSummary> = self.inner.rooms.lock().values().map(|r| r.summary()).collect();
        v.sort_by_key(|a| a.name.to_lowercase());
        v
    }

    pub fn room(&self, id: &str) -> Result<Arc<Room>> {
        self.inner.rooms.lock().get(id).cloned().ok_or_else(|| "That room isn't on this DevCat.".to_string())
    }

    /// A new room with this DevCat as its owner.
    pub fn create(&self, name: &str, my_name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the room a name.".into());
        }
        let id = hex::encode(crate::crypto::random32());
        let me = self.id();
        let mut membership = Membership {
            room: id.clone(),
            owner: me.clone(),
            version: 1,
            epoch: 0,
            settings: RoomSettings { name: name.to_string(), agent_edits: false },
            members: vec![Member { id: me, name: clean_name(my_name), role: Role::Owner, added_at: now_ms() }],
            sig: String::new(),
        };
        membership.sign(&self.inner.secret);
        let state = RoomState {
            id: id.clone(),
            membership,
            keys: [(0, RoomKey::generate().to_hex())].into(),
            invites: vec![],
            prefs: Prefs::default(),
            hints: BTreeMap::new(),
            keeper_seen: BTreeMap::new(),
            removed: false,
        };
        self.inner.store.save_state(&state).map_err(|e| e.to_string())?;
        self.inner.open_room(state, RoomDoc::new());
        Ok(id)
    }

    /// An invite link for `role` (the owner's DevCat only). Single use, 24 hours.
    pub fn invite(&self, room: &str, role: Role) -> Result<String> {
        let r = self.room(room)?;
        if !r.is_owner() {
            return Err("Only the room's owner can invite.".into());
        }
        if matches!(role, Role::Owner | Role::Keeper) {
            return Err("Invite people as editors or viewers.".into());
        }
        let now = now_ms();
        let token = hex::encode(crate::crypto::random16());
        let name = {
            let mut st = r.state.lock();
            st.invites.retain(|i| i.usable(now));
            st.invites.push(Invite { token: token.clone(), role, created_at: now, expires_at: now + INVITE_TTL_MS, used: false });
            st.membership.settings.name.clone()
        };
        r.save_state();
        let h = self.inner.my_hints();
        let relay = match &self.inner.relay {
            RelayConfig::Public => Some(devcat_remote_proto::RELAY_PUBLIC.to_string()),
            RelayConfig::Custom(u) => Some(u.clone()),
            RelayConfig::Off => None,
        };
        Ok(Ticket { room: room.to_string(), token, name, host: self.id(), relay, home: h.relay, addrs: h.addrs }.to_url())
    }

    /// Ask to join with an invite link; waits for the owner's answer (up to [`APPROVAL`]).
    pub async fn join(&self, link: &str, my_name: &str) -> Result<String> {
        let t = Ticket::parse(link)?;
        if self.inner.rooms.lock().contains_key(&t.room) {
            return Err(format!("You're already in {}.", t.name));
        }
        if t.host == self.id() {
            return Err("That's your own room's invite.".into());
        }
        let hints = Hints { relay: t.home.clone().or(t.relay.clone().filter(|r| r != devcat_remote_proto::RELAY_PUBLIC)), addrs: t.addrs.clone() };
        self.inner.learn(&t.host, &hints);
        let conn = self.inner.dial(&t.host, Some(&hints)).await.map_err(|e| format!("Couldn't reach the room's owner: {e}. They need DevCat open."))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let hello = Hello::Join { room: t.room.clone(), token: t.token.clone(), name: clean_name(my_name), hints: self.inner.my_hints() };
        write_frame(&mut send, &hello).await.map_err(|e| e.to_string())?;
        let mut reply: Option<Reply> = tokio::time::timeout(ANSWER, read_frame(&mut recv)).await.map_err(|_| "The owner's DevCat didn't answer.".to_string())?.map_err(|e| e.to_string())?;
        if matches!(reply, Some(Reply::Waiting)) {
            reply = tokio::time::timeout(APPROVAL + Duration::from_secs(10), read_frame(&mut recv)).await.map_err(|_| "Nobody let you in in time. Ask for a new invite.".to_string())?.map_err(|e| e.to_string())?;
        }
        conn.close(0u32.into(), b"done");
        match reply {
            Some(Reply::Admitted { membership, keys, doc, hints }) => {
                let me = self.id();
                if !membership.verify(&t.room) || membership.owner != t.host || membership.member(&me).is_none() {
                    return Err("The answer from the owner's DevCat didn't check out.".into());
                }
                let doc = RoomDoc::load(&unb64(&doc).unwrap_or_default()).unwrap_or_default();
                for (id, h) in &hints {
                    self.inner.learn(id, h);
                }
                let state = RoomState { id: t.room.clone(), membership, keys: keys.into_iter().collect(), invites: vec![], prefs: Prefs::default(), hints, keeper_seen: BTreeMap::new(), removed: false };
                self.inner.store.save_state(&state).map_err(|e| e.to_string())?;
                self.inner.store.save_doc(&t.room, &doc.snapshot()).map_err(|e| e.to_string())?;
                self.inner.open_room(state, doc);
                Ok(t.room)
            }
            Some(Reply::Denied { reason, .. }) => Err(reason),
            _ => Err("The owner's DevCat closed the connection.".into()),
        }
    }

    /// Forget a room on this DevCat (the others keep it).
    pub fn forget(&self, room: &str) -> Result<()> {
        let r = self.inner.rooms.lock().remove(room);
        if let Some(r) = r {
            r.stop();
        }
        self.inner.store.delete(room).map_err(|e| e.to_string())
    }
}

impl Inner {
    fn me(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Every member's last known address, this DevCat's own included.
    fn all_hints(&self, st: &RoomState) -> BTreeMap<String, Hints> {
        let mut h = st.hints.clone();
        h.insert(self.me(), self.my_hints());
        h
    }

    fn my_hints(&self) -> Hints {
        let a = self.endpoint.addr();
        Hints { relay: a.relay_urls().next().map(|u| u.to_string()), addrs: a.ip_addrs().map(|s| s.to_string()).collect() }
    }

    /// Remember how to reach `id` (gossip and dials use it).
    fn learn(&self, id: &str, h: &Hints) {
        let Ok(eid) = id.parse::<EndpointId>() else { return };
        if eid == self.endpoint.id() {
            return;
        }
        let addr = addr_of(eid, h);
        if !addr.is_empty() {
            self.lookup.add_endpoint_info(addr);
        }
    }

    async fn dial(&self, peer: &str, hints: Option<&Hints>) -> Result<Connection> {
        let id: EndpointId = peer.parse().map_err(|_| "bad member id".to_string())?;
        let addr = hints.map(|h| addr_of(id, h)).unwrap_or_else(|| EndpointAddr::new(id));
        tokio::time::timeout(CONNECT, self.endpoint.connect(addr, wire::ALPN)).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())
    }

    fn open_room(self: &Arc<Self>, state: RoomState, doc: RoomDoc) -> Arc<Room> {
        for (id, h) in &state.hints {
            self.learn(id, h);
        }
        let room = Room::new(self, state, doc);
        self.rooms.lock().insert(room.id.clone(), room.clone());
        room.start();
        room
    }

    // ------------------------------------------------------------ incoming

    async fn serve(self: Arc<Self>, conn: Connection) {
        let peer = conn.remote_id().to_string();
        let Ok((mut send, mut recv)) = conn.accept_bi().await else { return };
        let hello: Hello = match tokio::time::timeout(ANSWER, read_frame(&mut recv)).await {
            Ok(Ok(Some(h))) => h,
            _ => return,
        };
        let path = crate::path::conn_path(&conn);
        match hello {
            Hello::Join { room, token, name, hints } => {
                let reply = self.answer_join(&peer, &room, &token, &name, &hints, &mut send).await;
                let _ = write_frame(&mut send, &reply).await;
            }
            Hello::Sync { room, membership, version, name: _, hints } => {
                let Some(r) = self.rooms.lock().get(&room).cloned() else {
                    let _ = write_frame(&mut send, &Reply::Denied { reason: "That room isn't here.".into(), removed: false, membership: None }).await;
                    return;
                };
                r.adopt(membership);
                let st = r.state.lock().clone();
                let Some(m) = st.membership.member(&peer).cloned() else {
                    // with the list the owner signed: the proof that lets their DevCat delete its copy
                    let _ = write_frame(&mut send, &Reply::Denied { reason: "You're not in this room any more.".into(), removed: true, membership: Some(st.membership.clone()) }).await;
                    return;
                };
                if !m.role.reads() {
                    return;
                }
                r.note_hints(&peer, &hints);
                if let Some(p) = path {
                    r.paths.lock().insert(peer.clone(), p);
                }
                let reply = Reply::Synced { membership: st.membership.clone(), keys: st.keys.clone().into_iter().collect(), updates: b64(&r.doc.updates_since(&unb64(&version).unwrap_or_default())), version: b64(&r.doc.version()), hints: self.all_hints(&st) };
                if write_frame(&mut send, &reply).await.is_err() {
                    return;
                }
                if let Ok(Ok(Some(p))) = tokio::time::timeout(ANSWER, read_frame::<_, Push>(&mut recv)).await && let Some(bytes) = unb64(&p.updates) {
                    r.import_remote(&bytes);
                }
            }
            Hello::Blob { room, hash } => {
                let r = self.rooms.lock().get(&room).cloned();
                let sealed = r.filter(|r| r.state.lock().membership.member(&peer).is_some_and(|m| m.role.reads())).and_then(|_| self.store.get_blob(&room, &hash));
                let _ = write_frame(&mut send, &Reply::Blob { sealed: sealed.map(|b| b64(&b)) }).await;
            }
            Hello::KeeperPush { room, membership, items } => {
                let reply = match &self.keeper {
                    Some(k) => k.push(&peer, &room, membership, items),
                    None => Reply::Denied { reason: "This DevCat isn't a keeper.".into(), removed: false, membership: None },
                };
                let _ = write_frame(&mut send, &reply).await;
            }
            Hello::KeeperFetch { room, membership, after } => {
                let reply = match &self.keeper {
                    Some(k) => k.fetch(&peer, &room, membership, after),
                    None => Reply::Denied { reason: "This DevCat isn't a keeper.".into(), removed: false, membership: None },
                };
                let _ = write_frame(&mut send, &reply).await;
            }
        }
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    }

    async fn answer_join(&self, peer: &str, room: &str, token: &str, name: &str, hints: &Hints, send: &mut iroh::endpoint::SendStream) -> Reply {
        let denied = |reason: &str| Reply::Denied { reason: reason.to_string(), removed: false, membership: None };
        let Some(r) = self.rooms.lock().get(room).cloned() else { return denied("That room isn't on the owner's DevCat any more.") };
        if !r.is_owner() {
            return denied("Only the room's owner can let people in.");
        }
        // Already in (a retry after a lost answer): admit again.
        if r.state.lock().membership.member(peer).is_none() {
            let now = now_ms();
            let Some(invite) = r.state.lock().invites.iter().find(|i| i.token == token && i.usable(now)).cloned() else {
                return denied("This invite was used already or has expired. Ask for a new one.");
            };
            if !self.pending_tokens.lock().insert(token.to_string()) {
                return denied("Someone is already using this invite.");
            }
            let _ = write_frame(send, &Reply::Waiting).await;
            let req = JoinRequest { request_id: hex::encode(crate::crypto::random16()), room: room.to_string(), room_name: r.name(), member: peer.to_string(), name: clean_name(name), role: invite.role };
            let answer = tokio::time::timeout(APPROVAL, self.host.approve_join(req)).await.ok().flatten();
            self.pending_tokens.lock().remove(token);
            {
                let mut st = r.state.lock();
                if let Some(i) = st.invites.iter_mut().find(|i| i.token == token) {
                    i.used = true;
                }
            }
            let Some(role) = answer.filter(|r| matches!(r, Role::Editor | Role::Viewer)) else {
                r.save_state();
                return denied("The owner didn't let you in.");
            };
            r.update_membership(|m| m.members.push(Member { id: peer.to_string(), name: clean_name(name), role, added_at: now_ms() }), false);
        }
        r.note_hints(peer, hints);
        let st = r.state.lock().clone();
        Reply::Admitted { hints: self.all_hints(&st), membership: st.membership, keys: st.keys.into_iter().collect(), doc: b64(&r.doc.snapshot()) }
    }
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

fn clean_name(n: &str) -> String {
    let n: String = n.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    if n.is_empty() { "Someone".into() } else { n }
}

struct GossipLink {
    epoch: u32,
    tx: GossipSender,
    task: JoinHandle<()>,
}

pub struct Room {
    pub id: String,
    idb: [u8; 32],
    node: Weak<Inner>,
    me: String,
    state: Mutex<RoomState>,
    doc: RoomDoc,
    gossip: Mutex<Option<GossipLink>>,
    presence: Mutex<HashMap<String, Presence>>,
    paths: Mutex<HashMap<String, ConnPath>>,
    last_sync: Mutex<HashMap<String, i64>>,
    /// Members with a postponed sync.
    sync_later: Mutex<HashSet<String>>,
    seen: Mutex<HashSet<String>>,
    focus: Mutex<(Option<String>, bool)>,
    dirty: AtomicBool,
    /// Removed and deleted (`wipe`): nothing is written for it any more.
    wiped: AtomicBool,
    keeper_pushed: Mutex<HashMap<String, Vec<u8>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    subs: Mutex<Vec<loro::Subscription>>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    out_rx: Mutex<Option<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

impl Room {
    fn new(node: &Arc<Inner>, state: RoomState, doc: RoomDoc) -> Arc<Room> {
        let idb: [u8; 32] = hex::decode(&state.id).ok().and_then(|b| b.try_into().ok()).unwrap_or_default();
        let (out, out_rx) = mpsc::unbounded_channel();
        let seen = doc.messages().into_iter().map(|m| m.id).collect();
        Arc::new(Room {
            id: state.id.clone(),
            idb,
            node: Arc::downgrade(node),
            me: node.me(),
            state: Mutex::new(state),
            doc,
            gossip: Mutex::new(None),
            presence: Mutex::new(HashMap::new()),
            paths: Mutex::new(HashMap::new()),
            last_sync: Mutex::new(HashMap::new()),
            sync_later: Mutex::new(HashSet::new()),
            seen: Mutex::new(seen),
            focus: Mutex::new((None, false)),
            dirty: AtomicBool::new(false),
            wiped: AtomicBool::new(false),
            keeper_pushed: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
            subs: Mutex::new(Vec::new()),
            out,
            out_rx: Mutex::new(Some(out_rx)),
        })
    }

    fn node(&self) -> Option<Arc<Inner>> {
        self.node.upgrade()
    }

    // ------------------------------------------------------------- reading

    pub fn doc(&self) -> &RoomDoc {
        &self.doc
    }

    pub fn name(&self) -> String {
        self.state.lock().membership.settings.name.clone()
    }

    pub fn membership(&self) -> Membership {
        self.state.lock().membership.clone()
    }

    pub fn prefs(&self) -> Prefs {
        self.state.lock().prefs.clone()
    }

    pub fn set_prefs(&self, p: Prefs) {
        self.state.lock().prefs = p;
        self.save_state();
    }

    pub fn is_owner(&self) -> bool {
        self.state.lock().membership.owner == self.me
    }

    pub fn removed(&self) -> bool {
        self.state.lock().removed
    }

    pub fn role(&self) -> Role {
        let st = self.state.lock();
        st.membership.member(&self.me).map(|m| m.role).unwrap_or(Role::Viewer)
    }

    /// This DevCat's user, or their agent.
    pub fn me(&self, agent: bool) -> Author {
        let st = self.state.lock();
        let name = st.membership.member(&self.me).map(|m| m.name.clone()).unwrap_or_else(|| "Me".into());
        Author { member: self.me.clone(), name, agent, via: None }
    }

    /// This DevCat user's coding agent acting from outside the room (`via`: Claude Code, Codex…).
    pub fn agent_via(&self, via: &str) -> Author {
        Author { via: Some(via.chars().take(40).collect()), ..self.me(true) }
    }

    pub fn summary(&self) -> RoomSummary {
        let st = self.state.lock();
        let now = now_ms();
        let online = self.presence.lock().iter().filter(|(id, p)| now - p.at < PRESENCE_FRESH_MS && st.membership.member(id).is_some()).count() + 1;
        RoomSummary {
            id: self.id.clone(),
            name: st.membership.settings.name.clone(),
            role: st.membership.member(&self.me).map(|m| m.role).unwrap_or(Role::Viewer),
            members: st.membership.members.iter().filter(|m| m.role != Role::Keeper).count(),
            online,
            removed: st.removed,
        }
    }

    pub fn members(&self) -> Vec<MemberView> {
        let st = self.state.lock();
        let (pres, paths, now) = (self.presence.lock(), self.paths.lock(), now_ms());
        let focus = self.focus.lock().clone();
        st.membership
            .members
            .iter()
            .map(|m| {
                let me = m.id == self.me;
                let p = pres.get(&m.id).filter(|p| now - p.at < PRESENCE_FRESH_MS);
                MemberView {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    role: m.role,
                    me,
                    online: me || p.is_some(),
                    file: if me { focus.0.clone() } else { p.and_then(|p| p.file.clone()) },
                    typing: if me { focus.1 } else { p.is_some_and(|p| p.typing) },
                    path: paths.get(&m.id).cloned(),
                }
            })
            .collect()
    }

    pub fn invites(&self) -> Vec<Invite> {
        let now = now_ms();
        self.state.lock().invites.iter().filter(|i| i.usable(now)).cloned().collect()
    }

    // ----------------------------------------------------------- lifecycle

    fn start(self: &Arc<Self>) {
        // Local changes → gossip.
        let tx = self.out.clone();
        let sub = self.doc.on_local_update(move |b| {
            let _ = tx.send(b.to_vec());
        });
        self.subs.lock().push(sub);
        let me = me_device_peer(&self.doc);
        self.doc.set_peer(me, &self.me);
        // files in folders from before root names were encoded (doc.rs `move_old_roots`)
        if self.is_owner() {
            self.doc.move_old_roots();
        }

        if let Some(mut rx) = self.out_rx.lock().take() {
            let w = Arc::downgrade(self);
            self.spawn(async move {
                while let Some(bytes) = rx.recv().await {
                    let Some(r) = w.upgrade() else { break };
                    r.dirty.store(true, Ordering::Relaxed);
                    r.broadcast_update(&bytes).await;
                    r.after_change();
                }
            });
        }
        let w = Arc::downgrade(self);
        self.spawn(async move {
            if let Some(r) = w.upgrade() {
                r.subscribe().await;
                r.sync_all().await;
                r.sync_keepers().await;
            }
            let mut tick = tokio::time::interval(SAVE_EVERY);
            let mut n: u64 = 0;
            loop {
                tick.tick().await;
                let Some(r) = w.upgrade() else { break };
                n += 1;
                r.save();
                if n.is_multiple_of(PRESENCE_EVERY.as_secs() / SAVE_EVERY.as_secs()) {
                    r.announce().await;
                    if r.is_owner() {
                        r.doc.sweep_claims(now_ms());
                    }
                }
                if n.is_multiple_of(KEEPER_EVERY.as_secs() / SAVE_EVERY.as_secs()) {
                    r.sync_keepers().await;
                }
                if n.is_multiple_of(RESYNC_EVERY.as_secs() / SAVE_EVERY.as_secs()) {
                    r.sync_all().await;
                }
            }
        });
    }

    fn spawn(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks.lock().push(tokio::spawn(f));
    }

    fn stop(&self) {
        for t in self.tasks.lock().drain(..) {
            t.abort();
        }
        if let Some(g) = self.gossip.lock().take() {
            g.task.abort();
        }
        self.subs.lock().clear();
    }

    pub fn save(&self) {
        if self.wiped.load(Ordering::Relaxed) {
            return;
        }
        if self.dirty.swap(false, Ordering::Relaxed) && let Some(n) = self.node() && let Err(e) = n.store.save_doc(&self.id, &self.doc.snapshot()) {
            tracing::warn!("saving room {}: {e}", self.id);
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn save_state(&self) {
        if self.wiped.load(Ordering::Relaxed) {
            return;
        }
        let st = self.state.lock().clone();
        if let Some(n) = self.node() {
            let _ = n.store.save_state(&st);
        }
    }

    /// This DevCat was removed, on a list the owner signed: delete its copy of the room (the
    /// document, shared files, keys and settings), stop its work, and say so. What the person
    /// already saved outside the room stays theirs. A DevCat that doesn't play along could keep
    /// it: this is what an honest one does.
    fn wipe(self: &Arc<Self>) {
        if self.wiped.swap(true, Ordering::Relaxed) {
            return;
        }
        self.stop();
        let Some(node) = self.node() else { return };
        node.rooms.lock().remove(&self.id);
        if let Err(e) = node.store.delete(&self.id) {
            tracing::warn!("deleting room {}: {e}", self.id);
        }
        node.gone.lock().push(Gone { id: self.id.clone(), name: self.name() });
        self.changed(Change::Removed);
    }

    fn changed(&self, what: Change) {
        if let Some(n) = self.node() {
            n.host.changed(&self.id, what);
        }
    }

    /// After any change to the document: tell the app, and report new thread messages.
    fn after_change(&self) {
        self.changed(Change::Doc);
        let fresh: Vec<Message> = {
            let mut seen = self.seen.lock();
            self.doc.messages().into_iter().filter(|m| seen.insert(m.id.clone())).collect()
        };
        if let Some(n) = self.node() {
            for m in &fresh {
                n.host.message(&self.id, m);
            }
        }
    }

    // -------------------------------------------------------------- gossip

    fn key(&self, epoch: u32) -> Option<RoomKey> {
        self.state.lock().keys.get(&epoch).and_then(|k| RoomKey::from_hex(k))
    }

    /// Join the gossip topic of the current epoch (boxed: it and [`Self::sync_with`] start each
    /// other).
    fn subscribe(self: &Arc<Self>) -> n0_future::boxed::BoxFuture<()> {
        let r = self.clone();
        Box::pin(async move { r.subscribe_now().await })
    }

    async fn subscribe_now(self: Arc<Self>) {
        let Some(node) = self.node() else { return };
        let (epoch, bootstrap) = {
            let st = self.state.lock();
            if st.removed {
                return;
            }
            let ids: Vec<EndpointId> = st.membership.members.iter().filter(|m| m.id != self.me && m.role.reads()).filter_map(|m| m.id.parse().ok()).collect();
            (st.membership.epoch, ids)
        };
        if self.gossip.lock().as_ref().is_some_and(|g| g.epoch == epoch) {
            return;
        }
        let Some(key) = self.key(epoch) else { return };
        let topic = TopicId::from_bytes(key.topic(&self.idb, epoch));
        let Ok(sub) = node.gossip.subscribe(topic, bootstrap).await else { return };
        let (tx, mut rx) = sub.split();
        let w = Arc::downgrade(&self);
        let task = tokio::spawn(async move {
            while let Some(ev) = rx.next().await {
                let Some(r) = w.upgrade() else { break };
                match ev {
                    Ok(GossipEvent::Received(m)) => r.on_gossip(&m.content).await,
                    Ok(GossipEvent::NeighborUp(id)) => {
                        let (r2, id) = (r.clone(), id.to_string());
                        tokio::spawn(async move { r2.sync_with(&id).await });
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let old = self.gossip.lock().replace(GossipLink { epoch, tx, task });
        if let Some(o) = old {
            o.task.abort();
        }
        self.announce().await;
    }

    async fn send_gossip(&self, msg: &Gossip) -> bool {
        let (epoch, tx) = match self.gossip.lock().as_ref() {
            Some(g) => (g.epoch, g.tx.clone()),
            None => return false,
        };
        let Some(key) = self.key(epoch) else { return false };
        let plain = serde_json::to_vec(msg).expect("serializes");
        let env = Envelope { e: epoch, s: b64(&key.seal(&self.idb, epoch, &plain)) };
        let bytes = serde_json::to_vec(&env).expect("serializes");
        if bytes.len() > wire::MAX_GOSSIP {
            return false;
        }
        tx.broadcast(bytes.into()).await.is_ok()
    }

    async fn broadcast_update(&self, update: &[u8]) {
        if !self.send_gossip(&Gossip::Update { from: self.me.clone(), bytes: b64(update) }).await {
            self.send_gossip(&Gossip::Have { from: self.me.clone() }).await;
        }
    }

    async fn on_gossip(self: &Arc<Self>, content: &[u8]) {
        let Ok(env) = serde_json::from_slice::<Envelope>(content) else { return };
        let Some(key) = self.key(env.e) else { return };
        let Some(plain) = unb64(&env.s).and_then(|s| key.open(&self.idb, env.e, &s)) else { return };
        let Ok(msg) = serde_json::from_slice::<Gossip>(&plain) else { return };
        let member = |id: &str| self.state.lock().membership.member(id).is_some();
        match msg {
            Gossip::Update { from, bytes } if member(&from) => {
                if let Some(b) = unb64(&bytes) {
                    self.import_remote(&b);
                }
            }
            Gossip::Have { from } if member(&from) => {
                let r = self.clone();
                tokio::spawn(async move { r.sync_with(&from).await });
            }
            Gossip::Presence { from, file, typing, hints, .. } if member(&from) && from != self.me => {
                self.note_hints(&from, &hints);
                self.presence.lock().insert(from, Presence { file, typing, at: now_ms() });
                self.changed(Change::Presence);
            }
            Gossip::Membership { membership } => {
                self.adopt(membership);
            }
            _ => {}
        }
    }

    /// Say who's here and what they look at.
    async fn announce(&self) {
        let Some(node) = self.node() else { return };
        let (file, typing) = self.focus.lock().clone();
        let name = self.me(false).name;
        self.send_gossip(&Gossip::Presence { from: self.me.clone(), name, file, typing, hints: node.my_hints() }).await;
    }

    /// What this DevCat's user is looking at (shown to the others).
    pub async fn set_focus(&self, file: Option<String>, typing: bool) {
        let changed = {
            let mut f = self.focus.lock();
            let changed = f.0 != file || f.1 != typing;
            *f = (file, typing);
            changed
        };
        if changed {
            self.announce().await;
        }
    }

    fn note_hints(&self, id: &str, h: &Hints) {
        if h.relay.is_none() && h.addrs.is_empty() {
            return;
        }
        let changed = {
            let mut st = self.state.lock();
            if st.hints.get(id) != Some(h) {
                st.hints.insert(id.to_string(), h.clone());
                true
            } else {
                false
            }
        };
        if changed {
            if let Some(n) = self.node() {
                n.learn(id, h);
            }
            self.save_state();
        }
    }

    // --------------------------------------------------------------- merge

    fn import_remote(&self, bytes: &[u8]) {
        if let Ok(true) = self.doc.import(bytes) {
            self.dirty.store(true, Ordering::Relaxed);
            self.after_change();
        }
    }

    /// Changes from this DevCat's editor (a copy of the document in the window): merge, and pass
    /// on to the others (they aren't "local" to the backend's copy, so gossip them here).
    pub async fn import_from_editor(&self, bytes: &[u8]) -> Result<()> {
        if !self.role().can_edit() {
            return Err("You can only read this room.".into());
        }
        if self.doc.import(bytes)? {
            self.dirty.store(true, Ordering::Relaxed);
            self.broadcast_update(bytes).await;
            self.after_change();
        }
        Ok(())
    }

    /// A newer member list (from anyone: it is checked against the owner's signature).
    fn adopt(self: &Arc<Self>, m: Membership) {
        let (removed, need_key) = {
            let mut st = self.state.lock();
            if !st.membership.superseded_by(&m) {
                return;
            }
            st.membership = m;
            let removed = st.membership.member(&self.me).is_none();
            st.removed = removed;
            (removed, !st.keys.contains_key(&st.membership.epoch))
        };
        if removed {
            self.wipe();
            return;
        }
        self.save_state();
        self.changed(Change::Members);
        let r = self.clone();
        tokio::spawn(async move {
            if need_key {
                // Someone who has the new key gives it to us (a sync hands keys to members).
                let (owner, others) = {
                    let st = r.state.lock();
                    let others: Vec<String> = st.membership.members.iter().filter(|m| m.id != st.membership.owner && m.id != r.me && m.role.reads()).map(|m| m.id.clone()).collect();
                    (st.membership.owner.clone(), others)
                };
                for p in std::iter::once(owner).chain(others) {
                    let has_key = {
                        let st = r.state.lock();
                        st.keys.contains_key(&st.membership.epoch)
                    };
                    if has_key {
                        break;
                    }
                    r.sync_forced(&p).await;
                }
            }
            r.subscribe().await;
        });
    }

    /// Change the member list (owner only): sign, keep, and tell everyone.
    fn update_membership(self: &Arc<Self>, f: impl FnOnce(&mut Membership), rekey: bool) -> Option<Membership> {
        let node = self.node()?;
        let (m, old_epoch) = {
            let mut st = self.state.lock();
            if st.membership.owner != self.me {
                return None;
            }
            let old_epoch = st.membership.epoch;
            let mut m = st.membership.clone();
            f(&mut m);
            m.version += 1;
            if rekey {
                m.epoch += 1;
                st.keys.insert(m.epoch, RoomKey::generate().to_hex());
            }
            m.sign(&node.secret);
            st.membership = m.clone();
            (m, old_epoch)
        };
        self.save_state();
        self.changed(Change::Members);
        let r = self.clone();
        let msg = Gossip::Membership { membership: m.clone() };
        tokio::spawn(async move {
            // On the current topic, so everyone (a removed member too) learns of it…
            r.send_gossip(&msg).await;
            if rekey && old_epoch != r.state.lock().membership.epoch {
                // …then move to the new key's topic.
                r.subscribe().await;
                r.send_gossip(&msg).await;
            }
        });
        Some(m)
    }

    // ---------------------------------------------------------------- sync

    async fn sync_all(self: &Arc<Self>) {
        let peers: Vec<String> = {
            let st = self.state.lock();
            st.membership.members.iter().filter(|m| m.id != self.me && m.role.reads()).map(|m| m.id.clone()).collect()
        };
        let jobs = peers.into_iter().map(|p| {
            let r = self.clone();
            async move { r.sync_with(&p).await }
        });
        n0_future::join_all(jobs).await;
    }

    /// Catch up with one member, both ways.
    pub fn sync_with(self: &Arc<Self>, peer: &str) -> n0_future::boxed::BoxFuture<()> {
        let (r, peer) = (self.clone(), peer.to_string());
        Box::pin(async move { r.sync_now(&peer, false).await })
    }

    /// [`Self::sync_with`] even if we synced with them a moment ago.
    fn sync_forced(self: &Arc<Self>, peer: &str) -> n0_future::boxed::BoxFuture<()> {
        let (r, peer) = (self.clone(), peer.to_string());
        Box::pin(async move { r.sync_now(&peer, true).await })
    }

    /// `force`: even if we synced with them a moment ago (we need a key they have).
    async fn sync_now(self: Arc<Self>, peer: &str, force: bool) {
        if peer == self.me || self.removed() {
            return;
        }
        {
            let mut last = self.last_sync.lock();
            let now = now_ms();
            match last.get(peer).copied() {
                // Synced a moment ago: postpone (once) to the end of the gap, never drop it, so
                // whatever happened since is still caught up.
                Some(t) if !force && now - t < SYNC_GAP_MS => {
                    if self.sync_later.lock().insert(peer.to_string()) {
                        let (r, p, wait) = (self.clone(), peer.to_string(), (SYNC_GAP_MS - (now - t)) as u64);
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(wait)).await;
                            r.sync_later.lock().remove(&p);
                            r.sync_forced(&p).await;
                        });
                    }
                    return;
                }
                _ => {
                    last.insert(peer.to_string(), now);
                }
            }
        }
        let Some(node) = self.node() else { return };
        let hints = self.state.lock().hints.get(peer).cloned();
        let Ok(conn) = node.dial(peer, hints.as_ref()).await else { return };
        self.paths.lock().insert(peer.to_string(), crate::path::conn_path(&conn).unwrap_or(ConnPath { direct: false, rtt_ms: 0, relay: None }));
        let Ok((mut send, mut recv)) = conn.open_bi().await else { return };
        let hello = Hello::Sync { room: self.id.clone(), membership: self.membership(), version: b64(&self.doc.version()), name: self.me(false).name, hints: node.my_hints() };
        if write_frame(&mut send, &hello).await.is_err() {
            return;
        }
        match tokio::time::timeout(ANSWER, read_frame::<_, Reply>(&mut recv)).await {
            Ok(Ok(Some(Reply::Synced { membership, keys, updates, version, hints }))) => {
                let new_epoch = {
                    let mut st = self.state.lock();
                    let before = st.keys.len();
                    for (e, k) in keys {
                        st.keys.entry(e).or_insert(k);
                    }
                    st.keys.len() != before
                };
                if new_epoch {
                    self.save_state();
                }
                self.adopt(membership);
                for (id, h) in hints.iter().filter(|(id, _)| **id != self.me) {
                    self.note_hints(id, h);
                }
                if let Some(b) = unb64(&updates) {
                    self.import_remote(&b);
                }
                // Only to someone who still reads the room: the owner also syncs with a member it
                // just removed (so they learn it), and must not send what was written since.
                let reads = self.state.lock().membership.member(peer).is_some_and(|m| m.role.reads());
                let mine = if reads { self.doc.updates_since(&unb64(&version).unwrap_or_default()) } else { Vec::new() };
                let _ = write_frame(&mut send, &Push { updates: b64(&mine) }).await;
                let _ = send.finish();
                if new_epoch {
                    self.subscribe().await;
                }
            }
            // with the owner's signed list, the proof (see `adopt`)
            Ok(Ok(Some(Reply::Denied { removed: true, membership: Some(m), .. }))) => self.adopt(m),
            // an older DevCat says so without proof: read-only, nothing deleted on its word
            Ok(Ok(Some(Reply::Denied { removed: true, membership: None, .. }))) => {
                self.state.lock().removed = true;
                self.save_state();
                self.changed(Change::Removed);
            }
            _ => {}
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    }

    // ---------------------------------------------------------------- owner

    /// Remove a member (owner only). Starts a new epoch: they can't read anything new.
    pub fn remove_member(self: &Arc<Self>, member: &str) -> Result<()> {
        if !self.is_owner() {
            return Err("Only the owner can remove members.".into());
        }
        if member == self.me {
            return Err("You own this room; you can't remove yourself.".into());
        }
        let reads = self.state.lock().membership.member(member).map(|m| m.role.reads()).ok_or("Not a member.")?;
        let everyone: Vec<String> = self.state.lock().membership.members.iter().filter(|m| m.id != self.me && m.role.reads()).map(|m| m.id.clone()).collect();
        self.update_membership(|m| m.members.retain(|x| x.id != member), reads).ok_or("Couldn't update the room.")?;
        self.presence.lock().remove(member);
        // Tell each of them directly (gossip may not reach someone who just arrived): the sync
        // carries the signed list, so the removed member learns it at once and the others
        // come for the new key.
        for p in everyone {
            let r = self.clone();
            tokio::spawn(async move { r.sync_forced(&p).await });
        }
        Ok(())
    }

    pub fn set_role(self: &Arc<Self>, member: &str, role: Role) -> Result<()> {
        if !self.is_owner() || member == self.me || matches!(role, Role::Owner | Role::Keeper) {
            return Err("Only the owner changes roles, between editor and viewer.".into());
        }
        self.state.lock().membership.member(member).ok_or("Not a member.")?;
        self.update_membership(
            |m| {
                if let Some(x) = m.members.iter_mut().find(|x| x.id == member) {
                    x.role = role;
                }
            },
            false,
        )
        .ok_or("Couldn't update the room.")?;
        Ok(())
    }

    pub fn set_settings(self: &Arc<Self>, name: Option<String>, agent_edits: Option<bool>) -> Result<()> {
        if !self.is_owner() {
            return Err("Only the owner changes the room's settings.".into());
        }
        self.update_membership(
            |m| {
                if let Some(n) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    m.settings.name = n.chars().take(80).collect();
                }
                if let Some(a) = agent_edits {
                    m.settings.agent_edits = a;
                }
            },
            false,
        )
        .ok_or("Couldn't update the room.")?;
        Ok(())
    }

    /// Add a blind keeper (owner only): it stores the room's encrypted changes for whoever is
    /// offline, and can't read them.
    /// `hints`: how to reach it, if iroh's DNS lookup can't (own relay, or none).
    pub fn add_keeper(self: &Arc<Self>, keeper: &str, name: &str, hints: Option<Hints>) -> Result<()> {
        if !self.is_owner() {
            return Err("Only the owner can add a keeper.".into());
        }
        keeper.parse::<EndpointId>().map_err(|_| "That isn't a DevCat keeper id (64 hex characters).".to_string())?;
        if self.state.lock().membership.member(keeper).is_some() {
            return Err("Already in the room.".into());
        }
        if let Some(h) = hints {
            self.note_hints(keeper, &h);
        }
        self.update_membership(|m| m.members.push(Member { id: keeper.to_string(), name: clean_name(name), role: Role::Keeper, added_at: now_ms() }), false).ok_or("Couldn't update the room.")?;
        Ok(())
    }

    // ---------------------------------------------------------------- blobs

    /// Share a binary file: sealed with the current key, kept here, named in the document.
    pub fn add_blob(&self, path: &str, bytes: &[u8], mime: &str, by: &Author) -> Result<()> {
        if bytes.len() > MAX_BLOB {
            return Err("The file is too large to share (25 MB at most).".into());
        }
        let node = self.node().ok_or("closed")?;
        let epoch = self.state.lock().membership.epoch;
        let key = self.key(epoch).ok_or("No room key.")?;
        // epoch (4 bytes) || sealed, so any later key holder can open it.
        let mut stored = epoch.to_be_bytes().to_vec();
        stored.extend(key.seal(&self.idb, epoch, bytes));
        let hash = blake3::hash(&stored).to_hex().to_string();
        node.store.put_blob(&self.id, &hash, &stored).map_err(|e| e.to_string())?;
        self.doc.add_blob(path, BlobRef { hash, size: bytes.len() as u64, mime: mime.to_string() }, by, now_ms())
    }

    /// A shared binary file's content: from here, or from the first member who has it.
    pub async fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        let node = self.node().ok_or("closed")?;
        let stored = match node.store.get_blob(&self.id, hash) {
            Some(b) => b,
            None => {
                let peers: Vec<(String, Option<Hints>)> = {
                    let st = self.state.lock();
                    st.membership.members.iter().filter(|m| m.id != self.me && m.role.reads()).map(|m| (m.id.clone(), st.hints.get(&m.id).cloned())).collect()
                };
                let mut found = None;
                for (p, h) in peers {
                    let Ok(conn) = node.dial(&p, h.as_ref()).await else { continue };
                    let Ok((mut send, mut recv)) = conn.open_bi().await else { continue };
                    if write_frame(&mut send, &Hello::Blob { room: self.id.clone(), hash: hash.to_string() }).await.is_err() {
                        continue;
                    }
                    if let Ok(Ok(Some(Reply::Blob { sealed: Some(s) }))) = tokio::time::timeout(Duration::from_secs(120), read_frame(&mut recv)).await
                        && let Some(b) = unb64(&s).filter(|b| blake3::hash(b).to_hex().as_str() == hash)
                    {
                        let _ = node.store.put_blob(&self.id, hash, &b);
                        found = Some(b);
                        break;
                    }
                }
                found.ok_or("Nobody who has this file is online right now.")?
            }
        };
        if stored.len() < 4 {
            return Err("The file is damaged.".into());
        }
        let epoch = u32::from_be_bytes(stored[..4].try_into().unwrap());
        let key = self.key(epoch).ok_or("This file was shared with a key you don't have.")?;
        key.open(&self.idb, epoch, &stored[4..]).ok_or_else(|| "The file is damaged.".to_string())
    }

    // --------------------------------------------------------------- keeper

    /// Push what each keeper lacks, and fetch what it has that we lack (also every few seconds).
    pub async fn sync_keepers(self: &Arc<Self>) {
        let Some(node) = self.node() else { return };
        let (keepers, membership, epoch) = {
            let st = self.state.lock();
            if st.removed {
                return;
            }
            (st.membership.members.iter().filter(|m| m.role == Role::Keeper).map(|m| m.id.clone()).collect::<Vec<_>>(), st.membership.clone(), st.membership.epoch)
        };
        let Some(key) = self.key(epoch) else { return };
        for k in keepers {
            let hints = self.state.lock().hints.get(&k).cloned();
            // fetch
            let after = self.state.lock().keeper_seen.get(&k).copied().unwrap_or(0);
            let fetched = keeper_call(&node, &k, hints.as_ref(), Hello::KeeperFetch { room: self.id.clone(), membership: membership.clone(), after }).await;
            // the keeper knows a newer list without us: it's the proof (see `adopt`)
            if let Some(Reply::Denied { removed: true, membership: Some(m), .. }) = fetched {
                self.adopt(m);
                return;
            }
            if let Some(Reply::Items { items, last }) = fetched {
                for (_, item) in items {
                    let Ok(env) = serde_json::from_slice::<Envelope>(&unb64(&item).unwrap_or_default()) else { continue };
                    let Some(k2) = self.key(env.e) else { continue };
                    if let Some(bytes) = unb64(&env.s).and_then(|s| k2.open(&self.idb, env.e, &s)) {
                        self.import_remote(&bytes);
                    }
                }
                self.state.lock().keeper_seen.insert(k.clone(), last);
                self.save_state();
            }
            // push
            let pushed = self.keeper_pushed.lock().get(&k).cloned().unwrap_or_default();
            let now_v = self.doc.version();
            if pushed == now_v {
                continue;
            }
            let updates = self.doc.updates_since(&pushed);
            let env = Envelope { e: epoch, s: b64(&key.seal(&self.idb, epoch, &updates)) };
            let item = b64(&serde_json::to_vec(&env).expect("serializes"));
            if let Some(Reply::Kept { .. }) = keeper_call(&node, &k, hints.as_ref(), Hello::KeeperPush { room: self.id.clone(), membership: membership.clone(), items: vec![item] }).await {
                self.keeper_pushed.lock().insert(k.clone(), now_v);
            }
        }
    }
}

async fn keeper_call(node: &Inner, keeper: &str, hints: Option<&Hints>, hello: Hello) -> Option<Reply> {
    let conn = node.dial(keeper, hints).await.ok()?;
    let (mut send, mut recv) = conn.open_bi().await.ok()?;
    write_frame(&mut send, &hello).await.ok()?;
    let r = tokio::time::timeout(Duration::from_secs(60), read_frame(&mut recv)).await.ok()?.ok()?;
    conn.close(0u32.into(), b"done");
    r
}

/// The Loro peer this DevCat's backend copy edits as.
fn me_device_peer(doc: &RoomDoc) -> u64 {
    doc.peer()
}
