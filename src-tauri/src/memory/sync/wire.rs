//! What two computers say to each other to share memory (`devcat/memory-sync/1`): one
//! bidirectional stream per connection, JSON frames as in `devcat-remote-proto`.
//!
//! - **Pair:** the caller sends [`Hello::Pair`] with the one-time code from the other computer's
//!   ticket; the callee answers `Waiting` while its user decides, then `Paired` or `Denied`.
//! - **Sync:** the caller sends [`Hello::Sync`]; the callee answers with its manifest (`Manifest`),
//!   or `Denied` when it doesn't know the caller. The caller, which has its own, decides what moves
//!   (`plan::decide`) and sends [`Ask`]s: `Get` (files it takes, answered `Files`), `Put` (files the
//!   callee takes, answered `Done`), then `Bye`.
use super::plan::Item;
use serde::{Deserialize, Serialize};

pub const ALPN: &[u8] = b"devcat/memory-sync/1";

/// The ticket one computer shows and the other pastes: a pairing link (`PairLink`) with this scheme.
pub const SCHEME: &str = "devcat://memory-sync";

/// Files per `Get` / `Put` (each is at most `plan::MAX_FILE`).
pub const BATCH: usize = 100;

/// How to reach a computer: its relay server and direct addresses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Hello {
    Pair { code: String, name: String, os: String, hints: Hints },
    Sync { name: String, hints: Hints },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Reply {
    /// Pair: the other computer's user is being asked.
    Waiting,
    Paired { name: String, os: String },
    Denied { reason: String },
    Manifest { name: String, hints: Hints, manifest: Vec<Item> },
    Files { files: Vec<File> },
    /// Put: how many changes were taken, and how many weren't (changed meanwhile, no such place…).
    Done { applied: usize, skipped: usize },
}

/// The caller's requests after the manifests are exchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Ask {
    Get { files: Vec<FileKey> },
    Put { changes: Vec<Put> },
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileKey {
    pub place: String,
    pub path: String,
}

/// A file's text (`None`: it isn't there any more).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct File {
    pub place: String,
    pub path: String,
    pub text: Option<String>,
}

/// Take this version of a file (`item.hash` none: delete it), but only if the file is still the
/// version the callee's manifest showed (`expect`); otherwise it changed meanwhile and the next
/// sync sorts it out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Put {
    pub item: Item,
    pub expect: Option<String>,
    pub text: Option<String>,
}
