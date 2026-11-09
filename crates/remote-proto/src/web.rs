//! Browser access: DevCat Mobile as a web page served by the host over plain HTTP on the LAN
//! (browsers only allow their own crypto on HTTPS pages, so the page brings its own and the
//! host checks it here). The same messages as the app, over a WebSocket at `/ws`:
//!
//! 1. Text frames, the handshake:
//!    browser → `{"t":"open","device":<device id> | null,"nonce":<16 random bytes, b64url>}`
//!              (`device` null = pairing with the secret from the QR link, else reconnecting)
//!    host    → `{"t":"challenge","nonce":<16 random bytes, b64url>}` | `{"t":"denied","reason":…}`
//! 2. Binary frames, everything after: `seq (u64 BE) ‖ ChaCha20-Poly1305(key, nonce = 0u32 ‖ seq,
//!    aad = seq, JSON)`. `seq` counts from 0 in each direction; a frame with any other number is
//!    rejected (no replays, no reordering).
//!
//! Keys: `prk = HKDF-SHA256-Extract(salt = browser nonce ‖ host nonce, ikm = secret)`;
//! `key = HKDF-Expand(prk, "devcat-web/1 <mode> <dir>", 32)`, mode `pair` (secret = the 32-byte
//! pairing secret in the QR link), `device` (secret = the device secret from `paired`) or `code`,
//! dir `c2h` / `h2c`. A wrong secret fails to decrypt the first frame.
//!
//! Pairing with just the code someone typed (mode `code`, no QR link, so no 32-byte secret): the
//! open carries `key`, a fresh X25519 public key, and the challenge the host's own; the secret is
//! `X25519 shared secret ‖ the code` ([`normalize_code`]). Someone watching the network can't
//! compute the shared secret, so can't test guesses of the code offline either; a wrong code fails
//! to decrypt the hello and counts as a wrong try (five end the pairing). The page itself comes over
//! plain HTTP, so this protects against people *watching* the network, not against one able to
//! *change* traffic (it could alter the page); the app has no such gap.
use serde::{Deserialize, Serialize};

pub const WS_PATH: &str = "/ws";
pub const NONCE_LEN: usize = 16;
pub const SECRET_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Handshake {
    Open {
        device: Option<String>,
        nonce: String,
        /// Pairing by code: the browser's X25519 public key (b64url).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Challenge {
        nonce: String,
        /// Pairing by code: the host's X25519 public key (b64url).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Denied { reason: String },
}

/// Which secret the keys come from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Pair,
    Device,
    /// Pairing with a typed code (see the module docs).
    Code,
}

/// A pairing code as typed: letters and digits only, upper case (`ab3k-7q` → `AB3K7Q`).
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// A `k=v&…` value from a link fragment (percent-decoded).
pub fn fragment_param(fragment: &str, key: &str) -> Option<String> {
    fragment.trim_start_matches('#').split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| super::dec(v))
    })
}

#[cfg(feature = "crypto")]
pub use crypto::*;

#[cfg(feature = "crypto")]
mod crypto {
    use super::Mode;
    use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
    use ring::agreement::{EphemeralPrivateKey, UnparsedPublicKey, X25519, agree_ephemeral};
    use ring::hkdf::{HKDF_SHA256, KeyType, Salt};
    use ring::rand::SystemRandom;

    /// One side's X25519 key for pairing by code (used once).
    pub struct CodeKey {
        private: EphemeralPrivateKey,
        pub public: [u8; 32],
    }

    impl CodeKey {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).expect("x25519 key");
            let mut public = [0u8; 32];
            public.copy_from_slice(private.compute_public_key().expect("x25519 public key").as_ref());
            Self { private, public }
        }

        /// The secret `code` mode's keys come from: the shared secret ‖ the code. `None` when
        /// the other side's key isn't a usable X25519 key.
        pub fn secret(self, peer: &[u8], code: &str) -> Option<Vec<u8>> {
            let code = super::normalize_code(code);
            agree_ephemeral(self.private, &UnparsedPublicKey::new(&X25519, peer), |shared| [shared, code.as_bytes()].concat()).ok()
        }
    }

    struct Len32;
    impl KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }

    fn derive(secret: &[u8], salt: &[u8], info: &str) -> [u8; 32] {
        let prk = Salt::new(HKDF_SHA256, salt).extract(secret);
        let mut out = [0u8; 32];
        let infos = [info.as_bytes()];
        prk.expand(&infos, Len32).expect("hkdf length").fill(&mut out).expect("hkdf fill");
        out
    }

    /// One direction: a key and its frame counter.
    pub struct Half {
        key: LessSafeKey,
        seq: u64,
    }

    impl Half {
        fn new(k: [u8; 32]) -> Self {
            Self { key: LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &k).expect("key")), seq: 0 }
        }

        fn nonce(seq: u64) -> Nonce {
            let mut n = [0u8; 12];
            n[4..].copy_from_slice(&seq.to_be_bytes());
            Nonce::assume_unique_for_key(n)
        }

        pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
            let seq = self.seq.to_be_bytes();
            let mut buf = plaintext.to_vec();
            self.key.seal_in_place_append_tag(Self::nonce(self.seq), Aad::from(seq), &mut buf).expect("seal");
            self.seq += 1;
            let mut out = seq.to_vec();
            out.extend(buf);
            out
        }

        /// `None` if the frame is out of order, replayed, tampered or under another key.
        pub fn open(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
            if frame.len() < 8 + 16 {
                return None;
            }
            let seq = u64::from_be_bytes(frame[..8].try_into().ok()?);
            if seq != self.seq {
                return None;
            }
            let mut buf = frame[8..].to_vec();
            let plain = self.key.open_in_place(Self::nonce(seq), Aad::from(seq.to_be_bytes()), &mut buf).ok()?.to_vec();
            self.seq += 1;
            Some(plain)
        }
    }

    /// The host's side of a session: `send` = host → browser, `recv` = browser → host.
    pub struct HostKeys {
        pub send: Half,
        pub recv: Half,
    }

    /// The browser's side (for tests; the real browser does the same in JavaScript).
    pub struct BrowserKeys {
        pub send: Half,
        pub recv: Half,
    }

    fn keys(secret: &[u8], client_nonce: &[u8], host_nonce: &[u8], mode: Mode) -> ([u8; 32], [u8; 32]) {
        let salt = [client_nonce, host_nonce].concat();
        let m = match mode {
            Mode::Pair => "pair",
            Mode::Device => "device",
            Mode::Code => "code",
        };
        (derive(secret, &salt, &format!("devcat-web/1 {m} c2h")), derive(secret, &salt, &format!("devcat-web/1 {m} h2c")))
    }

    pub fn host_keys(secret: &[u8], client_nonce: &[u8], host_nonce: &[u8], mode: Mode) -> HostKeys {
        let (c2h, h2c) = keys(secret, client_nonce, host_nonce, mode);
        HostKeys { send: Half::new(h2c), recv: Half::new(c2h) }
    }

    pub fn browser_keys(secret: &[u8], client_nonce: &[u8], host_nonce: &[u8], mode: Mode) -> BrowserKeys {
        let (c2h, h2c) = keys(secret, client_nonce, host_nonce, mode);
        BrowserKeys { send: Half::new(c2h), recv: Half::new(h2c) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trip_and_rejections() {
            let (s, c, h) = ([7u8; 32], [1u8; 16], [2u8; 16]);
            let mut host = host_keys(&s, &c, &h, Mode::Device);
            let mut phone = browser_keys(&s, &c, &h, Mode::Device);
            let f0 = phone.send.seal(b"hello");
            let f1 = phone.send.seal(b"again");
            assert!(host.recv.open(&f1).is_none(), "out of order");
            assert_eq!(host.recv.open(&f0).unwrap(), b"hello");
            assert!(host.recv.open(&f0).is_none(), "replay");
            assert_eq!(host.recv.open(&f1).unwrap(), b"again");
            let back = host.send.seal(b"welcome");
            assert_eq!(phone.recv.open(&back).unwrap(), b"welcome");

            let mut tampered = phone.send.seal(b"x");
            *tampered.last_mut().unwrap() ^= 1;
            assert!(host.recv.open(&tampered).is_none());

            // Other secret, other mode: nothing opens.
            let mut wrong = browser_keys(&[8u8; 32], &c, &h, Mode::Device);
            assert!(host_keys(&s, &c, &h, Mode::Device).recv.open(&wrong.send.seal(b"x")).is_none());
            let mut pair = browser_keys(&s, &c, &h, Mode::Pair);
            assert!(host_keys(&s, &c, &h, Mode::Device).recv.open(&pair.send.seal(b"x")).is_none());
        }

        /// Pinned output, mirrored by the browser's test (mobile/src/web/crypto.test.ts).
        #[test]
        fn known_answer() {
            let (s, c, h) = ([7u8; 32], [1u8; 16], [2u8; 16]);
            let mut phone = browser_keys(&s, &c, &h, Mode::Pair);
            let f = phone.send.seal(b"{\"t\":\"ping\"}");
            assert_eq!(super::super::b64url(&f), KNOWN_FRAME, "update the browser test if this changes on purpose");
        }

        pub const KNOWN_FRAME: &str = "AAAAAAAAAABO6TjSdXFSiXF7k2BZrBtoGllhdWPwQkElkhpx";

        /// Code mode, from a fixed shared secret and a code typed with a dash and lower case:
        /// mirrored by the browser's check (normalising, joining and the `code` label).
        #[test]
        fn known_answer_code() {
            let secret = [&[7u8; 32][..], crate::web::normalize_code("ab3k-7q").as_bytes()].concat();
            let mut phone = browser_keys(&secret, &[1u8; 16], &[2u8; 16], Mode::Code);
            let f = phone.send.seal(b"{\"t\":\"ping\"}");
            assert_eq!(super::super::b64url(&f), KNOWN_CODE_FRAME, "update the browser check if this changes on purpose");
        }

        pub const KNOWN_CODE_FRAME: &str = "AAAAAAAAAADzgV6usRZ3cRk_v3g6W1E3C2ad5DH-i0PUJO2O";

        /// Both sides of a code pairing get the same secret only with the same code.
        #[test]
        fn code_keys_agree() {
            let (a, b) = (CodeKey::new(), CodeKey::new());
            let (pa, pb) = (a.public, b.public);
            assert_eq!(a.secret(&pb, "AB3K 7Q").unwrap(), b.secret(&pa, "ab3k7q").unwrap());
            let (a, b) = (CodeKey::new(), CodeKey::new());
            let (pa, pb) = (a.public, b.public);
            assert_ne!(a.secret(&pb, "AB3K7Q").unwrap(), b.secret(&pa, "AB3K7R").unwrap());
            assert!(CodeKey::new().secret(&[0u8; 32], "AB3K7Q").is_none(), "low-order key refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url() {
        for v in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0xfb, 0xff, 0xfe]] {
            assert_eq!(b64url_decode(&b64url(v)).unwrap(), v);
        }
        assert_eq!(b64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert!(b64url_decode("a").is_none());
        assert!(b64url_decode("a*b=").is_none());
    }

    #[test]
    fn handshake_shapes() {
        let h: Handshake = serde_json::from_str(r#"{"t":"open","device":null,"nonce":"AQID"}"#).unwrap();
        assert_eq!(h, Handshake::Open { device: None, nonce: "AQID".into(), key: None });
        assert_eq!(serde_json::to_string(&Handshake::Challenge { nonce: "x".into(), key: None }).unwrap(), r#"{"t":"challenge","nonce":"x"}"#);
        let h: Handshake = serde_json::from_str(r#"{"t":"open","device":null,"nonce":"AQID","key":"Ag"}"#).unwrap();
        assert_eq!(h, Handshake::Open { device: None, nonce: "AQID".into(), key: Some("Ag".into()) });
        assert_eq!(normalize_code(" ab3k-7q "), "AB3K7Q");
    }
}
