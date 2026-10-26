//! The hosted browser version of DevCat Mobile reaching DevCat on a computer from any network.
//! Browsers can't send UDP, so this iroh endpoint goes through the computer's relay (end-to-end
//! encrypted; the relay only forwards packets), and speaks the same protocol as the app
//! (crates/remote-proto): JS sends and receives the JSON messages, this side frames them.
use devcat_remote_proto::{ALPN, RELAY_PUBLIC, read_frame, write_frame};
use iroh::address_lookup::PkarrResolver;
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use serde_json::Value;
use std::rc::Rc;
use tokio::sync::Mutex;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// This browser's endpoint.
#[wasm_bindgen]
pub struct Node {
    ep: Endpoint,
}

#[wasm_bindgen]
impl Node {
    /// `secret_hex`: this browser's key (32 random bytes, kept by the page). `relay`: the
    /// computer's relay, `public` (iroh's relays, the computer found through iroh's DNS) or a URL.
    pub async fn create(secret_hex: String, relay: String) -> Result<Node, JsValue> {
        let bytes: [u8; 32] = hex::decode(secret_hex.trim()).map_err(err)?.try_into().map_err(|_| err("The browser key is damaged"))?;
        let builder = if relay == RELAY_PUBLIC {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Default).address_lookup(PkarrResolver::n0_dns())
        } else {
            let url: RelayUrl = relay.parse().map_err(|_| err(format!("Not a relay address: {relay}")))?;
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom([url]))
        };
        let ep = builder.secret_key(SecretKey::from_bytes(&bytes)).bind().await.map_err(|e| err(format!("Couldn't start networking: {e}")))?;
        Ok(Node { ep })
    }

    /// This browser's id (its public key), which the computer lists as a paired device.
    pub fn id(&self) -> String {
        self.ep.id().to_string()
    }

    /// Connect to the computer `host_id` (hex) through `relay` (as in [`Node::create`]); `home`:
    /// the relay server it was last seen on, tried before iroh's DNS lookup. Resolves to a [`Pipe`].
    pub fn open(&self, host_id: String, relay: String, home: Option<String>) -> js_sys::Promise {
        let ep = self.ep.clone();
        future_to_promise(async move {
            let id = host_id.parse().map_err(|_| err("Invalid computer id"))?;
            let mut addr = EndpointAddr::new(id);
            if relay != RELAY_PUBLIC {
                addr = addr.with_relay_url(relay.parse::<RelayUrl>().map_err(err)?);
            }
            if let Some(url) = home.and_then(|h| h.parse::<RelayUrl>().ok()) {
                addr = addr.with_relay_url(url);
            }
            let conn = ep.connect(addr, ALPN).await.map_err(err)?;
            let (send, recv) = conn.open_bi().await.map_err(err)?;
            Ok(Pipe { conn, send: Rc::new(Mutex::new(send)), recv: Rc::new(Mutex::new(recv)) }.into())
        })
    }

    pub async fn close(self) {
        self.ep.close().await;
    }
}

/// One connection to a computer: JSON messages in and out.
#[wasm_bindgen]
pub struct Pipe {
    conn: Connection,
    send: Rc<Mutex<SendStream>>,
    recv: Rc<Mutex<RecvStream>>,
}

#[wasm_bindgen]
impl Pipe {
    /// Send one message (a JSON text, e.g. `{"t":"call",…}`).
    pub fn send(&self, json: String) -> js_sys::Promise {
        let send = self.send.clone();
        future_to_promise(async move {
            let msg: Value = serde_json::from_str(&json).map_err(err)?;
            write_frame(&mut *send.lock().await, &msg).await.map_err(err)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The next message as JSON text, or null when the computer closed the connection.
    pub fn recv(&self) -> js_sys::Promise {
        let recv = self.recv.clone();
        future_to_promise(async move {
            let m: Option<Value> = read_frame(&mut *recv.lock().await).await.map_err(err)?;
            Ok(m.map(|v| JsValue::from_str(&v.to_string())).unwrap_or(JsValue::NULL))
        })
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}
