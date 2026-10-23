//! Who is in a room and how to get in: roles, the member list the owner signs, invites and the
//! invite link.
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

pub const TICKET_SCHEME: &str = "devcat://room";

/// How long an invite link works.
pub const INVITE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Made the room; approves joins, removes members, changes settings.
    Owner,
    /// Reads, writes, talks; their agent can join in.
    Editor,
    /// Reads and talks; can't change documents.
    Viewer,
    /// A blind keeper: stores and forwards the room's encrypted changes, can't read them.
    Keeper,
}

impl Role {
    pub fn can_edit(self) -> bool {
        matches!(self, Role::Owner | Role::Editor)
    }
    /// Has the room key (everyone but a keeper).
    pub fn reads(self) -> bool {
        !matches!(self, Role::Keeper)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    /// The member's DevCat (iroh endpoint id, hex).
    pub id: String,
    pub name: String,
    pub role: Role,
    pub added_at: i64,
}

/// Room-wide settings, owned by the owner (signed with the member list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSettings {
    pub name: String,
    /// Agents may edit directly inside a section they claimed; otherwise they only suggest.
    #[serde(default)]
    pub agent_edits: bool,
}

/// The member list. Only the owner changes it, and signs every version; the newest valid version
/// wins. `epoch` names the room key in use: removing a member starts a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Membership {
    pub room: String,
    pub owner: String,
    pub version: u64,
    pub epoch: u32,
    pub settings: RoomSettings,
    pub members: Vec<Member>,
    /// The owner's signature over everything above (hex).
    #[serde(default)]
    pub sig: String,
}

impl Membership {
    fn signed_bytes(&self) -> Vec<u8> {
        let unsigned = Membership { sig: String::new(), ..self.clone() };
        let mut b = b"devcat-room-membership-v1".to_vec();
        b.extend(serde_json::to_vec(&unsigned).expect("serializes"));
        b
    }

    pub fn sign(&mut self, owner: &SecretKey) {
        self.sig = hex::encode(owner.sign(&self.signed_bytes()).to_bytes());
    }

    /// Signed by the owner it names, for the room it names.
    pub fn verify(&self, room: &str) -> bool {
        if self.room != room {
            return false;
        }
        let (Ok(owner), Some(sig)) = (self.owner.parse::<PublicKey>(), parse_sig(&self.sig)) else {
            return false;
        };
        owner.verify(&self.signed_bytes(), &sig).is_ok() && self.member(&self.owner).is_some_and(|m| m.role == Role::Owner)
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.id == id)
    }

    /// `other` is a valid, newer version of the same room from the same owner.
    pub fn superseded_by(&self, other: &Membership) -> bool {
        other.owner == self.owner && other.version > self.version && other.verify(&self.room)
    }
}

fn parse_sig(hex_sig: &str) -> Option<iroh::Signature> {
    let b: [u8; 64] = hex::decode(hex_sig).ok()?.try_into().ok()?;
    Some(iroh::Signature::from_bytes(&b))
}

/// An invite the owner made (kept by the owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    /// One-time token (hex); only its holder can ask to join.
    pub token: String,
    pub role: Role,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub used: bool,
}

impl Invite {
    pub fn usable(&self, now: i64) -> bool {
        !self.used && now < self.expires_at
    }
}

/// What an invite link carries:
/// `devcat://room?v=1&room=<id>&token=<t>&name=<room>&host=<owner id>[&relay=…][&home=…][&addr=…]`.
/// No room key: a leaked link only lets someone *ask* the owner to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub room: String,
    pub token: String,
    pub name: String,
    /// The owner's DevCat, who answers the join request.
    pub host: String,
    pub relay: Option<String>,
    pub home: Option<String>,
    pub addrs: Vec<String>,
}

impl Ticket {
    pub fn to_url(&self) -> String {
        let mut s = format!("{TICKET_SCHEME}?v=1&room={}&token={}&name={}&host={}", enc(&self.room), enc(&self.token), enc(&self.name), enc(&self.host));
        if let Some(r) = &self.relay {
            s.push_str(&format!("&relay={}", enc(r)));
        }
        if let Some(h) = &self.home {
            s.push_str(&format!("&home={}", enc(h)));
        }
        for a in &self.addrs {
            s.push_str(&format!("&addr={}", enc(a)));
        }
        s
    }

    pub fn parse(s: &str) -> Result<Ticket, String> {
        let q = s.trim().strip_prefix(TICKET_SCHEME).and_then(|q| q.strip_prefix('?')).ok_or("Not a DevCat room invite (it starts with devcat://room)")?;
        let (mut room, mut token, mut name, mut host, mut relay, mut home, mut addrs) = (None, None, None, None, None, None, Vec::new());
        for part in q.split('&') {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let v = dec(v);
            match k {
                "room" => room = Some(v),
                "token" => token = Some(v),
                "name" => name = Some(v),
                "host" => host = Some(v),
                "relay" if !v.is_empty() => relay = Some(v),
                "home" if !v.is_empty() => home = Some(v),
                "addr" if !v.is_empty() && !addrs.contains(&v) => addrs.push(v),
                _ => {}
            }
        }
        let hex64 = |v: &Option<String>| v.as_ref().is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()));
        if !hex64(&room) || !hex64(&host) {
            return Err("The invite is damaged (no valid room or owner)".into());
        }
        let token = token.filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("The invite has no valid token")?;
        Ok(Ticket { room: room.unwrap().to_lowercase(), token: token.to_lowercase(), name: name.unwrap_or_else(|| "Room".into()), host: host.unwrap().to_lowercase(), relay, home, addrs })
    }
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn membership(owner: &SecretKey) -> Membership {
        let id = owner.public().to_string();
        Membership {
            room: "ab".repeat(32),
            owner: id.clone(),
            version: 1,
            epoch: 0,
            settings: RoomSettings { name: "Q4 spec".into(), agent_edits: false },
            members: vec![Member { id, name: "Arif".into(), role: Role::Owner, added_at: 1 }],
            sig: String::new(),
        }
    }

    #[test]
    fn membership_signature() {
        let owner = SecretKey::generate();
        let mut m = membership(&owner);
        assert!(!m.verify(&m.room.clone()), "unsigned");
        m.sign(&owner);
        assert!(m.verify(&m.room.clone()));
        assert!(!m.verify(&"cd".repeat(32)), "other room");

        // Anyone else editing it breaks the signature.
        let mut forged = m.clone();
        forged.members.push(Member { id: SecretKey::generate().public().to_string(), name: "Mallory".into(), role: Role::Editor, added_at: 2 });
        assert!(!forged.verify(&m.room.clone()));
        // Re-signed by someone who isn't the owner it names.
        let mallory = SecretKey::generate();
        forged.sign(&mallory);
        assert!(!forged.verify(&m.room.clone()));
        // Claiming to be the owner requires the owner's key; a self-made owner isn't the room's owner.
        let mut takeover = membership(&mallory);
        takeover.room = m.room.clone();
        takeover.version = 9;
        takeover.sign(&mallory);
        assert!(takeover.verify(&m.room.clone()), "valid on its own…");
        assert!(!m.superseded_by(&takeover), "…but not a newer version of *this* owner's room");

        let mut next = m.clone();
        next.version = 2;
        next.sign(&owner);
        assert!(m.superseded_by(&next));
        assert!(!next.superseded_by(&m), "older");
    }

    #[test]
    fn ticket_round_trip() {
        let t = Ticket {
            room: "ab".repeat(32),
            token: "0f".repeat(16),
            name: "Q4 spec & plan".into(),
            host: "cd".repeat(32),
            relay: Some("public".into()),
            home: Some("https://aps1-1.relay.n0.iroh.link./".into()),
            addrs: vec!["192.168.1.2:47101".into()],
        };
        let url = t.to_url();
        assert!(url.starts_with("devcat://room?v=1&room=abab"), "{url}");
        assert_eq!(Ticket::parse(&format!("  {url}\n")).unwrap(), t);
        assert!(Ticket::parse("devcat://pair?v=1").is_err());
        assert!(Ticket::parse(&url.replace("token=0f0f", "token=zz0f")).is_err());
        assert!(Ticket::parse(&url.replace(&"cd".repeat(32), "cd")).is_err());
    }

    #[test]
    fn invite_usable() {
        let i = Invite { token: "00".repeat(16), role: Role::Editor, created_at: 0, expires_at: 100, used: false };
        assert!(i.usable(99));
        assert!(!i.usable(100));
        assert!(!Invite { used: true, ..i }.usable(1));
    }
}
