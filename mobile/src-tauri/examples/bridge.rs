//! Dev only: serve the phone-side commands over HTTP so the mobile UI runs in a normal browser
//! (`pnpm mobile:bridge`, port 1442) for UI testing. The networking is the real one: it pairs
//! with and talks to a real DevCat (or `cargo run --example remotedemo` in src-tauri).
//!   POST /invoke {cmd, args} → {ok} | {err}
//!   GET  /events             → text/event-stream of {event, payload}
//! `cargo run --example bridge -- [port] [data dir]`. Binds to localhost only.
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    }
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(1443);
    let dir = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("devcat-mobile-bridge"));
    let (tx, _) = broadcast::channel::<String>(4096);
    let t = tx.clone();
    let sink: devcat_mobile_lib::client::Sink = Arc::new(move |event, payload| {
        let _ = t.send(json!({ "event": event, "payload": payload }).to_string());
    });
    let app = Arc::new(devcat_mobile_lib::headless(dir, "Browser test phone", sink).expect("start"));
    let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
    eprintln!("mobile bridge on http://127.0.0.1:{port}");
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let (app, tx) = (app.clone(), tx.clone());
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 64 * 1024];
            let (head_end, len) = loop {
                let n = match sock.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                buf.extend_from_slice(&tmp[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                    let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0usize);
                    break (i + 4, len);
                }
            };
            let first = String::from_utf8_lossy(&buf[..head_end]).lines().next().unwrap_or("").to_string();
            const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n";
            if first.starts_with("GET /events") {
                let _ = sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n{CORS}\r\n").as_bytes()).await;
                let mut rx = tx.subscribe();
                while let Ok(m) = rx.recv().await {
                    if sock.write_all(format!("data: {m}\n\n").as_bytes()).await.is_err() {
                        return;
                    }
                }
                return;
            }
            while buf.len() < head_end + len {
                let n = match sock.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = if first.starts_with("OPTIONS") {
                String::new()
            } else {
                let req: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
                let cmd = req["cmd"].as_str().unwrap_or("").to_string();
                let args = req.get("args").cloned().unwrap_or(json!({}));
                match devcat_mobile_lib::bridge_call(&app, "dev", &cmd, args).await {
                    Ok(v) => json!({ "ok": v }).to_string(),
                    Err(e) => json!({ "err": e }).to_string(),
                }
            };
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{CORS}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}
