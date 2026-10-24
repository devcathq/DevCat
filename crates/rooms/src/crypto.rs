//! Room secrets: the room key encrypts everything members send each other (on top of iroh's own
//! end-to-end encryption, so relays and the blind keeper only ever see ciphertext), and the gossip
//! topic is derived from it, so only members can find the room's swarm.
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;

const NONCE: usize = 24;

/// 32 random bytes, shared by the members of one epoch.
#[derive(Clone, PartialEq, Eq)]
pub struct RoomKey(pub [u8; 32]);

impl std::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomKey(…)")
    }
}

impl RoomKey {
    pub fn generate() -> Self {
        RoomKey(random32())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        hex::decode(s.trim()).ok()?.try_into().ok().map(RoomKey)
    }

    /// `nonce || ciphertext`. `room` and `epoch` are bound in as associated data, so a message
    /// can't be replayed into another room or epoch.
    pub fn seal(&self, room: &[u8; 32], epoch: u32, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE];
        rand::rng().fill_bytes(&mut nonce);
        let aad = aad(room, epoch);
        let ct = XChaCha20Poly1305::new((&self.0).into())
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &aad })
            .expect("encryption never fails for in-memory buffers");
        let mut out = Vec::with_capacity(NONCE + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// The plaintext, or `None` if the message wasn't sealed with this key for this room/epoch.
    pub fn open(&self, room: &[u8; 32], epoch: u32, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < NONCE {
            return None;
        }
        let (nonce, ct) = sealed.split_at(NONCE);
        XChaCha20Poly1305::new((&self.0).into()).decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad(room, epoch) }).ok()
    }

    /// The gossip topic of this room in this epoch: unguessable without the key, and new when
    /// the key changes (a removed member can't follow the room to its new topic).
    pub fn topic(&self, room: &[u8; 32], epoch: u32) -> [u8; 32] {
        let mut h = blake3::Hasher::new_keyed(&self.0);
        h.update(b"devcat-room-topic-v1");
        h.update(room);
        h.update(&epoch.to_be_bytes());
        *h.finalize().as_bytes()
    }
}

fn aad(room: &[u8; 32], epoch: u32) -> Vec<u8> {
    let mut a = b"devcat-room-v1".to_vec();
    a.extend_from_slice(room);
    a.extend_from_slice(&epoch.to_be_bytes());
    a
}

pub fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    b
}

pub fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_binding() {
        let k = RoomKey::generate();
        let (room, other) = ([1u8; 32], [2u8; 32]);
        let s = k.seal(&room, 3, b"hello");
        assert_eq!(k.open(&room, 3, &s).as_deref(), Some(&b"hello"[..]));
        assert!(k.open(&room, 4, &s).is_none(), "other epoch");
        assert!(k.open(&other, 3, &s).is_none(), "other room");
        assert!(RoomKey::generate().open(&room, 3, &s).is_none(), "other key");
        let mut bad = s.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(k.open(&room, 3, &bad).is_none(), "tampered");
        assert!(k.open(&room, 3, &s[..10]).is_none(), "short");
        assert_ne!(k.seal(&room, 3, b"hello"), s, "fresh nonce each time");
    }

    #[test]
    fn topic_depends_on_key_and_epoch() {
        let (a, b) = (RoomKey::generate(), RoomKey::generate());
        let room = [7u8; 32];
        assert_eq!(a.topic(&room, 1), a.topic(&room, 1));
        assert_ne!(a.topic(&room, 1), a.topic(&room, 2));
        assert_ne!(a.topic(&room, 1), b.topic(&room, 1));
        assert_eq!(RoomKey::from_hex(&a.to_hex()), Some(a));
    }
}
