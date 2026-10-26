//! The blind keeper: a DevCat that is always on (a server, or anyone's spare machine) and keeps a
//! room's changes for members who are offline when others edit. It never has the room key: members
//! push changes already sealed with it, and fetch what they missed. It only answers members of the
//! member list the owner signed, and can be limited to rooms of chosen owners.
//!
//! On disk, per room: `<dir>/<room>/membership.json` and `<dir>/<room>/log.jsonl` (one
//! `{"i":n,"item":"…"}` per line).
use crate::model::{Membership, Role};
use crate::wire::Reply;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Items handed out per fetch.
const PAGE: usize = 500;
/// Largest item (one sealed batch of changes).
const MAX_ITEM: usize = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Line {
    i: u64,
    item: String,
}

pub struct Keeper {
    dir: PathBuf,
    owners: Vec<String>,
    me: String,
    lock: Mutex<()>,
}

impl Keeper {
    pub fn new(dir: PathBuf, owners: Vec<String>, me: String) -> Keeper {
        let _ = std::fs::create_dir_all(&dir);
        Keeper { dir, owners, me, lock: Mutex::new(()) }
    }

    fn denied(reason: &str) -> Reply {
        Reply::Denied { reason: reason.to_string(), removed: false }
    }

    /// The member list to judge `peer` by: the newest valid one of what's stored and what came.
    fn admit(&self, peer: &str, room: &str, offered: Membership) -> Result<PathBuf, Box<Reply>> {
        if !crate::store::valid_id(room) {
            return Err(Self::denied("Bad room id.").into());
        }
        let d = self.dir.join(room);
        let stored: Option<Membership> = std::fs::read(d.join("membership.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let m = match stored {
            Some(s) if s.superseded_by(&offered) => offered,
            Some(s) => s,
            None => {
                if !offered.verify(room) {
                    return Err(Self::denied("The member list isn't signed by the room's owner.").into());
                }
                if !self.owners.is_empty() && !self.owners.contains(&offered.owner) {
                    return Err(Self::denied("This keeper doesn't keep rooms of that owner.").into());
                }
                offered
            }
        };
        if m.member(&self.me).map(|x| x.role) != Some(Role::Keeper) {
            return Err(Self::denied("This DevCat isn't the room's keeper.").into());
        }
        if !m.member(peer).is_some_and(|x| x.role.reads()) {
            return Err(Reply::Denied { reason: "You're not in this room.".into(), removed: true }.into());
        }
        std::fs::create_dir_all(&d).map_err(|e| Box::new(Self::denied(&e.to_string())))?;
        let _ = crate::store::write_atomic(&d.join("membership.json"), &serde_json::to_vec(&m).expect("serializes"));
        Ok(d)
    }

    fn last(d: &std::path::Path) -> u64 {
        let Ok(f) = std::fs::File::open(d.join("log.jsonl")) else { return 0 };
        std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Line>(&l).ok()).map(|l| l.i).last().unwrap_or(0)
    }

    pub fn push(&self, peer: &str, room: &str, membership: Membership, items: Vec<String>) -> Reply {
        let _g = self.lock.lock();
        let d = match self.admit(peer, room, membership) {
            Ok(d) => d,
            Err(r) => return *r,
        };
        let mut last = Self::last(&d);
        let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("log.jsonl")) else {
            return Self::denied("The keeper can't write.");
        };
        for item in items.into_iter().filter(|i| i.len() <= MAX_ITEM) {
            last += 1;
            let line = serde_json::to_string(&Line { i: last, item }).expect("serializes");
            if writeln!(f, "{line}").is_err() {
                return Self::denied("The keeper can't write.");
            }
        }
        let _ = f.sync_all();
        Reply::Kept { last }
    }

    pub fn fetch(&self, peer: &str, room: &str, membership: Membership, after: u64) -> Reply {
        let _g = self.lock.lock();
        let d = match self.admit(peer, room, membership) {
            Ok(d) => d,
            Err(r) => return *r,
        };
        let mut items = Vec::new();
        let mut last = after;
        if let Ok(f) = std::fs::File::open(d.join("log.jsonl")) {
            for l in std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Line>(&l).ok()) {
                last = last.max(l.i);
                if l.i > after && items.len() < PAGE {
                    items.push((l.i, l.item));
                } else if items.len() >= PAGE {
                    last = items.last().map(|(i, _)| *i).unwrap_or(after);
                    break;
                }
            }
        }
        Reply::Items { items, last }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Member, RoomSettings};
    use iroh::SecretKey;

    #[test]
    fn keeps_for_members_only() {
        let owner = SecretKey::generate();
        let (oid, keeper, alice, eve) = (owner.public().to_string(), SecretKey::generate().public().to_string(), SecretKey::generate().public().to_string(), SecretKey::generate().public().to_string());
        let room = "ab".repeat(32);
        let mut m = Membership {
            room: room.clone(),
            owner: oid.clone(),
            version: 1,
            epoch: 0,
            settings: RoomSettings { name: "R".into(), agent_edits: false },
            members: vec![
                Member { id: oid.clone(), name: "O".into(), role: Role::Owner, added_at: 0 },
                Member { id: alice.clone(), name: "A".into(), role: Role::Editor, added_at: 0 },
                Member { id: keeper.clone(), name: "K".into(), role: Role::Keeper, added_at: 0 },
            ],
            sig: String::new(),
        };
        m.sign(&owner);
        let dir = std::env::temp_dir().join(format!("devcat-keeper-{}", crate::now_ms()));
        let k = Keeper::new(dir.clone(), vec![], keeper.clone());
        assert!(matches!(k.push(&alice, &room, m.clone(), vec!["x".into(), "y".into()]), Reply::Kept { last: 2 }));
        assert!(matches!(k.push(&oid, &room, m.clone(), vec!["z".into()]), Reply::Kept { last: 3 }));
        match k.fetch(&alice, &room, m.clone(), 1) {
            Reply::Items { items, last } => {
                assert_eq!(items.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), ["y", "z"]);
                assert_eq!(last, 3);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(k.fetch(&eve, &room, m.clone(), 0), Reply::Denied { .. }), "not a member");
        // Eve forges a member list naming herself: not signed by the stored owner.
        let mut forged = m.clone();
        forged.version = 9;
        forged.members.push(Member { id: eve.clone(), name: "E".into(), role: Role::Editor, added_at: 0 });
        assert!(matches!(k.fetch(&eve, &room, forged, 0), Reply::Denied { .. }));
        // The owner removes Alice: the newer list counts from then on.
        let mut next = m.clone();
        next.version = 2;
        next.members.retain(|x| x.id != alice);
        next.sign(&owner);
        assert!(matches!(k.fetch(&oid, &room, next, 0), Reply::Items { .. }));
        assert!(matches!(k.fetch(&alice, &room, m.clone(), 0), Reply::Denied { removed: true, .. }), "the older list she offers doesn't win");
        // A keeper limited to other owners keeps nothing for this room.
        let k2 = Keeper::new(dir.join("other"), vec!["ff".repeat(32)], keeper.clone());
        assert!(matches!(k2.push(&alice, &room, m, vec!["x".into()]), Reply::Denied { .. }));
        let _ = std::fs::remove_dir_all(dir);
    }
}
