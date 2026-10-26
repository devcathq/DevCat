//! Browser access (protocol: devcat_remote_proto::web): a TCP listener on the same port number
//! as the app's UDP one. Serves the built mobile web app (embedded at build time) and upgrades
//! `/ws` to an encrypted session that runs the same command loop as the app.
use super::server::{Host, WireRx, WireTx};
use super::store::Device;
use devcat_remote_proto::web::{self as w, Handshake, Half, Mode};
use devcat_remote_proto::{ClientMsg, HostMsg};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

mod assets {
    include!(concat!(env!("OUT_DIR"), "/mobile_web.rs"));
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn serve(host: Arc<Host>, listener: TcpListener) {
    while let Ok((sock, peer)) = listener.accept().await {
        let host = host.clone();
        tokio::spawn(async move {
            let _ = handle(host, sock, peer.to_string()).await;
        });
    }
}

/// Read the request head without consuming it (the WebSocket handshake reads it again).
async fn peek_head(sock: &TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let n = tokio::time::timeout_at(deadline, sock.peek(&mut buf)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        if let Some(i) = buf[..n].windows(4).position(|x| x == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf[..i]).into_owned());
        }
        if n == buf.len() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn handle(host: Arc<Host>, mut sock: TcpStream, peer: String) -> std::io::Result<()> {
    let Some(head) = peek_head(&sock).await else { return Ok(()) };
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let upgrade = lines.any(|l| l.to_ascii_lowercase().starts_with("upgrade:") && l.to_ascii_lowercase().contains("websocket"));
    if path == w::WS_PATH && upgrade {
        if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
            session(host, ws, peer).await;
        }
        return Ok(());
    }
    // Plain HTTP: consume the request, answer, close.
    let mut drain = vec![0u8; head.len() + 4];
    let _ = sock.read_exact(&mut drain).await;
    let resp = respond(method, path);
    sock.write_all(&resp).await?;
    sock.shutdown().await
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn http(status: &str, ctype: &str, cache: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

const NOT_BUILT: &str = "<!doctype html><meta name=viewport content='width=device-width'><body style='font:16px -apple-system,sans-serif;background:#0f1114;color:#e6e8ec;padding:24px'><h2>DevCat</h2><p>The browser version of DevCat Mobile isn't included in this build of DevCat. Use the DevCat Mobile app, or build it with <code>pnpm mobile:web</code>.</p>";

pub fn has_assets() -> bool {
    !assets::FILES.is_empty()
}

fn respond(method: &str, path: &str) -> Vec<u8> {
    if method != "GET" && method != "HEAD" {
        return http("405 Method Not Allowed", "text/plain", "no-store", b"");
    }
    if assets::FILES.is_empty() {
        return http("200 OK", "text/html; charset=utf-8", "no-store", NOT_BUILT.as_bytes());
    }
    let find = |p: &str| assets::FILES.iter().find(|(f, _)| *f == p).map(|(_, b)| *b);
    if let Some(body) = find(path).filter(|_| path != "/") {
        let cache = if path.starts_with("/assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        return http("200 OK", content_type(path), cache, body);
    }
    // The app's own routes (and /) get the page itself; files that don't exist are 404s.
    let last = path.rsplit('/').next().unwrap_or("");
    match find("/index.html") {
        Some(index) if !last.contains('.') => http("200 OK", "text/html; charset=utf-8", "no-store", index),
        _ => http("404 Not Found", "text/plain", "no-store", b"not found"),
    }
}

type Ws = WebSocketStream<TcpStream>;

struct WebRx {
    stream: futures::stream::SplitStream<Ws>,
    keys: Half,
    stop: Arc<Notify>,
}

struct WebTx {
    sink: futures::stream::SplitSink<Ws, Message>,
    keys: Half,
}

#[async_trait::async_trait]
impl WireRx for WebRx {
    async fn recv(&mut self) -> Option<ClientMsg> {
        loop {
            let msg = tokio::select! {
                m = self.stream.next() => m?,
                _ = self.stop.notified() => return None,
            };
            match msg.ok()? {
                Message::Binary(b) => {
                    let plain = self.keys.open(&b)?;
                    return serde_json::from_slice(&plain).ok();
                }
                Message::Close(_) => return None,
                _ => continue,
            }
        }
    }
}

#[async_trait::async_trait]
impl WireTx for WebTx {
    async fn send(&mut self, m: &HostMsg) -> bool {
        let Ok(json) = serde_json::to_vec(m) else { return false };
        self.sink.send(Message::Binary(self.keys.seal(&json).into())).await.is_ok()
    }
}

async fn send_text(sink: &mut futures::stream::SplitSink<Ws, Message>, h: &Handshake) {
    if let Ok(t) = serde_json::to_string(h) {
        let _ = sink.send(Message::Text(t.into())).await;
    }
}

async fn session(host: Arc<Host>, ws: Ws, peer: String) {
    let (mut sink, mut stream) = ws.split();
    let deny = |reason: &str| Handshake::Denied { reason: reason.to_string() };
    // 1. open → challenge
    let open = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.next()).await.ok().flatten().and_then(Result::ok);
    let Some(Message::Text(t)) = open else { return };
    let Ok(Handshake::Open { device, nonce }) = serde_json::from_str::<Handshake>(&t) else { return };
    let Some(client_nonce) = w::b64url_decode(&nonce).filter(|n| n.len() == w::NONCE_LEN) else { return };
    let (mode, secret, known) = match device {
        Some(id) => match host.store.devices().into_iter().find(|d| d.id == id && d.kind == "web") {
            Some(d) => match d.secret.as_deref().and_then(w::b64url_decode) {
                Some(s) => (Mode::Device, s, Some(d)),
                None => return send_text(&mut sink, &deny("This browser's pairing is broken. Scan the QR code on the computer again.")).await,
            },
            None => return send_text(&mut sink, &deny("This browser isn't paired with the computer any more. Scan the QR code in DevCat → Mobile again.")).await,
        },
        None => match host.web_secret() {
            Some(s) => (Mode::Pair, s.to_vec(), None),
            None => return send_text(&mut sink, &deny("The pairing code expired or was used. On the computer click New code and scan again.")).await,
        },
    };
    let host_nonce: [u8; w::NONCE_LEN] = super::store::random_bytes();
    send_text(&mut sink, &Handshake::Challenge { nonce: w::b64url(&host_nonce) }).await;
    let w::HostKeys { send, recv } = w::host_keys(&secret, &client_nonce, &host_nonce, mode);
    let stop = Arc::new(Notify::new());
    let mut rx = WebRx { stream, keys: recv, stop: stop.clone() };
    let mut tx = WebTx { sink, keys: send };

    // 2. the first encrypted frame proves the secret
    let hello = tokio::time::timeout(HANDSHAKE_TIMEOUT, rx.recv()).await.ok().flatten();
    let Some(ClientMsg::Hello { name, platform, .. }) = hello else {
        if mode == Mode::Pair {
            host.pairing_attempt_failed();
        }
        return send_text(&mut tx.sink, &deny("Couldn't verify this link. Scan the QR code on the computer again.")).await;
    };
    let device = match known {
        Some(d) => d,
        None => {
            host.consume_pairing();
            let (name, platform) = Host::tidy(&name, &platform);
            let host_name = host_info_name(&host);
            if !tx.send(&HostMsg::Pairing { host_name }).await {
                return;
            }
            let secret: [u8; w::SECRET_LEN] = super::store::random_bytes();
            let id = format!("web-{}", hex::encode(super::store::random_bytes::<8>()));
            let new = Device { id: id.clone(), name, platform, access: String::new(), paired_at: 0, last_seen: None, kind: "web".into(), secret: Some(w::b64url(&secret)) };
            match host.approve_new(new, Some(peer.clone()), std::future::pending()).await {
                Ok(d) => {
                    if !tx.send(&HostMsg::Paired { device_id: id, secret: w::b64url(&secret) }).await {
                        return;
                    }
                    d
                }
                Err(reason) => {
                    let _ = tx.send(&HostMsg::Denied { reason }).await;
                    return;
                }
            }
        }
    };
    let closer = Box::new(move |_: &str| stop.notify_one()) as Box<dyn Fn(&str) + Send + Sync>;
    host.session(device, Some(peer), Box::new(rx), Box::new(tx), closer, None).await;
}

fn host_info_name(host: &Host) -> String {
    host.host_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::server::tests::{TestBackend, free_port, tmp_store};
    use parking_lot::Mutex;
    use serde_json::json;
    use tokio_tungstenite::connect_async;

    type Client = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

    /// A browser: open → challenge → keys → encrypted hello. Returns the socket and keys, or the
    /// denial text.
    async fn browser(port: u16, device: Option<&str>, secret: &[u8]) -> Result<(Client, w::BrowserKeys), String> {
        let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/ws")).await.unwrap();
        let nonce: [u8; 16] = crate::remote::store::random_bytes();
        let open = Handshake::Open { device: device.map(str::to_string), nonce: w::b64url(&nonce) };
        ws.send(Message::Text(serde_json::to_string(&open).unwrap().into())).await.unwrap();
        let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("no challenge") };
        let host_nonce = match serde_json::from_str::<Handshake>(&t).unwrap() {
            Handshake::Challenge { nonce } => w::b64url_decode(&nonce).unwrap(),
            Handshake::Denied { reason } => return Err(reason),
            h => panic!("{h:?}"),
        };
        let mode = if device.is_some() { Mode::Device } else { Mode::Pair };
        let mut keys = w::browser_keys(secret, &nonce, &host_nonce, mode);
        let hello = ClientMsg::Hello { name: "Safari on iPhone".into(), platform: "ios".into(), app_version: "web".into(), pair_code: None };
        ws.send(Message::Binary(keys.send.seal(&serde_json::to_vec(&hello).unwrap()).into())).await.unwrap();
        Ok((ws, keys))
    }

    async fn next_msg(ws: &mut Client, keys: &mut w::BrowserKeys) -> Result<HostMsg, String> {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.map_err(|_| "timeout".to_string())? {
                Some(Ok(Message::Binary(b))) => return Ok(serde_json::from_slice(&keys.recv.open(&b).ok_or("bad frame")?).unwrap()),
                Some(Ok(Message::Text(t))) => return Err(t.to_string()),
                Some(Ok(_)) => continue,
                _ => return Err("closed".into()),
            }
        }
    }

    #[tokio::test]
    async fn browser_pairs_calls_and_reconnects() {
        let backend = Arc::new(TestBackend { allow: true, events: Mutex::new(vec![]) });
        let host = Host::new(tmp_store(), backend);
        let port = free_port();
        // The TCP port might differ in use from the UDP one in tests; same number is the norm.
        host.start(port, true, None).await.unwrap();
        assert!(host.web_running(), "{:?}", host.web_error());

        // No pairing in progress: refused before any crypto.
        let err = browser(port, None, &[0u8; 32]).await.err().unwrap();
        assert!(err.contains("expired"), "{err}");

        let (_, secret, _) = host.pair_start();
        // Wrong secret: the hello doesn't decrypt.
        let (mut ws, mut k) = browser(port, None, &[9u8; 32]).await.unwrap();
        assert!(next_msg(&mut ws, &mut k).await.unwrap_err().contains("Couldn't verify"));

        // Right secret: pairing → approval → device secret → welcome.
        let (mut ws, mut k) = browser(port, None, &secret).await.unwrap();
        assert!(matches!(next_msg(&mut ws, &mut k).await.unwrap(), HostMsg::Pairing { .. }));
        let HostMsg::Paired { device_id, secret: dev_secret } = next_msg(&mut ws, &mut k).await.unwrap() else { panic!() };
        let HostMsg::Welcome { device, .. } = next_msg(&mut ws, &mut k).await.unwrap() else { panic!() };
        assert_eq!((device.id.as_str(), device.access.as_str()), (device_id.as_str(), "readonly"));
        assert!(host.web_secret().is_none(), "pairing used up");
        let stored = host.store.devices().into_iter().find(|d| d.id == device_id).unwrap();
        assert_eq!((stored.kind.as_str(), stored.secret.as_deref()), ("web", Some(dev_secret.as_str())));

        // Calls answer by id, encrypted both ways.
        let call = ClientMsg::Call { id: 3, cmd: "echo".into(), args: json!({ "q": 1 }) };
        ws.send(Message::Binary(k.send.seal(&serde_json::to_vec(&call).unwrap()).into())).await.unwrap();
        assert_eq!(next_msg(&mut ws, &mut k).await.unwrap(), HostMsg::Result { id: 3, ok: Some(json!({ "args": { "q": 1 }, "readonly": true })), err: None });
        // A replayed frame ends the session.
        let frame = k.send.seal(&serde_json::to_vec(&ClientMsg::Ping).unwrap());
        ws.send(Message::Binary(frame.clone().into())).await.unwrap();
        assert_eq!(next_msg(&mut ws, &mut k).await.unwrap(), HostMsg::Pong);
        ws.send(Message::Binary(frame.into())).await.unwrap();
        assert!(next_msg(&mut ws, &mut k).await.is_err(), "closed after a replay");

        // Reconnect with the device secret; a wrong one or an unknown device is refused.
        let raw = w::b64url_decode(&dev_secret).unwrap();
        let (mut ws2, mut k2) = browser(port, Some(&device_id), &raw).await.unwrap();
        assert!(matches!(next_msg(&mut ws2, &mut k2).await.unwrap(), HostMsg::Welcome { .. }));
        let (mut ws3, mut k3) = browser(port, Some(&device_id), &[1u8; 32]).await.unwrap();
        assert!(next_msg(&mut ws3, &mut k3).await.is_err());
        assert!(browser(port, Some("web-unknown"), &raw).await.err().unwrap().contains("isn't paired"));

        // Revoke drops the session.
        host.revoke(&device_id);
        assert!(next_msg(&mut ws2, &mut k2).await.is_err());
        host.stop().await;
    }

    #[test]
    fn static_routes() {
        let r = String::from_utf8_lossy(&respond("POST", "/")).into_owned();
        assert!(r.starts_with("HTTP/1.1 405"));
        let r = String::from_utf8_lossy(&respond("GET", "/")).into_owned();
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(r.contains("text/html"));
        if has_assets() {
            assert!(String::from_utf8_lossy(&respond("GET", "/nope.js")).starts_with("HTTP/1.1 404"));
            assert!(String::from_utf8_lossy(&respond("GET", "/agents")).starts_with("HTTP/1.1 200"));
        }
    }
}
