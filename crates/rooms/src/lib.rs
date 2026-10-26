//! DevCat rooms: people on their own networks share documents, discuss them in a thread, and each
//! person's DevCat Agent can join in, peer to peer over iroh. See `docs/rooms-architecture.md`.
//!
//! - [`model`]: members, roles, the owner-signed member list, invites and invite links.
//! - [`crypto`]: the room key (encrypts all room traffic) and the gossip topic derived from it.
//! - [`doc`]: the shared document (Loro CRDT): files, thread, claims, suggestions, history.
//! - [`node`]: the network side: the rooms endpoint, joining, sync, gossip, members, blobs.
//! - [`keeper`]: the blind keeper (stores encrypted changes for offline members).
//! - [`store`], [`wire`]: files on disk, and what DevCats say to each other.
pub mod crypto;
pub mod doc;
pub mod keeper;
pub mod model;
pub mod node;
pub mod path;
pub mod store;
pub mod wire;

pub use crypto::RoomKey;
pub use doc::{Author, RoomDoc};
pub use model::{Invite, Member, Membership, Role, RoomSettings, Ticket};
pub use node::{Change, Config, Host, JoinRequest, MemberView, Node, RelayConfig, Room, RoomSummary};
pub use store::{Mentions, Prefs};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
