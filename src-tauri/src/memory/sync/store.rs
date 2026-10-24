//! Memory sync's files in `<app data>/memory-sync/`: this computer's sync key, the paired
//! computers, the index of shared files (`plan::Index`) and the audit log. Readable by the current
//! user only, like remote access's.
use super::plan::Index;
use super::wire::Hints;
use crate::error::{AppError, AppResult};
use crate::remote::store::{AuditEntry, audit_append, audit_read, random_bytes, write_private};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A paired computer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// Its sync key (iroh endpoint id, hex): the only thing that lets it in.
    pub id: String,
    pub name: String,
    /// macos | windows | linux
    #[serde(default)]
    pub os: String,
    pub paired_at: i64,
    /// The last sync that went through (either side started it).
    #[serde(default)]
    pub last_sync: Option<i64>,
    /// What the last sync moved ("2 received, 1 sent"), when it moved anything.
    #[serde(default)]
    pub last_result: Option<String>,
    /// Why the last sync this computer started failed (cleared by the next one that works).
    #[serde(default)]
    pub last_error: Option<String>,
    /// Where it was last reached.
    #[serde(default)]
    pub hints: Hints,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Self { dir: dir.to_path_buf() }
    }

    fn read_json<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        std::fs::read(self.dir.join(name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// This computer's sync identity (32 secret bytes), created on first use. Its own key, not
    /// remote access's: syncing memory doesn't need phones turned on, nor the other way round.
    pub fn secret_key(&self) -> AppResult<[u8; 32]> {
        let path = self.dir.join("node.key");
        if let Ok(hex_text) = std::fs::read_to_string(&path) {
            let bytes = hex::decode(hex_text.trim()).map_err(|e| AppError::msg(format!("memory-sync/node.key: {e}")))?;
            return bytes.try_into().map_err(|_| AppError::msg("memory-sync/node.key has the wrong length"));
        }
        let key: [u8; 32] = random_bytes();
        write_private(&path, hex::encode(key).as_bytes())?;
        Ok(key)
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.read_json("peers.json")
    }

    fn save_peers(&self, p: &[Peer]) -> AppResult<()> {
        write_private(&self.dir.join("peers.json"), &serde_json::to_vec_pretty(p)?)
    }

    pub fn peer(&self, id: &str) -> Option<Peer> {
        self.peers().into_iter().find(|p| p.id == id)
    }

    pub fn add_peer(&self, p: Peer) -> AppResult<()> {
        let mut all = self.peers();
        all.retain(|x| x.id != p.id);
        all.push(p);
        self.save_peers(&all)
    }

    pub fn update_peer(&self, id: &str, f: impl FnOnce(&mut Peer)) -> Option<Peer> {
        let mut all = self.peers();
        let found = all.iter_mut().find(|p| p.id == id).map(|p| {
            f(p);
            p.clone()
        });
        if found.is_some() {
            let _ = self.save_peers(&all);
        }
        found
    }

    pub fn remove_peer(&self, id: &str) -> Option<Peer> {
        let mut all = self.peers();
        let gone = all.iter().position(|p| p.id == id).map(|i| all.remove(i));
        if gone.is_some() {
            let _ = self.save_peers(&all);
        }
        gone
    }

    pub fn index(&self) -> Index {
        self.read_json("index.json")
    }

    pub fn save_index(&self, i: &Index) -> AppResult<()> {
        write_private(&self.dir.join("index.json"), &serde_json::to_vec(i)?)
    }

    pub fn audit(&self, e: &AuditEntry) {
        audit_append(&self.dir.join("audit.jsonl"), e);
    }

    /// Newest first.
    pub fn audit_tail(&self, n: usize) -> Vec<AuditEntry> {
        audit_read(&self.dir.join("audit.jsonl"), n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_and_key() {
        let d = std::env::temp_dir().join(format!("devcat-msync-store-{}", uuid::Uuid::new_v4()));
        let s = Store::new(&d);
        let k = s.secret_key().unwrap();
        assert_eq!(k, s.secret_key().unwrap(), "stable");
        let p = Peer { id: "a".into(), name: "Desk".into(), os: "linux".into(), paired_at: 1, last_sync: None, last_result: None, last_error: None, hints: Hints::default() };
        s.add_peer(p.clone()).unwrap();
        s.add_peer(p.clone()).unwrap();
        assert_eq!(s.peers().len(), 1);
        s.update_peer("a", |p| p.last_sync = Some(5));
        assert_eq!(s.peer("a").unwrap().last_sync, Some(5));
        assert!(s.remove_peer("a").is_some() && s.remove_peer("a").is_none());
    }
}
