//! What DevCats say to each other about rooms.
//!
//! **Direct** (`devcat/room/1`, one request per connection, JSON frames as in
//! `devcat-remote-proto`): the caller sends a [`Hello`], the callee answers with [`Reply`]s.
//! - `Join`: an invite token, answered `Waiting` then `Admitted` (or `Denied`) by the owner.
//! - `Sync`: catch up both ways; the callee sends what the caller lacks, the caller then sends
//!   [`Push`] with what the callee lacks. Also hands out room keys to members that miss one.
//! - `Blob`: a shared binary file, still encrypted with the room key.
//! - `Keeper*`: store and fetch encrypted changes on a blind keeper.
//!
//! **Gossip** (iroh-gossip, topic derived from the room key): [`Envelope`]s, each a [`Gossip`]
//! message sealed with the room key of its epoch.
use crate::model::Membership;
use crate::store::Hints;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ALPN: &[u8] = b"devcat/room/1";

/// Largest gossip message; bigger changes are announced (`Have`) and fetched directly.
pub const MAX_GOSSIP: usize = 32 * 1024;

pub use devcat_remote_proto::web::{b64url as b64, b64url_decode as unb64};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Hello {
    Join { room: String, token: String, name: String, hints: Hints },
    Sync { room: String, membership: Membership, version: String, name: String, hints: Hints },
    Blob { room: String, hash: String },
    KeeperPush { room: String, membership: Membership, items: Vec<String> },
    KeeperFetch { room: String, membership: Membership, after: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Reply {
    /// Join: the owner is being asked.
    Waiting,
    /// `keys`: (epoch, key hex); a list because JSON object keys are strings.
    Admitted { membership: Membership, keys: Vec<(u32, String)>, doc: String, hints: BTreeMap<String, Hints> },
    Denied { reason: String, #[serde(default)] removed: bool },
    Synced { membership: Membership, keys: Vec<(u32, String)>, updates: String, version: String, hints: BTreeMap<String, Hints> },
    Blob { sealed: Option<String> },
    /// Keeper: stored, up to this index.
    Kept { last: u64 },
    /// Keeper: items after the asked index (sealed, as pushed), and the last index.
    Items { items: Vec<(u64, String)>, last: u64 },
}

/// The caller's half of a sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    pub updates: String,
}

/// On the gossip topic: `epoch` names the key that sealed `sealed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub e: u32,
    pub s: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Gossip {
    /// Document changes (Loro update bytes, base64url).
    Update { from: String, bytes: String },
    /// Changes too big for gossip: sync with `from` to get them.
    Have { from: String },
    /// Who's here and what they look at (never stored).
    Presence { from: String, name: String, file: Option<String>, typing: bool, hints: Hints },
    /// A newer member list (the owner's signature makes it trustworthy whoever relays it).
    Membership { membership: Membership },
}
