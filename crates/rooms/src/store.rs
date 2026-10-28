//! Rooms on disk, in `<dir>/`: this DevCat's room key pair (`key`), and per room
//! `<room id>/room.json` ([`RoomState`]), `<room id>/doc.loro` (the document) and
//! `<room id>/blobs/<hash>` (shared binary files, encrypted). Writes go to a temporary file first,
//! so a crash never leaves half a file.
use crate::model::{Invite, Membership};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How to reach a member's DevCat (besides iroh's DNS lookup).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
}

/// Who may wake this DevCat's agent in a room by mentioning it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mentions {
    /// Any member, and other agents (within the room's limits).
    #[default]
    Anyone,
    /// Only me.
    Me,
    /// The agent stays out of this room.
    Off,
}

/// This DevCat's own choices for a room (never shared).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    /// The AI connection the agent uses here (none: the first DevCat Agent connection).
    #[serde(default)]
    pub agent_provider: Option<String>,
    #[serde(default)]
    pub mentions: Mentions,
    /// This user's coding agents (Claude Code, Codex… through `devcat mcp`, and DevCat Agent
    /// chats) may use the room: read it, write in the thread, suggest changes. Off until the user
    /// turns it on.
    #[serde(default)]
    pub coding_agents: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomState {
    pub id: String,
    pub membership: Membership,
    /// Room keys by epoch (hex). Empty on a keeper.
    #[serde(default)]
    pub keys: BTreeMap<u32, String>,
    /// Invites (the owner's DevCat only).
    #[serde(default)]
    pub invites: Vec<Invite>,
    #[serde(default)]
    pub prefs: Prefs,
    /// Last known way to reach each member.
    #[serde(default)]
    pub hints: BTreeMap<String, Hints>,
    /// How far this DevCat has read each keeper's log.
    #[serde(default)]
    pub keeper_seen: BTreeMap<String, u64>,
    /// This DevCat was removed from the room (kept read-only until the user forgets it).
    #[serde(default)]
    pub removed: bool,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Store> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This DevCat's room identity (made on first use; separate from phone pairing's).
    pub fn secret_key(&self) -> std::io::Result<[u8; 32]> {
        let p = self.dir.join("key");
        if let Ok(s) = std::fs::read_to_string(&p) && let Some(k) = hex::decode(s.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()) {
            return Ok(k);
        }
        let k = crate::crypto::random32();
        write_atomic(&p, hex::encode(k).as_bytes())?;
        Ok(k)
    }

    fn room_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    pub fn room_ids(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return vec![] };
        rd.filter_map(|e| e.ok())
            .filter(|e| e.path().join("room.json").exists())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_id(n))
            .collect()
    }

    pub fn load(&self, id: &str) -> std::io::Result<(RoomState, Option<Vec<u8>>)> {
        let d = self.room_dir(id);
        let state: RoomState = serde_json::from_slice(&std::fs::read(d.join("room.json"))?).map_err(std::io::Error::other)?;
        let doc = std::fs::read(d.join("doc.loro")).ok();
        Ok((state, doc))
    }

    pub fn save_state(&self, s: &RoomState) -> std::io::Result<()> {
        let d = self.room_dir(&s.id);
        std::fs::create_dir_all(&d)?;
        write_atomic(&d.join("room.json"), &serde_json::to_vec_pretty(s).map_err(std::io::Error::other)?)
    }

    pub fn save_doc(&self, id: &str, snapshot: &[u8]) -> std::io::Result<()> {
        let d = self.room_dir(id);
        std::fs::create_dir_all(&d)?;
        write_atomic(&d.join("doc.loro"), snapshot)
    }

    pub fn blob_path(&self, room: &str, hash: &str) -> Option<PathBuf> {
        (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| self.room_dir(room).join("blobs").join(hash))
    }

    pub fn put_blob(&self, room: &str, hash: &str, sealed: &[u8]) -> std::io::Result<()> {
        let p = self.blob_path(room, hash).ok_or_else(|| std::io::Error::other("bad blob hash"))?;
        std::fs::create_dir_all(p.parent().unwrap())?;
        write_atomic(&p, sealed)
    }

    pub fn get_blob(&self, room: &str, hash: &str) -> Option<Vec<u8>> {
        std::fs::read(self.blob_path(room, hash)?).ok()
    }

    /// Forget a room on this DevCat.
    pub fn delete(&self, id: &str) -> std::io::Result<()> {
        if !valid_id(id) {
            return Err(std::io::Error::other("bad room id"));
        }
        match std::fs::remove_dir_all(self.room_dir(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The keeper's own files for a room it keeps: `<dir>/keep/<room id>/`.
    pub fn keep_dir(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.dir.join("keep").join(id))
    }
}

pub fn valid_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Member, Role, RoomSettings};

    #[test]
    fn round_trip() {
        let tmp = std::env::temp_dir().join(format!("devcat-rooms-store-{}", crate::now_ms()));
        let st = Store::new(&tmp).unwrap();
        let k = st.secret_key().unwrap();
        assert_eq!(st.secret_key().unwrap(), k, "stable");
        let id = "ab".repeat(32);
        let s = RoomState {
            id: id.clone(),
            membership: Membership { room: id.clone(), owner: "o".into(), version: 1, epoch: 0, settings: RoomSettings { name: "R".into(), agent_edits: false }, members: vec![Member { id: "o".into(), name: "O".into(), role: Role::Owner, added_at: 0 }], sig: String::new() },
            keys: [(0, "00".repeat(32))].into(),
            invites: vec![],
            prefs: Prefs::default(),
            hints: BTreeMap::new(),
            keeper_seen: BTreeMap::new(),
            removed: false,
        };
        st.save_state(&s).unwrap();
        st.save_doc(&id, b"doc").unwrap();
        assert_eq!(st.room_ids(), vec![id.clone()]);
        let (back, doc) = st.load(&id).unwrap();
        assert_eq!(back, s);
        assert_eq!(doc.as_deref(), Some(&b"doc"[..]));
        let h = "cd".repeat(32);
        st.put_blob(&id, &h, b"sealed").unwrap();
        assert_eq!(st.get_blob(&id, &h).as_deref(), Some(&b"sealed"[..]));
        assert!(st.blob_path(&id, "../x").is_none());
        st.delete(&id).unwrap();
        assert!(st.room_ids().is_empty());
        assert!(st.delete("../etc").is_err());
        let _ = std::fs::remove_dir_all(tmp);
    }
}
