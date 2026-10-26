//! DevCat remote access: the wire protocol between DevCat on a computer (the host) and the
//! DevCat mobile app (the client). Shared by both so they can't drift apart.
//!
//! Transport: one iroh (QUIC) connection per phone, ALPN [`ALPN`]. iroh tries the host's
//! addresses directly and, when the host has a relay ([`PairLink::relay`]), also goes through
//! the relay and punches a direct path from there (other networks). Relays only forward
//! end-to-end encrypted packets. The client opens a single
//! bidirectional stream and both sides exchange length-prefixed JSON frames on it:
//!   u32 big-endian length, then that many bytes of JSON ([`ClientMsg`] / [`HostMsg`]).
//!
//! Session:
//!   client → `hello` (with `pairCode` when pairing for the first time)
//!   host   → `pairing` (waiting for the user to approve on the computer; pairing only)
//!   host   → `welcome` | `denied`
//!   then any number of `call` → `result` (answered out of order) and `event`s from the host;
//!   `ping` → `pong` keeps idle connections (and NAT mappings) alive.
//!
//! Browsers (DevCat Mobile served as a web page by the host, see [`web`]) use the same
//! messages over an encrypted WebSocket on the same port number (TCP instead of UDP).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// QUIC ALPN of this protocol version. Bump the suffix for incompatible changes.
pub const ALPN: &[u8] = b"devcat/remote/1";

/// Largest frame either side accepts (query results can be big; terminal output is batched).
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    Hello {
        /// Shown on the computer ("Arif's iPhone").
        name: String,
        /// ios | android | macos | windows | linux
        platform: String,
        app_version: String,
        /// One-time code from the pairing QR; only on the first connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pair_code: Option<String>,
    },
    Call {
        id: u64,
        cmd: String,
        #[serde(default)]
        args: Value,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum HostMsg {
    /// Pairing code accepted; the user must still approve the phone on the computer.
    Pairing { host_name: String },
    Welcome { host: HostInfo, device: DeviceInfo },
    Denied { reason: String },
    Result {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<Value>,
    },
    Event { event: String, payload: Value },
    Pong,
    /// Browser pairing only: the device's id and long-term secret (base64url), sent once,
    /// encrypted, right before `welcome`. The browser keeps them to reconnect.
    Paired { device_id: String, secret: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    /// The host's iroh endpoint id (public key, hex).
    pub id: String,
    pub name: String,
    /// macos | windows | linux
    pub os: String,
    pub app_version: String,
    /// Where the host listens right now ("ip:port"), so phones follow address changes.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// The relay the host uses: [`RELAY_PUBLIC`] or a relay URL; none when relays are off.
    #[serde(default)]
    pub relay: Option<String>,
    /// The relay server the host is connected to right now (URL), so phones go straight there
    /// instead of looking it up through iroh's DNS first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

/// How a connection travels right now: straight between the two devices, or through a relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnPath {
    pub direct: bool,
    /// Round-trip time, milliseconds.
    pub rtt_ms: u64,
    /// The relay server (URL) when not direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

/// [`PairLink::relay`] / [`HostInfo::relay`] value for iroh's public relays (the host's current
/// relay is then found through iroh's DNS lookup).
pub const RELAY_PUBLIC: &str = "public";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// The phone's iroh endpoint id as the host knows it.
    pub id: String,
    pub name: String,
    /// "full" | "readonly"
    pub access: String,
}

#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    TooLarge(usize),
    Json(serde_json::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "connection error: {e}"),
            FrameError::TooLarge(n) => write!(f, "message too large ({n} bytes)"),
            FrameError::Json(e) => write!(f, "bad message: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(e: std::io::Error) -> Self {
        FrameError::Io(e)
    }
}

/// Write one frame. Not cancel-safe: use from a single writer task.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let body = serde_json::to_vec(msg).map_err(FrameError::Json)?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame; `Ok(None)` on a clean end of stream.
pub async fn read_frame<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<Option<T>, FrameError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map(Some).map_err(FrameError::Json)
}

pub mod web;

/// Port DevCat listens on for phones unless the user picks another one.
pub const DEFAULT_PORT: u16 = 47100;

/// What the pairing QR on the computer encodes:
/// `devcat://pair?v=1&id=<endpoint id>&code=<one-time code>&name=<computer name>&addr=<ip:port>[&addr=…]`
#[derive(Debug, Clone, PartialEq)]
pub struct PairLink {
    pub id: String,
    pub code: String,
    pub name: String,
    /// Addresses to try ("ip:port" or "host:port"), most likely first.
    pub addrs: Vec<String>,
    /// The host's relay ([`RELAY_PUBLIC`] or a relay URL), for reaching it from other networks.
    pub relay: Option<String>,
    /// The relay server the host is connected to right now ([`HostInfo::home`]).
    pub home: Option<String>,
}

pub const PAIR_SCHEME: &str = "devcat://pair";

impl PairLink {
    pub fn to_url(&self) -> String {
        let mut s = format!("{PAIR_SCHEME}?v=1&id={}&code={}&name={}", enc(&self.id), enc(&self.code), enc(&self.name));
        for a in &self.addrs {
            s.push_str(&format!("&addr={}", enc(a)));
        }
        if let Some(r) = &self.relay {
            s.push_str(&format!("&relay={}", enc(r)));
        }
        if let Some(h) = self.relay.as_ref().and(self.home.as_ref()) {
            s.push_str(&format!("&home={}", enc(h)));
        }
        s
    }

    /// The link for the hosted browser version (`page`, e.g. `https://…github.io/DevCat/`), which
    /// connects through the relay: `<page>#v=1&id=…&code=…&name=…&relay=…[&addr=…]`.
    pub fn to_hosted_url(&self, page: &str) -> String {
        let mut s = format!("{page}#v=1&id={}&code={}&name={}", enc(&self.id), enc(&self.code), enc(&self.name));
        if let Some(r) = &self.relay {
            s.push_str(&format!("&relay={}", enc(r)));
        }
        if let Some(h) = self.relay.as_ref().and(self.home.as_ref()) {
            s.push_str(&format!("&home={}", enc(h)));
        }
        for a in &self.addrs {
            s.push_str(&format!("&addr={}", enc(a)));
        }
        s
    }

    /// The browser version of the link: `http://<addr>/#v=1&id=…&code=…&s=<web secret>&name=…[&addr=…]`.
    /// Everything after `#` stays on the phone (browsers never send it over the network).
    pub fn to_web_url(&self, web_secret: &str) -> String {
        let host = self.addrs.first().cloned().unwrap_or_default();
        let mut s = format!("http://{host}/#v=1&id={}&code={}&s={}&name={}", enc(&self.id), enc(&self.code), enc(web_secret), enc(&self.name));
        for a in self.addrs.iter().skip(1) {
            s.push_str(&format!("&addr={}", enc(a)));
        }
        if let Some(r) = &self.relay {
            s.push_str(&format!("&relay={}", enc(r)));
        }
        if let Some(h) = self.relay.as_ref().and(self.home.as_ref()) {
            s.push_str(&format!("&home={}", enc(h)));
        }
        s
    }

    /// Accepts the link with surrounding whitespace (pasted text): `devcat://pair?…`, or the
    /// browser link `http://host:port/#…` (its host:port becomes the first address), or the hosted
    /// browser link `https://…#…`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (query, mut addrs) = if let Some(q) = s.strip_prefix(PAIR_SCHEME).and_then(|q| q.strip_prefix('?')) {
            (q, Vec::new())
        } else if let Some((rest, served_by_host)) = s.strip_prefix("http://").map(|r| (r, true)).or_else(|| s.strip_prefix("https://").map(|r| (r, false))) {
            let (host, frag) = rest.split_once('#').ok_or("This link has no pairing details (nothing after #)")?;
            // DevCat serves its page over plain HTTP; an https page is the hosted one.
            let host = host.trim_end_matches('/').to_string();
            (frag, if !served_by_host || host.is_empty() || host.contains('/') { vec![] } else { vec![host] })
        } else {
            return Err("Not a DevCat pairing link (it starts with devcat://pair or http://)".to_string());
        };
        let (mut id, mut code, mut name, mut relay, mut home) = (None, None, None, None, None);
        for part in query.split('&') {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let v = dec(v);
            match k {
                "id" => id = Some(v),
                "code" => code = Some(v),
                "name" => name = Some(v),
                "relay" if !v.is_empty() => relay = Some(v),
                "home" if !v.is_empty() => home = Some(v),
                "addr" if !v.is_empty() && !addrs.contains(&v) => addrs.push(v),
                _ => {}
            }
        }
        let id = id.filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("The pairing link has no valid computer id")?;
        let code = code.filter(|v| !v.is_empty()).ok_or("The pairing link has no pairing code")?;
        if addrs.is_empty() && relay.is_none() {
            return Err("The pairing link has no address".into());
        }
        Ok(PairLink { id, code, name: name.unwrap_or_else(|| "DevCat".into()), addrs, home: relay.as_ref().and(home), relay })
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
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_shapes() {
        let hello = ClientMsg::Hello { name: "Phone".into(), platform: "ios".into(), app_version: "1".into(), pair_code: None };
        assert_eq!(serde_json::to_value(&hello).unwrap(), json!({ "t": "hello", "name": "Phone", "platform": "ios", "appVersion": "1" }));
        let call: ClientMsg = serde_json::from_value(json!({ "t": "call", "id": 7, "cmd": "net_status" })).unwrap();
        assert_eq!(call, ClientMsg::Call { id: 7, cmd: "net_status".into(), args: Value::Null });
        let r = HostMsg::Result { id: 7, ok: Some(json!([1])), err: None };
        assert_eq!(serde_json::to_value(&r).unwrap(), json!({ "t": "result", "id": 7, "ok": [1] }));
        assert_eq!(serde_json::to_value(HostMsg::Pairing { host_name: "Mac".into() }).unwrap(), json!({ "t": "pairing", "hostName": "Mac" }));
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &HostMsg::Pong).await.unwrap();
        write_frame(&mut a, &ClientMsg::Ping).await.unwrap();
        let m: Option<HostMsg> = read_frame(&mut b).await.unwrap();
        assert_eq!(m, Some(HostMsg::Pong));
        let m: Option<ClientMsg> = read_frame(&mut b).await.unwrap();
        assert_eq!(m, Some(ClientMsg::Ping));
    }

    #[tokio::test]
    async fn frames_end_and_limits() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let big = ClientMsg::Call { id: 1, cmd: "x".into(), args: json!("y".repeat(10_000)) };
        let w = tokio::spawn(async move {
            write_frame(&mut a, &big).await.unwrap();
        });
        let m: Option<ClientMsg> = read_frame(&mut b).await.unwrap();
        w.await.unwrap();
        assert!(matches!(m, Some(ClientMsg::Call { id: 1, .. })));
        let m: Option<ClientMsg> = read_frame(&mut b).await.unwrap();
        assert!(m.is_none(), "clean EOF");

        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(u32::MAX).to_be_bytes()).await.unwrap();
        assert!(matches!(read_frame::<_, ClientMsg>(&mut b).await, Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn pair_link_round_trip() {
        let l = PairLink {
            id: "ab".repeat(32),
            code: "K7Q2-X9".into(),
            name: "Arif's MacBook Pro".into(),
            addrs: vec!["192.168.1.20:47100".into(), "[fe80::1]:47100".into()],
            relay: None,
            home: None,
        };
        let url = l.to_url();
        assert!(url.starts_with("devcat://pair?v=1&id=abab"), "{url}");
        assert!(url.contains("name=Arif%27s%20MacBook%20Pro"), "{url}");
        assert!(url.contains("addr=192.168.1.20%3A47100"), "{url}");
        assert_eq!(PairLink::parse(&format!("  {url}\n")).unwrap(), l);
        let no_addr = PairLink { addrs: vec![], ..l.clone() };
        assert!(PairLink::parse(&no_addr.to_url()).is_err());
        // The browser link carries the same, with the first address as its host.
        let web = l.to_web_url("c2VjcmV0");
        assert!(web.starts_with("http://192.168.1.20:47100/#v=1&id=abab"), "{web}");
        assert!(web.contains("&s=c2VjcmV0&"), "{web}");
        assert_eq!(PairLink::parse(&web).unwrap(), l);
        assert_eq!(web::fragment_param(web.split_once('#').unwrap().1, "s").as_deref(), Some("c2VjcmV0"));

        // With a relay, an address isn't needed; the hosted page's host isn't an address.
        let relayed = PairLink { addrs: vec![], relay: Some(RELAY_PUBLIC.into()), ..l.clone() };
        assert_eq!(PairLink::parse(&relayed.to_url()).unwrap(), relayed);
        let hosted = relayed.to_hosted_url("https://example.github.io/DevCat/");
        assert!(hosted.starts_with("https://example.github.io/DevCat/#v=1&id=abab"), "{hosted}");
        assert_eq!(PairLink::parse(&hosted).unwrap(), relayed);
        let custom = PairLink { relay: Some("https://relay.example.com./".into()), ..l.clone() };
        assert_eq!(PairLink::parse(&custom.to_hosted_url("https://x.dev/")).unwrap(), custom);

        // The host's current relay server rides along with every link, but only with a relay.
        let homed = PairLink { home: Some("https://euc1-1.relay.n0.iroh.link./".into()), ..relayed.clone() };
        for url in [homed.to_url(), homed.to_hosted_url("https://x.dev/"), homed.to_web_url("c2VjcmV0")] {
            assert!(url.contains("&home=https%3A%2F%2Feuc1-1.relay.n0.iroh.link.%2F"), "{url}");
            assert_eq!(PairLink::parse(&url).unwrap().home, homed.home, "{url}");
        }
        let stray = PairLink { home: Some("https://r.example/".into()), ..l.clone() };
        assert!(!stray.to_url().contains("home="));
        assert_eq!(PairLink::parse(&format!("{}&home=https%3A%2F%2Fr.example%2F", l.to_url())).unwrap().home, None);
    }

    #[test]
    fn pair_link_rejects_garbage() {
        assert!(PairLink::parse("https://example.com").is_err());
        assert!(PairLink::parse("devcat://pair?v=1&id=xyz&code=1").is_err());
        assert!(PairLink::parse(&format!("devcat://pair?v=1&id={}", "a".repeat(64))).is_err());
        assert_eq!(dec("a%2"), "a%2");
        assert_eq!(dec("%zz"), "%zz");
    }
}
