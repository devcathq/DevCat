//! The room's shared document: one Loro CRDT holding the files, the discussion thread, claims,
//! suggestions and the history of applied changes. Every member keeps a full copy; copies merge
//! without losing anything (see docs/rooms-architecture.md §5).
//!
//! Layout (records are JSON strings, so every copy can read them without a schema):
//! - `files` (map): path → [`FileMeta`]; each text file's content is the root text `file:<path>`,
//!   so two people creating the same file at once share one text instead of overwriting each other.
//!   A whiteboard's shapes are the root map `board:<path>` (shape id → its JSON, Excalidraw's).
//!   Folders are only the `/` in paths.
//! - `thread` (list): [`Message`]s.
//! - `claims` (map): id → [`ClaimRecord`]: an agent's lease on a range it is about to edit.
//! - `suggestions` (map): id → [`SuggestionRecord`]: a change waiting for a person.
//! - `history` (list): [`ChangeRecord`]s of applied suggestions, agent edits and reverts.
//! - `peers` (map): Loro peer id → member id, to attribute typing.
//! - `tasks` (map): id → [`Task`]: work for a person or an agent.
//! - `pins` (map): message id → [`Pin`]: the thread's decisions.
//! - `reactions` (map): `message|member|emoji` → [`Reaction`].
//!
//! Ranges are kept as Loro cursors on their first and last character, so they follow the text as
//! others type, and text typed just outside a range stays outside it. A change is applied only if
//! its range still holds exactly what its author saw; otherwise it is *out of date*.
use loro::cursor::{Cursor, Side};
use loro::{ExportMode, LoroDoc, LoroMap, LoroText, LoroValue, VersionVector};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const FILES: &str = "files";
const THREAD: &str = "thread";
const CLAIMS: &str = "claims";
const SUGGESTIONS: &str = "suggestions";
const HISTORY: &str = "history";
const PEERS: &str = "peers";
const TASKS: &str = "tasks";
const PINS: &str = "pins";
const REACTIONS: &str = "reactions";

/// Reactions people can put on a message.
pub const REACTION_EMOJI: &[&str] = &["👍", "❤️", "🎉", "😄", "🤔", "👀", "✅"];
/// Longest task title, in characters.
pub const MAX_TASK_TITLE: usize = 300;
/// Most shapes on one whiteboard.
pub const MAX_BOARD_SHAPES: usize = 5_000;

/// How long a claim lasts unless renewed.
pub const CLAIM_TTL_MS: i64 = 5 * 60 * 1000;
/// Largest text file, in characters. Loro merges concurrent edits around a text in time that grows
/// with the square of its length (0.3 s at 200,000 characters in the window, 40 s at 2 million),
/// so larger files are shared as they are instead (`rooms.rs`: `room_file_add`).
pub const MAX_FILE_CHARS: usize = 200_000;
/// Longest thread message, in characters.
pub const MAX_MESSAGE_CHARS: usize = 20_000;

/// A path inside a root container's name. Loro refuses "/" in root names when it imports changes
/// (a panic, in the window and here), so "/" is written `%2F` and "%" `%25`; a name with neither
/// (every file before folders) stays as it was. The window does the same (replica.ts `rootPart`).
fn root_part(path: &str) -> String {
    path.replace('%', "%25").replace('/', "%2F")
}

fn text_root(path: &str) -> String {
    format!("file:{}", root_part(path))
}

fn board_root(path: &str) -> String {
    format!("board:{}", root_part(path))
}

fn new_id() -> String {
    hex::encode(&crate::crypto::random16()[..8])
}

/// Who did something: a person, or a person's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Author {
    pub member: String,
    pub name: String,
    #[serde(default)]
    pub agent: bool,
    /// The person's coding agent that acted from outside the room (Claude Code, Codex, a DevCat
    /// Agent chat); none for the person, and for their agent in the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl Author {
    pub fn label(&self) -> String {
        match (self.agent, &self.via) {
            (true, Some(v)) => format!("{}'s agent ({v})", self.name),
            (true, None) => format!("{}'s agent", self.name),
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text,
    /// A binary file (image, PDF, …) shared as a blob; the record names it, members fetch it.
    Blob,
    /// A whiteboard: its shapes are the root map `board:<path>`. DevCats from before boards don't
    /// read this kind, so they don't list the file.
    Board,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobRef {
    /// BLAKE3 hash of the *encrypted* content (hex).
    pub hash: String,
    /// Size of the original file.
    pub size: u64,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub kind: FileKind,
    pub created_by: Author,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    #[serde(flatten)]
    pub meta: FileMeta,
    /// Characters, for text files; shapes, for whiteboards.
    pub chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub author: Author,
    pub text: String,
    pub at: i64,
    /// 0 for people; an agent's reply is one more than what it answers (agents stop at a limit,
    /// so they can't keep answering each other).
    #[serde(default)]
    pub depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRecord {
    pub id: String,
    pub path: String,
    pub start: String,
    pub last: String,
    pub holder: Author,
    pub at: i64,
    pub expires: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A claim as it stands now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimView {
    pub id: String,
    pub path: String,
    pub from: usize,
    pub to: usize,
    pub text: String,
    pub holder: Author,
    pub at: i64,
    pub expires: i64,
    /// Two claims overlapping (made while out of sync): the earlier one wins.
    pub winning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionState {
    Open,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionRecord {
    pub id: String,
    pub path: String,
    pub author: Author,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The text the author saw and wants replaced ("" with no range: a new file).
    pub base: String,
    pub replacement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    pub state: SuggestionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    /// Why it was rejected or couldn't be applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionView {
    #[serde(flatten)]
    pub record: SuggestionRecord,
    /// Where the range is now (open suggestions with a range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<usize>,
    /// The range no longer holds `base` (someone changed it since).
    pub outdated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Suggestion,
    Edit,
    Revert,
    NewFile,
}

/// An applied change, kept so it can be reverted later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub id: String,
    pub kind: ChangeKind,
    pub path: String,
    pub by: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Author>,
    pub at: i64,
    pub before: String,
    pub after: String,
    /// The changed range afterwards (`last` is none when `after` is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Open,
    Doing,
    Done,
}

/// Who a task is for: a member, or that member's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignee {
    pub member: String,
    #[serde(default)]
    pub agent: bool,
}

/// Work for someone in the room. The record is replaced whole on each change (the latest change
/// wins when two are made at once).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// None: anyone may take it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<Assignee>,
    pub state: TaskState,
    pub created_by: Author,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Author>,
    #[serde(default)]
    pub updated_at: i64,
}

/// A change to a task; fields left out stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    /// Some(None) clears the notes.
    #[serde(default, with = "double_option")]
    pub notes: Option<Option<String>>,
    /// Some(None): for anyone.
    #[serde(default, with = "double_option")]
    pub assignee: Option<Option<Assignee>>,
    #[serde(default)]
    pub state: Option<TaskState>,
}

/// `null` and a missing field differ: `null` clears, missing keeps.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(inner) => inner.serialize(s),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// A thread message pinned as a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub message: String,
    pub by: Author,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub message: String,
    pub emoji: String,
    pub by: Author,
    pub at: i64,
}

pub type DocResult<T> = Result<T, String>;

pub struct RoomDoc {
    doc: LoroDoc,
}

impl RoomDoc {
    /// A new, empty document edited as `peer` (a fresh random id each run: Loro needs every
    /// concurrently editing copy to have its own).
    pub fn new() -> RoomDoc {
        let doc = LoroDoc::new();
        doc.set_record_timestamp(true);
        RoomDoc { doc }
    }

    pub fn load(snapshot: &[u8]) -> DocResult<RoomDoc> {
        let d = RoomDoc::new();
        d.doc.import(snapshot).map_err(|e| format!("The room's document is damaged: {e}"))?;
        Ok(d)
    }

    pub fn peer(&self) -> u64 {
        self.doc.peer_id()
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.doc.export(ExportMode::Snapshot).unwrap_or_default()
    }

    pub fn version(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }

    /// Everything the holder of `version` lacks (all of it for an empty or unreadable version).
    pub fn updates_since(&self, version: &[u8]) -> Vec<u8> {
        let vv = if version.is_empty() { VersionVector::default() } else { VersionVector::decode(version).unwrap_or_default() };
        self.doc.export(ExportMode::updates(&vv)).unwrap_or_default()
    }

    /// What a copy at `version` lacks, as [`Self::updates_since`], but a snapshot for an empty copy:
    /// Loro imports a whole history in time that grows with the square of its texts' lengths once
    /// edits were concurrent (a minute for a 2 MB file), a snapshot in milliseconds.
    pub fn catch_up(&self, version: &[u8]) -> Vec<u8> {
        let empty = version.is_empty() || VersionVector::decode(version).map(|v| v.is_empty()).unwrap_or(true);
        if empty { self.snapshot() } else { self.updates_since(version) }
    }

    /// Merge changes from another copy. Returns whether anything new came in.
    pub fn import(&self, bytes: &[u8]) -> DocResult<bool> {
        let before = self.doc.oplog_vv();
        self.doc.import(bytes).map_err(|e| format!("Couldn't merge changes: {e}"))?;
        Ok(self.doc.oplog_vv() != before)
    }

    /// Changes made here (after each commit), to send to the others.
    pub fn on_local_update(&self, f: impl Fn(&[u8]) + Send + Sync + 'static) -> loro::Subscription {
        self.doc.subscribe_local_update(Box::new(move |b| {
            f(b);
            true
        }))
    }

    /// Called after changes merged in from others.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) -> loro::Subscription {
        self.doc.subscribe_root(std::sync::Arc::new(move |_| f()))
    }

    fn commit(&self) {
        self.doc.commit();
    }

    /// Record which member a Loro peer (a DevCat, or one of its editor windows) types for.
    pub fn set_peer(&self, peer: u64, member: &str) {
        let peers = self.doc.get_map(PEERS);
        if peers.get(&peer.to_string()).is_none() {
            let _ = peers.insert(&peer.to_string(), member);
            self.commit();
        }
    }

    pub fn peer_member(&self, peer: u64) -> Option<String> {
        match self.doc.get_map(PEERS).get(&peer.to_string())?.into_value().ok()? {
            LoroValue::String(s) => Some(s.to_string()),
            _ => None,
        }
    }

    // ----------------------------------------------------------------- files

    fn meta(&self, path: &str) -> Option<FileMeta> {
        record(self.doc.get_map(FILES).get(path)?.into_value().ok()?)
    }

    fn text(&self, path: &str) -> LoroText {
        self.doc.get_text(text_root(path))
    }

    fn board(&self, path: &str) -> LoroMap {
        self.doc.get_map(board_root(path))
    }

    pub fn files(&self) -> Vec<FileInfo> {
        let mut out = Vec::new();
        self.doc.get_map(FILES).for_each(|path, v| {
            if let Some(meta) = v.into_value().ok().and_then(record::<FileMeta>) {
                let chars = match meta.kind {
                    FileKind::Text => self.text(path).len_unicode(),
                    FileKind::Board => self.board_shapes(path).len(),
                    FileKind::Blob => 0,
                };
                out.push(FileInfo { path: path.to_string(), meta, chars });
            }
        });
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Files in folders from before `root_part` kept their text under the raw path, which the
    /// window can't change without breaking the room: copy it to the new name. Only the room's
    /// owner does it (once), so two members can't both copy it and double the text. Returns
    /// whether anything moved.
    pub fn move_old_roots(&self) -> bool {
        let mut moved = false;
        for f in self.files().into_iter().filter(|f| f.path.contains(['/', '%'])) {
            match f.meta.kind {
                FileKind::Text => {
                    let new = self.text(&f.path);
                    let old = self.doc.get_text(format!("file:{}", f.path));
                    let content = old.to_string();
                    if new.len_unicode() == 0 && !content.is_empty() && new.insert(0, &content).is_ok() {
                        moved = true;
                    }
                }
                FileKind::Board => {
                    let (new, old) = (self.board(&f.path), self.doc.get_map(format!("board:{}", f.path)));
                    if new.is_empty() && !old.is_empty() {
                        old.for_each(|k, v| {
                            if let Ok(v) = v.into_value() {
                                let _ = new.insert(k, v);
                            }
                        });
                        moved = true;
                    }
                }
                FileKind::Blob => {}
            }
        }
        if moved {
            self.commit();
        }
        moved
    }

    /// The files in folder `dir` (and its folders).
    pub fn files_in(&self, dir: &str) -> Vec<FileInfo> {
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        self.files().into_iter().filter(|f| f.path.starts_with(&prefix)).collect()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.meta(path).is_some()
    }

    /// The text of a file, if it is a text file in the room.
    pub fn read(&self, path: &str) -> Option<String> {
        (self.meta(path)?.kind == FileKind::Text).then(|| self.text(path).to_string())
    }

    pub fn create_file(&self, path: &str, content: &str, by: &Author, now: i64) -> DocResult<()> {
        let path = clean_path(path)?;
        if self.exists(&path) {
            return Err(format!("{path} already exists in the room."));
        }
        if content.chars().count() > MAX_FILE_CHARS {
            return Err("The file is too large to edit together in a room (200,000 characters at most).".into());
        }
        let meta = FileMeta { kind: FileKind::Text, created_by: by.clone(), created_at: now, blob: None };
        self.doc.get_map(FILES).insert(&path, json(&meta)).map_err(err)?;
        let t = self.text(&path);
        // A file deleted and made again starts empty.
        if t.len_unicode() > 0 {
            t.delete(0, t.len_unicode()).map_err(err)?;
        }
        t.insert(0, content).map_err(err)?;
        self.commit();
        Ok(())
    }

    pub fn add_blob(&self, path: &str, blob: BlobRef, by: &Author, now: i64) -> DocResult<()> {
        let path = clean_path(path)?;
        if self.exists(&path) {
            return Err(format!("{path} already exists in the room."));
        }
        let meta = FileMeta { kind: FileKind::Blob, created_by: by.clone(), created_at: now, blob: Some(blob) };
        self.doc.get_map(FILES).insert(&path, json(&meta)).map_err(err)?;
        self.commit();
        Ok(())
    }

    /// A new, empty whiteboard.
    pub fn create_board(&self, path: &str, by: &Author, now: i64) -> DocResult<()> {
        let path = clean_path(path)?;
        if self.exists(&path) {
            return Err(format!("{path} already exists in the room."));
        }
        let meta = FileMeta { kind: FileKind::Board, created_by: by.clone(), created_at: now, blob: None };
        self.doc.get_map(FILES).insert(&path, json(&meta)).map_err(err)?;
        // A board deleted and made again starts empty.
        clear_map(&self.board(&path))?;
        self.commit();
        Ok(())
    }

    /// A whiteboard's shapes (Excalidraw elements), deleted ones left out.
    pub fn board_shapes(&self, path: &str) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        self.board(path).for_each(|_, v| {
            if let Some(el) = v.into_value().ok().and_then(record::<serde_json::Value>)
                && !el.get("isDeleted").and_then(serde_json::Value::as_bool).unwrap_or(false)
            {
                out.push(el);
            }
        });
        out
    }

    /// What a whiteboard says, for agents: its text (top to bottom, then left to right) and how
    /// many shapes of each kind it has.
    pub fn board_text(&self, path: &str) -> Option<String> {
        if self.meta(path)?.kind != FileKind::Board {
            return None;
        }
        let shapes = self.board_shapes(path);
        let num = |el: &serde_json::Value, k: &str| el.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let mut texts: Vec<(f64, f64, String)> = shapes
            .iter()
            .filter_map(|el| {
                let t = el.get("text").and_then(serde_json::Value::as_str)?.trim();
                (!t.is_empty()).then(|| (num(el, "y"), num(el, "x"), t.replace('\n', " ")))
            })
            .collect();
        texts.sort_by(|a, b| (a.0 / 40.0).floor().total_cmp(&(b.0 / 40.0).floor()).then(a.1.total_cmp(&b.1)));
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        for el in &shapes {
            *kinds.entry(el.get("type").and_then(serde_json::Value::as_str).unwrap_or("shape").to_string()).or_default() += 1;
        }
        let kinds: Vec<String> = kinds.into_iter().map(|(k, n)| format!("{n} {k}")).collect();
        let mut out = format!("Whiteboard {path}: {} shape(s) ({}).", shapes.len(), if kinds.is_empty() { "empty".into() } else { kinds.join(", ") });
        if !texts.is_empty() {
            out.push_str("\nText on it, top to bottom:\n");
            out.push_str(&texts.into_iter().map(|(_, _, t)| format!("- {t}")).collect::<Vec<_>>().join("\n"));
        }
        Some(out)
    }

    pub fn delete_file(&self, path: &str) -> DocResult<()> {
        let meta = self.meta(path).ok_or_else(|| format!("{path} isn't in the room."))?;
        self.doc.get_map(FILES).delete(path).map_err(err)?;
        match meta.kind {
            FileKind::Text => {
                let t = self.text(path);
                t.delete(0, t.len_unicode()).map_err(err)?;
            }
            FileKind::Board => clear_map(&self.board(path))?,
            FileKind::Blob => {}
        }
        self.commit();
        Ok(())
    }

    /// Delete every file in folder `dir`. Returns how many.
    pub fn delete_folder(&self, dir: &str) -> DocResult<usize> {
        let files = self.files_in(&clean_path(dir)?);
        for f in &files {
            self.delete_file(&f.path)?;
        }
        Ok(files.len())
    }

    /// Rename or move a file, or a folder with everything in it, to `to`. Refused while a file
    /// that moves has open suggestions or an agent's claim on it (their anchors stay with the old
    /// name). Returns each (old, new) path.
    pub fn rename(&self, from: &str, to: &str, now: i64) -> DocResult<Vec<(String, String)>> {
        let (from, to) = (clean_path(from)?, clean_path(to)?);
        if from == to {
            return Ok(vec![]);
        }
        let moves: Vec<(String, String)> = if self.exists(&from) {
            vec![(from.clone(), to.clone())]
        } else {
            if to.starts_with(&format!("{from}/")) {
                return Err("A folder can't move into itself.".into());
            }
            let n = from.len() + 1;
            self.files_in(&from).into_iter().map(|f| (f.path.clone(), format!("{to}/{}", &f.path[n..]))).collect()
        };
        if moves.is_empty() {
            return Err(format!("{from} isn't in the room."));
        }
        let (open, claims) = (self.suggestions(), self.claims(now));
        for (a, b) in &moves {
            if self.exists(b) {
                return Err(format!("{b} already exists in the room."));
            }
            if open.iter().any(|s| s.record.path == *a && s.record.state == SuggestionState::Open) {
                return Err(format!("{a} has suggestions waiting: accept or reject them first, then move it."));
            }
            if claims.iter().any(|c| c.path == *a) {
                return Err(format!("{} is editing {a} right now: try again when it's done.", claims.iter().find(|c| c.path == *a).map(|c| c.holder.label()).unwrap_or_default()));
            }
        }
        let files = self.doc.get_map(FILES);
        for (a, b) in &moves {
            let meta = self.meta(a).ok_or_else(|| format!("{a} isn't in the room."))?;
            match meta.kind {
                FileKind::Text => {
                    let (old, new) = (self.text(a), self.text(b));
                    let content = old.to_string();
                    if new.len_unicode() > 0 {
                        new.delete(0, new.len_unicode()).map_err(err)?;
                    }
                    new.insert(0, &content).map_err(err)?;
                    old.delete(0, old.len_unicode()).map_err(err)?;
                }
                FileKind::Board => {
                    let (old, new) = (self.board(a), self.board(b));
                    clear_map(&new)?;
                    let mut shapes = Vec::new();
                    old.for_each(|k, v| shapes.push((k.to_string(), v.into_value().ok())));
                    for (k, v) in shapes {
                        if let Some(v) = v {
                            new.insert(&k, v).map_err(err)?;
                        }
                    }
                    clear_map(&old)?;
                }
                FileKind::Blob => {}
            }
            files.insert(b, json(&meta)).map_err(err)?;
            files.delete(a).map_err(err)?;
        }
        self.commit();
        Ok(moves)
    }

    // ---------------------------------------------------------------- thread

    pub fn post(&self, author: &Author, text: &str, depth: u32, reply_to: Option<String>, now: i64) -> DocResult<Message> {
        let text = text.trim();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        let text: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
        let m = Message { id: new_id(), author: author.clone(), text, at: now, depth, reply_to };
        self.doc.get_list(THREAD).push(json(&m)).map_err(err)?;
        self.commit();
        Ok(m)
    }

    pub fn messages(&self) -> Vec<Message> {
        let mut out = Vec::new();
        self.doc.get_list(THREAD).for_each(|v| {
            if let Some(m) = v.into_value().ok().and_then(record::<Message>) {
                out.push(m);
            }
        });
        out
    }

    fn message_exists(&self, id: &str) -> bool {
        self.messages().iter().any(|m| m.id == id)
    }

    // ----------------------------------------------------------------- tasks

    pub fn tasks(&self) -> Vec<Task> {
        let mut out: Vec<Task> = Vec::new();
        self.doc.get_map(TASKS).for_each(|_, v| {
            if let Some(t) = v.into_value().ok().and_then(record::<Task>) {
                out.push(t);
            }
        });
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        out
    }

    pub fn task(&self, id: &str) -> Option<Task> {
        record(self.doc.get_map(TASKS).get(id)?.into_value().ok()?)
    }

    fn task_title(title: &str) -> DocResult<String> {
        let t = title.trim();
        if t.is_empty() {
            return Err("Give the task a title.".into());
        }
        Ok(t.chars().take(MAX_TASK_TITLE).collect())
    }

    pub fn add_task(&self, title: &str, notes: Option<String>, assignee: Option<Assignee>, by: &Author, now: i64) -> DocResult<Task> {
        let notes = notes.map(|n| n.trim().chars().take(MAX_MESSAGE_CHARS).collect::<String>()).filter(|n| !n.is_empty());
        let t = Task { id: new_id(), title: Self::task_title(title)?, notes, assignee, state: TaskState::Open, created_by: by.clone(), created_at: now, updated_by: None, updated_at: now };
        self.doc.get_map(TASKS).insert(&t.id, json(&t)).map_err(err)?;
        self.commit();
        Ok(t)
    }

    pub fn update_task(&self, id: &str, patch: TaskPatch, by: &Author, now: i64) -> DocResult<Task> {
        let mut t = self.task(id).ok_or("No such task (it may have been deleted).")?;
        if let Some(title) = patch.title {
            t.title = Self::task_title(&title)?;
        }
        if let Some(notes) = patch.notes {
            t.notes = notes.map(|n| n.trim().chars().take(MAX_MESSAGE_CHARS).collect::<String>()).filter(|n| !n.is_empty());
        }
        if let Some(a) = patch.assignee {
            t.assignee = a;
        }
        if let Some(s) = patch.state {
            t.state = s;
        }
        t.updated_by = Some(by.clone());
        t.updated_at = now;
        self.doc.get_map(TASKS).insert(&t.id, json(&t)).map_err(err)?;
        self.commit();
        Ok(t)
    }

    pub fn delete_task(&self, id: &str) -> DocResult<()> {
        self.task(id).ok_or("No such task.")?;
        self.doc.get_map(TASKS).delete(id).map_err(err)?;
        self.commit();
        Ok(())
    }

    // ------------------------------------------------------ pins, reactions

    /// Decisions: pinned messages, oldest pin first.
    pub fn pins(&self) -> Vec<Pin> {
        let mut out: Vec<Pin> = Vec::new();
        self.doc.get_map(PINS).for_each(|_, v| {
            if let Some(p) = v.into_value().ok().and_then(record::<Pin>) {
                out.push(p);
            }
        });
        out.sort_by(|a, b| (a.at, &a.message).cmp(&(b.at, &b.message)));
        out
    }

    /// Pin a message as a decision, or unpin it (`pin` false).
    pub fn pin(&self, message: &str, pin: bool, by: &Author, now: i64) -> DocResult<()> {
        if !self.message_exists(message) {
            return Err("That message isn't in the thread.".into());
        }
        let pins = self.doc.get_map(PINS);
        if pin {
            pins.insert(message, json(&Pin { message: message.to_string(), by: by.clone(), at: now })).map_err(err)?;
        } else {
            pins.delete(message).map_err(err)?;
        }
        self.commit();
        Ok(())
    }

    pub fn reactions(&self) -> Vec<Reaction> {
        let mut out: Vec<Reaction> = Vec::new();
        self.doc.get_map(REACTIONS).for_each(|_, v| {
            if let Some(r) = v.into_value().ok().and_then(record::<Reaction>) {
                out.push(r);
            }
        });
        out.sort_by(|a, b| (a.at, &a.by.member).cmp(&(b.at, &b.by.member)));
        out
    }

    /// Add `emoji` to a message, or take it back if `by` already put it there. Returns whether
    /// it's on now.
    pub fn react(&self, message: &str, emoji: &str, by: &Author, now: i64) -> DocResult<bool> {
        if !REACTION_EMOJI.contains(&emoji) {
            return Err("Not one of the room's reactions.".into());
        }
        if !self.message_exists(message) {
            return Err("That message isn't in the thread.".into());
        }
        let key = format!("{message}|{}|{emoji}", by.member);
        let map = self.doc.get_map(REACTIONS);
        let on = map.get(&key).is_none();
        if on {
            map.insert(&key, json(&Reaction { message: message.to_string(), emoji: emoji.to_string(), by: Author { via: None, agent: false, ..by.clone() }, at: now })).map_err(err)?;
        } else {
            map.delete(&key).map_err(err)?;
        }
        self.commit();
        Ok(on)
    }

    // ---------------------------------------------------------------- ranges

    fn cursor(&self, t: &LoroText, pos: usize) -> DocResult<String> {
        t.get_cursor(pos, Side::Middle).map(|c| hex::encode(c.encode())).ok_or_else(|| "Couldn't anchor the range.".to_string())
    }

    /// Where an anchor is now, and whether its character was deleted (then: where it was).
    fn pos(&self, c: &str) -> Option<(usize, bool)> {
        let c = Cursor::decode(&hex::decode(c).ok()?).ok()?;
        let r = self.doc.get_cursor_pos(&c).ok()?;
        Some((r.current.pos, r.update.is_some()))
    }

    /// Anchors for `[from, to)`: the first character, and the last (none for an empty range).
    fn anchors(&self, t: &LoroText, from: usize, to: usize) -> DocResult<(String, Option<String>)> {
        let start = self.cursor(t, from)?;
        let last = if to > from { Some(self.cursor(t, to - 1)?) } else { None };
        Ok((start, last))
    }

    /// Where an anchored range is now.
    fn resolve(&self, start: &str, last: Option<&str>) -> Option<(usize, usize)> {
        let (from, _) = self.pos(start)?;
        let to = match last {
            // The last character itself is gone: the range ends where it was.
            Some(l) => match self.pos(l)? {
                (p, true) => p,
                (p, false) => p + 1,
            },
            None => from,
        };
        (to >= from).then_some((from, to))
    }

    // ---------------------------------------------------------------- claims

    fn claim_records(&self) -> Vec<ClaimRecord> {
        let mut out = Vec::new();
        self.doc.get_map(CLAIMS).for_each(|_, v| {
            if let Some(c) = v.into_value().ok().and_then(record::<ClaimRecord>) {
                out.push(c);
            }
        });
        out
    }

    /// Claims still in force, with where they are now. Overlapping claims by different holders
    /// (made while out of sync): the earliest wins, ties go to the smaller member id.
    pub fn claims(&self, now: i64) -> Vec<ClaimView> {
        let mut views: Vec<ClaimView> = self
            .claim_records()
            .into_iter()
            .filter(|c| c.expires > now)
            .filter_map(|c| {
                let (from, to) = self.resolve(&c.start, Some(&c.last))?;
                let text: String = self.read(&c.path)?.chars().skip(from).take(to - from).collect();
                Some(ClaimView { id: c.id, path: c.path, from, to, text, holder: c.holder, at: c.at, expires: c.expires, winning: true, note: c.note })
            })
            .collect();
        views.sort_by(|a, b| (a.at, &a.holder.member, &a.id).cmp(&(b.at, &b.holder.member, &b.id)));
        for i in 0..views.len() {
            let beaten = views[..i].iter().any(|w| w.winning && w.path == views[i].path && w.holder != views[i].holder && w.from < views[i].to && views[i].from < w.to);
            views[i].winning = !beaten;
        }
        views
    }

    /// Claim the section `find` (exact text, once in the file) for [`CLAIM_TTL_MS`]. Claiming a
    /// section you already hold renews it. Fails while someone else holds an overlapping claim.
    pub fn claim(&self, path: &str, find: &str, holder: &Author, note: Option<String>, now: i64) -> DocResult<ClaimView> {
        let text = self.read(path).ok_or_else(|| format!("{path} isn't a text file in the room."))?;
        let (from, to) = locate(&text, find)?;
        if let Some(other) = self.claims(now).into_iter().find(|c| c.path == path && c.holder != *holder && c.from < to && from < c.to) {
            return Err(format!("{} is editing that part (claimed until {} min from now). Suggest a change instead, or ask in the thread.", other.holder.label(), ((other.expires - now) / 60_000).max(1)));
        }
        let mine = self.claims(now).into_iter().find(|c| c.path == path && c.holder == *holder && c.from == from && c.to == to);
        let t = self.text(path);
        let (start, last) = self.anchors(&t, from, to)?;
        let rec = ClaimRecord { id: mine.map(|c| c.id).unwrap_or_else(new_id), path: path.to_string(), start, last: last.unwrap_or_default(), holder: holder.clone(), at: now, expires: now + CLAIM_TTL_MS, note };
        self.doc.get_map(CLAIMS).insert(&rec.id, json(&rec)).map_err(err)?;
        self.commit();
        self.claims(now).into_iter().find(|c| c.id == rec.id).ok_or_else(|| "Couldn't claim that part.".to_string())
    }

    /// Give a claim back (only its holder can).
    pub fn release(&self, id: &str, holder: &Author) -> DocResult<()> {
        let c = self.claim_records().into_iter().find(|c| c.id == id).ok_or("No such claim.")?;
        if c.holder != *holder {
            return Err("Only whoever holds a claim can release it.".into());
        }
        self.doc.get_map(CLAIMS).delete(id).map_err(err)?;
        self.commit();
        Ok(())
    }

    /// Release every claim of `holder` (an agent's turn ended).
    pub fn release_all(&self, holder: &Author) {
        let mine: Vec<String> = self.claim_records().into_iter().filter(|c| c.holder == *holder).map(|c| c.id).collect();
        if !mine.is_empty() {
            for id in mine {
                let _ = self.doc.get_map(CLAIMS).delete(&id);
            }
            self.commit();
        }
    }

    /// Drop expired claims from the document.
    pub fn sweep_claims(&self, now: i64) {
        let old: Vec<String> = self.claim_records().into_iter().filter(|c| c.expires <= now).map(|c| c.id).collect();
        if !old.is_empty() {
            for id in old {
                let _ = self.doc.get_map(CLAIMS).delete(&id);
            }
            self.commit();
        }
    }

    // ----------------------------------------------------------- suggestions

    fn suggestion(&self, id: &str) -> Option<SuggestionRecord> {
        record(self.doc.get_map(SUGGESTIONS).get(id)?.into_value().ok()?)
    }

    fn put_suggestion(&self, s: &SuggestionRecord) -> DocResult<()> {
        self.doc.get_map(SUGGESTIONS).insert(&s.id, json(s)).map_err(err)
    }

    /// Propose replacing `find` (exact text, once in the file) with `replacement`. With a file
    /// that doesn't exist yet and `find` empty: propose a new file.
    pub fn suggest(&self, path: &str, find: &str, replacement: &str, author: &Author, note: Option<String>, now: i64) -> DocResult<SuggestionRecord> {
        let path = clean_path(path)?;
        let (base, start, last) = if !self.exists(&path) && find.is_empty() {
            (String::new(), None, None)
        } else {
            let text = self.read(&path).ok_or_else(|| format!("{path} isn't a text file in the room. To propose a new file, leave `find` empty."))?;
            let (from, to) = locate(&text, find)?;
            let t = self.text(&path);
            let (s, l) = self.anchors(&t, from, to)?;
            (find.to_string(), Some(s), l)
        };
        if base == replacement {
            return Err("The replacement is the same as the text.".into());
        }
        let s = SuggestionRecord { id: new_id(), path, author: author.clone(), at: now, note, base, replacement: replacement.to_string(), start, last, state: SuggestionState::Open, decided_by: None, decided_at: None, reason: None };
        self.put_suggestion(&s)?;
        self.commit();
        Ok(s)
    }

    pub fn suggestions(&self) -> Vec<SuggestionView> {
        let mut out = Vec::new();
        self.doc.get_map(SUGGESTIONS).for_each(|_, v| {
            if let Some(r) = v.into_value().ok().and_then(record::<SuggestionRecord>) {
                out.push(self.view(r));
            }
        });
        out.sort_by(|a, b| (a.record.at, &a.record.id).cmp(&(b.record.at, &b.record.id)));
        out
    }

    fn view(&self, r: SuggestionRecord) -> SuggestionView {
        let (mut from, mut to, mut outdated) = (None, None, false);
        if r.state == SuggestionState::Open {
            match &r.start {
                None => outdated = self.exists(&r.path),
                Some(s) => match (self.resolve(s, r.last.as_deref()), self.read(&r.path)) {
                    (Some((f, t)), Some(text)) => {
                        from = Some(f);
                        to = Some(t);
                        outdated = text.chars().skip(f).take(t - f).collect::<String>() != r.base;
                    }
                    _ => outdated = true,
                },
            }
        }
        SuggestionView { record: r, from, to, outdated }
    }

    /// Accept (apply) or reject a suggestion. An out-of-date one can't be accepted: its author
    /// (or anyone) proposes it again against the current text.
    pub fn decide(&self, id: &str, accept: bool, by: &Author, reason: Option<String>, now: i64) -> DocResult<Option<ChangeRecord>> {
        let s = self.suggestion(id).ok_or("No such suggestion.")?;
        if s.state != SuggestionState::Open {
            return Err("That suggestion was already decided.".into());
        }
        let mut done = SuggestionRecord { decided_by: Some(by.clone()), decided_at: Some(now), reason, ..s.clone() };
        if !accept {
            done.state = SuggestionState::Rejected;
            self.put_suggestion(&done)?;
            self.commit();
            return Ok(None);
        }
        let change = match &s.start {
            None => {
                if self.exists(&s.path) {
                    return Err(format!("{} was created meanwhile; this suggestion is out of date.", s.path));
                }
                self.create_file(&s.path, &s.replacement, &s.author, now)?;
                let t = self.text(&s.path);
                let (start, last) = self.anchors(&t, 0, s.replacement.chars().count())?;
                ChangeRecord { id: new_id(), kind: ChangeKind::NewFile, path: s.path.clone(), by: s.author.clone(), approved_by: Some(by.clone()), at: now, before: String::new(), after: s.replacement.clone(), start: Some(start), last, suggestion: Some(s.id.clone()), reverts: None }
            }
            Some(start) => {
                let (from, _) = self.checked_range(&s.path, start, s.last.as_deref(), &s.base).ok_or("Someone changed that text since; this suggestion is out of date. Propose it again.")?;
                self.replace_range(ChangeKind::Suggestion, &s.path, from, &s.base, &s.replacement, &s.author, Some(by), Some(s.id.clone()), None, now)?
            }
        };
        done.state = SuggestionState::Accepted;
        self.put_suggestion(&done)?;
        self.doc.get_list(HISTORY).push(json(&change)).map_err(err)?;
        self.commit();
        Ok(Some(change))
    }

    /// The range now, if it still holds exactly `expect`.
    fn checked_range(&self, path: &str, start: &str, last: Option<&str>, expect: &str) -> Option<(usize, usize)> {
        let (from, to) = self.resolve(start, last)?;
        let text = self.read(path)?;
        (text.chars().skip(from).take(to - from).collect::<String>() == expect).then_some((from, to))
    }

    /// Replace `old` at `from` with `new` as a minimal character diff (concurrent edits elsewhere
    /// in the range survive), and return the record of it (not yet in the history).
    #[allow(clippy::too_many_arguments)]
    fn replace_range(&self, kind: ChangeKind, path: &str, from: usize, old: &str, new: &str, by: &Author, approved: Option<&Author>, suggestion: Option<String>, reverts: Option<String>, now: i64) -> DocResult<ChangeRecord> {
        let t = self.text(path);
        if t.len_unicode() - old.chars().count() + new.chars().count() > MAX_FILE_CHARS {
            return Err("The file would get too large (200,000 characters at most).".into());
        }
        apply_minimal(&t, from, old, new)?;
        let (start, last) = self.anchors(&t, from, from + new.chars().count())?;
        Ok(ChangeRecord { id: new_id(), kind, path: path.to_string(), by: by.clone(), approved_by: approved.cloned(), at: now, before: old.to_string(), after: new.to_string(), start: Some(start), last, suggestion, reverts })
    }

    // ------------------------------------------------------------ agent edits

    /// An agent's direct edit: only inside a claim it holds (and wins), replacing `find` (exact
    /// text, once in the file) with `replacement`.
    pub fn edit(&self, path: &str, find: &str, replacement: &str, by: &Author, now: i64) -> DocResult<ChangeRecord> {
        let text = self.read(path).ok_or_else(|| format!("{path} isn't a text file in the room."))?;
        let (from, to) = locate(&text, find)?;
        let Some(claim) = self.claims(now).into_iter().find(|c| c.path == path && c.holder == *by && c.winning && c.from <= from && to <= c.to) else {
            return Err("Claim that part first (room_claim), then edit inside it. Without a claim, suggest the change instead.".into());
        };
        let change = self.replace_range(ChangeKind::Edit, path, from, find, replacement, by, None, None, None, now)?;
        // The claim keeps covering its section, now with the new text in it.
        let grown = claim.to + replacement.chars().count() - find.chars().count();
        if let Some(mut rec) = self.claim_records().into_iter().find(|c| c.id == claim.id) {
            let t = self.text(path);
            if grown > claim.from {
                let (start, last) = self.anchors(&t, claim.from, grown)?;
                rec.start = start;
                rec.last = last.unwrap_or_default();
                self.doc.get_map(CLAIMS).insert(&rec.id, json(&rec)).map_err(err)?;
            } else {
                self.doc.get_map(CLAIMS).delete(&rec.id).map_err(err)?;
            }
        }
        self.doc.get_list(HISTORY).push(json(&change)).map_err(err)?;
        self.commit();
        Ok(change)
    }

    // ---------------------------------------------------------------- history

    pub fn history(&self) -> Vec<ChangeRecord> {
        let mut out = Vec::new();
        self.doc.get_list(HISTORY).for_each(|v| {
            if let Some(c) = v.into_value().ok().and_then(record::<ChangeRecord>) {
                out.push(c);
            }
        });
        out
    }

    /// Whether a change can still be reverted (its text is still there, unchanged).
    pub fn revertible(&self, c: &ChangeRecord) -> bool {
        if self.history().iter().any(|h| h.reverts.as_deref() == Some(c.id.as_str())) {
            return false;
        }
        match (&c.kind, &c.start) {
            (ChangeKind::NewFile, _) => self.read(&c.path).as_deref() == Some(c.after.as_str()),
            (_, Some(start)) => self.checked_range(&c.path, start, c.last.as_deref(), &c.after).is_some(),
            _ => false,
        }
    }

    /// Undo an applied change, even an old one, if its text hasn't been changed since.
    pub fn revert(&self, id: &str, by: &Author, now: i64) -> DocResult<ChangeRecord> {
        let c = self.history().into_iter().find(|c| c.id == id).ok_or("No such change.")?;
        if !self.revertible(&c) {
            return Err("That text was changed since (or already reverted), so it can't be undone automatically.".into());
        }
        let rec = if c.kind == ChangeKind::NewFile {
            self.delete_file(&c.path)?;
            ChangeRecord { id: new_id(), kind: ChangeKind::Revert, path: c.path.clone(), by: by.clone(), approved_by: None, at: now, before: c.after.clone(), after: String::new(), start: None, last: None, suggestion: None, reverts: Some(c.id.clone()) }
        } else {
            let (from, _) = self.checked_range(&c.path, c.start.as_deref().unwrap_or_default(), c.last.as_deref(), &c.after).ok_or("That text was changed since.")?;
            self.replace_range(ChangeKind::Revert, &c.path, from, &c.after, &c.before, by, None, None, Some(c.id.clone()), now)?
        };
        self.doc.get_list(HISTORY).push(json(&rec)).map_err(err)?;
        self.commit();
        Ok(rec)
    }
}

impl Default for RoomDoc {
    fn default() -> Self {
        RoomDoc::new()
    }
}

/// Char range of the one occurrence of `find` in `text`.
pub fn locate(text: &str, find: &str) -> DocResult<(usize, usize)> {
    if find.is_empty() {
        return Err("`find` is empty: give the exact text of the part you mean.".into());
    }
    let hits: Vec<usize> = text.match_indices(find).map(|(i, _)| i).collect();
    match hits.as_slice() {
        [] => Err("That text isn't in the file (it has to match exactly, including spaces and line breaks). Read the file again.".into()),
        [i] => {
            let from = text[..*i].chars().count();
            Ok((from, from + find.chars().count()))
        }
        many => Err(format!("That text appears {} times in the file; include more of the surrounding text so it is unique.", many.len())),
    }
}

/// Apply `old` → `new` at `from` as the smallest set of character inserts and deletes.
fn apply_minimal(t: &LoroText, from: usize, old: &str, new: &str) -> DocResult<()> {
    let new_chars: Vec<char> = new.chars().collect();
    let diff = similar::TextDiff::configure().timeout(std::time::Duration::from_secs(2)).diff_chars(old, new);
    // Back to front, so earlier positions stay valid.
    for op in diff.ops().iter().rev() {
        let (tag, o, n) = op.as_tag_tuple();
        match tag {
            similar::DiffTag::Equal => {}
            similar::DiffTag::Delete => t.delete(from + o.start, o.len()).map_err(err)?,
            similar::DiffTag::Insert => t.insert(from + o.start, &new_chars[n].iter().collect::<String>()).map_err(err)?,
            similar::DiffTag::Replace => {
                t.delete(from + o.start, o.len()).map_err(err)?;
                t.insert(from + o.start, &new_chars[n].iter().collect::<String>()).map_err(err)?;
            }
        }
    }
    Ok(())
}

/// A file path as rooms keep it: relative, forward slashes, no `.`/`..`.
pub fn clean_path(p: &str) -> DocResult<String> {
    let parts: Vec<&str> = p.trim().split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect();
    if parts.is_empty() || parts.iter().any(|s| *s == ".." || s.contains(':')) {
        return Err(format!("Not a usable file name: {p:?}"));
    }
    let out = parts.join("/");
    if out.len() > 300 {
        return Err("The file name is too long.".into());
    }
    Ok(out)
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializes")
}

fn clear_map(m: &LoroMap) -> DocResult<()> {
    for k in m.keys().map(|k| k.to_string()).collect::<Vec<_>>() {
        m.delete(&k).map_err(err)?;
    }
    Ok(())
}

fn record<T: DeserializeOwned>(v: LoroValue) -> Option<T> {
    match v {
        LoroValue::String(s) => serde_json::from_str(&s).ok(),
        _ => None,
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(name: &str, agent: bool) -> Author {
        Author { member: format!("{name}-id"), name: name.into(), agent, via: None }
    }

    /// Two copies that exchange everything both ways.
    fn sync(a: &RoomDoc, b: &RoomDoc) {
        b.import(&a.updates_since(&b.version())).unwrap();
        a.import(&b.updates_since(&a.version())).unwrap();
    }

    #[test]
    fn concurrent_typing_keeps_both() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        a.create_file("plan.md", "The deadline is Friday.", &who("Arif", false), 1).unwrap();
        sync(&a, &b);
        // offline, each types
        a.text("plan.md").insert(22, " at 5pm").unwrap();
        a.commit();
        b.text("plan.md").insert(4, "final ").unwrap();
        b.commit();
        sync(&a, &b);
        assert_eq!(a.read("plan.md").unwrap(), "The final deadline is Friday at 5pm.");
        assert_eq!(b.read("plan.md"), a.read("plan.md"));
    }

    #[test]
    fn same_file_created_twice_merges() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        a.create_file("notes.md", "from A\n", &who("Arif", false), 1).unwrap();
        b.create_file("notes.md", "from B\n", &who("Akbar", false), 1).unwrap();
        sync(&a, &b);
        let t = a.read("notes.md").unwrap();
        assert!(t.contains("from A") && t.contains("from B"), "{t}");
        assert_eq!(b.read("notes.md").unwrap(), t);
    }

    #[test]
    fn thread_messages_from_both() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        a.post(&who("Arif", false), "hi", 0, None, 1).unwrap();
        b.post(&who("Akbar", false), "hello", 0, None, 1).unwrap();
        sync(&a, &b);
        let (ma, mb) = (a.messages(), b.messages());
        assert_eq!(ma.len(), 2);
        assert_eq!(ma, mb, "same order everywhere");
        assert!(a.post(&who("Arif", false), "  ", 0, None, 2).is_err());
    }

    #[test]
    fn suggestion_applies_minimally_and_survives_concurrent_typing() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        a.create_file("s.md", "Intro.\nShip v2 in March.\nOutro.", &who("Arif", false), 1).unwrap();
        sync(&a, &b);
        let s = a.suggest("s.md", "Ship v2 in March.", "Ship v2 in April after QA.", &who("Arif", true), None, 2).unwrap();
        // Meanwhile Akbar edits elsewhere in the file.
        b.text("s.md").insert(0, "# Plan\n").unwrap();
        b.commit();
        sync(&a, &b);
        let c = a.decide(&s.id, true, &who("Arif", false), None, 3).unwrap().unwrap();
        assert_eq!(a.read("s.md").unwrap(), "# Plan\nIntro.\nShip v2 in April after QA.\nOutro.");
        assert_eq!(c.approved_by.unwrap().name, "Arif");
        sync(&a, &b);
        assert_eq!(b.read("s.md"), a.read("s.md"));
        assert!(a.decide(&s.id, true, &who("Arif", false), None, 4).is_err(), "decided once");
    }

    #[test]
    fn outdated_suggestion_is_not_applied() {
        let a = RoomDoc::new();
        a.create_file("s.md", "Ship v2 in March.", &who("Arif", false), 1).unwrap();
        let s = a.suggest("s.md", "in March", "in April", &who("Akbar", true), None, 2).unwrap();
        assert!(!a.suggestions()[0].outdated);
        // Someone edits inside the range.
        let t = a.text("s.md");
        t.insert(14, "early ").unwrap(); // "Ship v2 in early March."? position inside "in March"
        a.commit();
        let v = &a.suggestions()[0];
        assert!(v.outdated, "{:?} {}", v, a.read("s.md").unwrap());
        assert!(a.decide(&s.id, true, &who("Arif", false), None, 3).is_err());
        assert!(!a.read("s.md").unwrap().contains("April"), "nothing applied");
        // Rejecting still works.
        a.decide(&s.id, false, &who("Arif", false), Some("stale".into()), 4).unwrap();
        assert_eq!(a.suggestions()[0].record.state, SuggestionState::Rejected);
    }

    #[test]
    fn typing_just_outside_a_range_keeps_it_valid() {
        let a = RoomDoc::new();
        a.create_file("s.md", "aa BBB cc", &who("Arif", false), 1).unwrap();
        a.suggest("s.md", "BBB", "XYZ", &who("Akbar", true), None, 2).unwrap();
        let t = a.text("s.md");
        t.insert(6, "!").unwrap(); // right after BBB
        t.insert(3, "?").unwrap(); // right before BBB
        a.commit();
        let v = &a.suggestions()[0];
        assert!(!v.outdated, "{v:?} {}", a.read("s.md").unwrap());
        assert_eq!((v.from, v.to), (Some(4), Some(7)));
    }

    #[test]
    fn new_file_suggestion() {
        let a = RoomDoc::new();
        let s = a.suggest("docs/new.md", "", "# New\n", &who("Akbar", true), None, 1).unwrap();
        let c = a.decide(&s.id, true, &who("Arif", false), None, 2).unwrap().unwrap();
        assert_eq!(a.read("docs/new.md").unwrap(), "# New\n");
        assert_eq!(c.kind, ChangeKind::NewFile);
        a.revert(&c.id, &who("Arif", false), 3).unwrap();
        assert!(!a.exists("docs/new.md"));
    }

    #[test]
    fn claims_lease_and_conflict() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        a.create_file("s.md", "one\ntwo\nthree\n", &who("Arif", false), 1).unwrap();
        sync(&a, &b);
        let (ag_a, ag_b) = (who("Arif", true), who("Akbar", true));
        // Out of sync, both claim the same line; Arif's agent was first.
        a.claim("s.md", "two", &ag_a, None, 10).unwrap();
        b.claim("s.md", "two", &ag_b, None, 11).unwrap();
        sync(&a, &b);
        let cs = a.claims(12);
        assert_eq!(cs.len(), 2);
        assert!(cs.iter().find(|c| c.holder == ag_a).unwrap().winning);
        assert!(!cs.iter().find(|c| c.holder == ag_b).unwrap().winning);
        assert_eq!(cs, b.claims(12), "both copies agree");
        // The loser can't edit; the winner can, only inside its claim.
        assert!(b.edit("s.md", "two", "TWO", &ag_b, 13).is_err());
        assert!(a.edit("s.md", "three", "THREE", &ag_a, 13).is_err(), "outside the claim");
        a.edit("s.md", "two", "2", &ag_a, 13).unwrap();
        assert_eq!(a.read("s.md").unwrap(), "one\n2\nthree\n");
        let mine = a.claims(14).into_iter().find(|c| c.holder == ag_a).unwrap();
        assert_eq!(mine.text, "2", "the claim follows the edited text");
        // While held, others can't claim it; after it expires they can.
        assert!(a.claim("s.md", "2", &ag_b, None, 14).is_err());
        assert!(a.claim("s.md", "2", &ag_b, None, 13 + CLAIM_TTL_MS + 1).is_ok());
        // Release.
        a.release_all(&ag_a);
        assert!(a.claims(15).iter().all(|c| c.holder != ag_a));
    }

    #[test]
    fn revert_old_change_only_if_untouched() {
        let a = RoomDoc::new();
        a.create_file("s.md", "alpha beta gamma", &who("Arif", false), 1).unwrap();
        let s = a.suggest("s.md", "beta", "BETA", &who("Akbar", true), None, 2).unwrap();
        let c = a.decide(&s.id, true, &who("Arif", false), None, 3).unwrap().unwrap();
        // Later edits elsewhere don't stop the revert.
        a.text("s.md").insert(0, ">> ").unwrap();
        a.commit();
        assert!(a.revertible(&c));
        a.revert(&c.id, &who("Arif", false), 4).unwrap();
        assert_eq!(a.read("s.md").unwrap(), ">> alpha beta gamma");
        assert!(a.revert(&c.id, &who("Arif", false), 5).is_err(), "only once");

        // A change whose text was edited since can't be reverted automatically.
        let s2 = a.suggest("s.md", "gamma", "GAMMA", &who("Akbar", true), None, 6).unwrap();
        let c2 = a.decide(&s2.id, true, &who("Arif", false), None, 7).unwrap().unwrap();
        let t = a.text("s.md");
        let at = a.read("s.md").unwrap().find("GAMMA").unwrap() + 2;
        t.insert(at, "x").unwrap();
        a.commit();
        assert!(!a.revertible(&c2));
        assert!(a.revert(&c2.id, &who("Arif", false), 8).is_err());
    }

    #[test]
    fn locate_and_paths() {
        assert_eq!(locate("héllo wörld", "wörld").unwrap(), (6, 11));
        assert!(locate("a a", "a").unwrap_err().contains("2 times"));
        assert!(locate("abc", "x").is_err());
        assert!(locate("abc", "").is_err());
        assert_eq!(clean_path("./docs\\a//b.md").unwrap(), "docs/a/b.md");
        assert!(clean_path("../x").is_err());
        assert!(clean_path("C:/x").is_err());
        assert!(clean_path("  ").is_err());
    }

    #[test]
    fn snapshot_round_trip_and_peers() {
        let a = RoomDoc::new();
        a.create_file("x.md", "x", &who("Arif", false), 1).unwrap();
        a.set_peer(a.peer(), "arif-id");
        let b = RoomDoc::load(&a.snapshot()).unwrap();
        assert_eq!(b.read("x.md").unwrap(), "x");
        assert_eq!(b.peer_member(a.peer()).as_deref(), Some("arif-id"));
        assert_ne!(a.peer(), b.peer(), "a loaded copy edits as a new peer");
        assert!(!b.import(&a.updates_since(&b.version())).unwrap(), "nothing new");
    }

    #[test]
    fn catch_up_sends_an_empty_copy_a_snapshot() {
        let a = RoomDoc::new();
        a.create_file("x.md", "x", &who("Arif", false), 1).unwrap();
        // The window sends its (empty) version encoded, or nothing.
        for v in [RoomDoc::new().version(), vec![]] {
            assert_eq!(a.catch_up(&v), a.snapshot());
            let b = RoomDoc::new();
            assert!(b.import(&a.catch_up(&v)).unwrap());
            assert_eq!(b.read("x.md").unwrap(), "x");
        }
        let b = RoomDoc::load(&a.snapshot()).unwrap();
        a.create_file("y.md", "y", &who("Arif", false), 2).unwrap();
        assert_eq!(a.catch_up(&b.version()), a.updates_since(&b.version()), "a copy with something gets updates");
        assert!(b.import(&a.catch_up(&b.version())).unwrap());
        assert_eq!(b.read("y.md").unwrap(), "y");
    }

    #[test]
    fn rename_and_move_files_and_folders() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        let arif = who("Arif", false);
        a.create_file("plan.md", "# Plan\n", &arif, 1).unwrap();
        a.create_file("docs/a.md", "A", &arif, 1).unwrap();
        a.create_file("docs/sub/b.md", "B", &arif, 1).unwrap();
        a.create_board("docs/flow", &arif, 1).unwrap();
        a.board("docs/flow").insert("s1", r#"{"id":"s1","type":"text","text":"Login","x":0,"y":0}"#).unwrap();
        a.commit();
        sync(&a, &b);

        // a file
        assert_eq!(a.rename("plan.md", "notes/plan.md", 2).unwrap(), vec![("plan.md".to_string(), "notes/plan.md".to_string())]);
        assert!(!a.exists("plan.md"));
        assert_eq!(a.read("notes/plan.md").unwrap(), "# Plan\n");
        // a folder, with what's in it (a board too)
        let moved = a.rename("docs", "spec", 3).unwrap();
        assert_eq!(moved.len(), 3);
        assert_eq!(a.read("spec/sub/b.md").unwrap(), "B");
        assert!(a.files_in("docs").is_empty());
        assert!(a.board_text("spec/flow").unwrap().contains("- Login"));
        assert_eq!(a.files().iter().find(|f| f.path == "spec/flow").unwrap().chars, 1);
        sync(&a, &b);
        assert_eq!(b.read("spec/a.md").unwrap(), "A", "the others see it moved");
        assert!(!b.exists("docs/a.md"));

        // refused: onto a file, into itself, while a suggestion waits, a missing name
        assert!(a.rename("spec/a.md", "notes/plan.md", 4).unwrap_err().contains("already exists"));
        assert!(a.rename("spec", "spec/inner", 4).unwrap_err().contains("into itself"));
        a.suggest("spec/a.md", "A", "AA", &who("Akbar", true), None, 4).unwrap();
        assert!(a.rename("spec/a.md", "spec/c.md", 5).unwrap_err().contains("suggestions waiting"));
        assert!(a.rename("nothing", "x", 5).is_err());

        // delete a folder
        assert_eq!(a.delete_folder("spec/sub").unwrap(), 1);
        assert!(!a.exists("spec/sub/b.md"));
    }

    /// Typed in the window's copy (loro-crdt 1.16.2, replica.ts naming): "typed in the window" in
    /// spec/a.md, and a shape on the board ideas/x. Loro refuses "/" in root names when it imports
    /// (a panic that took the room down), so the window and this side encode it.
    const WINDOW_UPDATE: &str = "6c6f726f000000000000000000000000e081ceda0004a901001400140110010700000000000000010100000000000501000001000b0204010200020401000004240273311066696c653a73706563253246612e6d640f626f6172643a69646561732532467800110104030300020204000303050b03031301004713747970656420696e207468652077696e646f7705317b226964223a227331222c2274797065223a2274657874222c2274657874223a224869222c2278223a302c2279223a307d";

    #[test]
    fn the_windows_changes_to_files_in_folders_merge() {
        let a = RoomDoc::new();
        let arif = who("Arif", false);
        a.create_file("spec/a.md", "", &arif, 1).unwrap();
        a.create_board("ideas/x", &arif, 1).unwrap();
        assert!(a.import(&hex::decode(WINDOW_UPDATE).unwrap()).unwrap());
        assert_eq!(a.read("spec/a.md").unwrap(), "typed in the window");
        assert!(a.board_text("ideas/x").unwrap().contains("- Hi"));
        // and they travel on to the others as usual
        let b = RoomDoc::new();
        b.import(&a.updates_since(&b.version())).unwrap();
        assert_eq!(b.read("spec/a.md").unwrap(), "typed in the window");
        assert_eq!(root_part("a%b/c.md"), "a%25b%2Fc.md");
        assert_eq!(text_root("plan.md"), "file:plan.md", "names without / stay as they were");
    }

    #[test]
    fn old_files_in_folders_move_to_their_new_name_once() {
        let a = RoomDoc::new();
        let arif = who("Arif", false);
        a.create_file("docs/old.md", "", &arif, 1).unwrap();
        // what a DevCat before root_part wrote: the raw path
        a.doc.get_text("file:docs/old.md").insert(0, "kept").unwrap();
        a.commit();
        assert_eq!(a.read("docs/old.md").unwrap(), "");
        assert!(a.move_old_roots());
        assert_eq!(a.read("docs/old.md").unwrap(), "kept");
        assert!(!a.move_old_roots(), "once");
    }

    #[test]
    fn boards_read_as_text_for_agents() {
        let a = RoomDoc::new();
        a.create_board("ideas", &who("Arif", false), 1).unwrap();
        let m = a.board("ideas");
        m.insert("1", r#"{"id":"1","type":"text","text":"Second row","x":5,"y":200}"#).unwrap();
        m.insert("2", r#"{"id":"2","type":"rectangle","x":0,"y":0}"#).unwrap();
        m.insert("3", r#"{"id":"3","type":"text","text":"Top right","x":300,"y":10}"#).unwrap();
        m.insert("4", r#"{"id":"4","type":"text","text":"Top left","x":0,"y":12}"#).unwrap();
        m.insert("5", r#"{"id":"5","type":"text","text":"gone","x":0,"y":0,"isDeleted":true}"#).unwrap();
        a.commit();
        let t = a.board_text("ideas").unwrap();
        assert!(t.starts_with("Whiteboard ideas: 4 shape(s) (1 rectangle, 3 text)."), "{t}");
        assert!(t.ends_with("- Top left\n- Top right\n- Second row"), "{t}");
        assert!(a.board_text("nope").is_none());
        assert!(a.read("ideas").is_none(), "not a text file");
        a.delete_file("ideas").unwrap();
        a.create_board("ideas", &who("Arif", false), 2).unwrap();
        assert!(a.board_shapes("ideas").is_empty(), "made again, it starts empty");
    }

    #[test]
    fn tasks_pins_and_reactions() {
        let (a, b) = (RoomDoc::new(), RoomDoc::new());
        let (arif, akbar) = (who("Arif", false), who("Akbar", false));
        let t = a.add_task("  Draft the intro ", None, Some(Assignee { member: akbar.member.clone(), agent: true }), &arif, 1).unwrap();
        assert_eq!((t.title.as_str(), t.state), ("Draft the intro", TaskState::Open));
        assert!(a.add_task(" ", None, None, &arif, 1).is_err());
        sync(&a, &b);
        let t2 = b.update_task(&t.id, TaskPatch { state: Some(TaskState::Doing), notes: Some(Some("on it".into())), ..Default::default() }, &akbar, 2).unwrap();
        assert_eq!(t2.assignee.as_ref().unwrap().member, akbar.member, "untouched fields stay");
        sync(&a, &b);
        assert_eq!(a.task(&t.id).unwrap().state, TaskState::Doing);
        let t3 = a.update_task(&t.id, TaskPatch { assignee: Some(None), notes: Some(None), ..Default::default() }, &arif, 3).unwrap();
        assert!(t3.assignee.is_none() && t3.notes.is_none(), "null clears");
        // a patch as the window sends it: a missing field keeps, null clears
        let p: TaskPatch = serde_json::from_str(r#"{"state":"done","assignee":null}"#).unwrap();
        assert_eq!((p.state, p.assignee.clone(), p.notes.clone()), (Some(TaskState::Done), Some(None), None));
        a.delete_task(&t.id).unwrap();
        assert!(a.tasks().is_empty());

        let m = a.post(&arif, "We ship on Friday.", 0, None, 4).unwrap();
        a.pin(&m.id, true, &arif, 5).unwrap();
        assert!(a.pin("nope", true, &arif, 5).is_err());
        sync(&a, &b);
        assert_eq!(b.pins().len(), 1);
        assert!(b.react(&m.id, "👍", &akbar, 6).unwrap(), "on");
        assert!(a.react(&m.id, "👍", &arif, 6).unwrap());
        assert!(a.react(&m.id, "💣", &arif, 6).is_err(), "only the room's reactions");
        sync(&a, &b);
        assert_eq!(a.reactions().len(), 2);
        assert!(!a.react(&m.id, "👍", &arif, 7).unwrap(), "a second click takes it back");
        a.pin(&m.id, false, &arif, 8).unwrap();
        sync(&a, &b);
        assert_eq!((b.reactions().len(), b.pins().len()), (1, 0));
    }

    #[test]
    fn authors_through_a_coding_agent() {
        let a = Author { via: Some("Claude Code".into()), ..who("Akbar", true) };
        assert_eq!(a.label(), "Akbar's agent (Claude Code)");
        assert_eq!(who("Akbar", true).label(), "Akbar's agent");
        // older records (no `via`) still read, and new ones don't write it when empty
        let old: Author = serde_json::from_str(r#"{"member":"m","name":"Arif","agent":true}"#).unwrap();
        assert_eq!(old.via, None);
        assert!(!serde_json::to_string(&who("Arif", false)).unwrap().contains("via"));
    }

    #[test]
    fn large_text_files_are_refused() {
        let a = RoomDoc::new();
        assert!(a.create_file("big.md", &"x".repeat(MAX_FILE_CHARS + 1), &who("Arif", false), 1).is_err());
        a.create_file("ok.md", &"x".repeat(MAX_FILE_CHARS), &who("Arif", false), 1).unwrap();
    }
}
