//! The loopback file server that shows local `.html` files in a review webview:
//! `GET http://127.0.0.1:<port>/f/<reviewId>/<path>`, reading only the review's folder.
//!
//! To Tauri a page served from here is a remote origin, so it gets no IPC (a custom URI scheme
//! would be a local origin with every DevCat command). The random review id is the read grant;
//! there are no CORS or Private-Network headers, so other pages can't read through it, and the
//! `Host` header must be `127.0.0.1:<port>` (DNS rebinding).

#![allow(dead_code)] // Task 7 starts file reviews (`file_server().url`)

use super::{review_store, Target};
use crate::error::{AppError, AppResult};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// Longest request head read (request line and headers).
const MAX_HEAD: usize = 16 * 1024;

pub struct FileServer {
    port: u16,
}

static SERVER: OnceLock<FileServer> = OnceLock::new();

/// The file server, started on first use (a listener on `127.0.0.1:0` and a thread).
pub fn file_server() -> &'static FileServer {
    SERVER.get_or_init(|| match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => {
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            std::thread::Builder::new()
                .name("review-files".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        std::thread::spawn(move || handle(stream, port));
                    }
                })
                .ok();
            FileServer { port }
        }
        Err(e) => {
            eprintln!("review file server: can't listen: {e}");
            FileServer { port: 0 }
        }
    })
}

/// The file server's origin if it has been started (a file review exists only then).
pub fn running_origin() -> Option<String> {
    SERVER.get().map(FileServer::origin)
}

impl FileServer {
    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Where the review's file `rel` (relative to its folder, `/`-separated) is served.
    pub fn url(&self, review: &str, rel: &str) -> String {
        let path: Vec<String> = rel.split('/').map(encode_segment).collect();
        format!("{}/f/{review}/{}", self.origin(), path.join("/"))
    }
}

fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `%XX` decoded; `None` when malformed or not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn refused() -> AppError {
    AppError::Msg("That file can't be shown".into())
}

/// `rel` (already percent-decoded) inside `root`, refusing anything that leads out of it:
/// `..`, absolute paths, and symlinks whose target is outside. Only existing files.
pub fn resolve_file(root: &Path, rel: &str) -> AppResult<PathBuf> {
    if rel.is_empty() || rel.contains(['\\', '\0']) {
        return Err(refused());
    }
    // Windows: `name:stream` is an alternate data stream, `C:` a drive
    if cfg!(windows) && rel.contains(':') {
        return Err(refused());
    }
    let rel_path = Path::new(rel);
    if !rel_path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(refused());
    }
    let root = root.canonicalize().map_err(|_| refused())?;
    let path = root.join(rel_path).canonicalize().map_err(|_| refused())?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(refused());
    }
    Ok(path)
}

/// What to answer to one request.
#[derive(Debug, PartialEq)]
enum Reply {
    File(PathBuf),
    Status(u16),
}

/// The answer to a request head: the `Host` check first, then the method, then the path.
fn route(head: &str, port: u16, root_of: impl Fn(&str) -> Option<PathBuf>) -> Reply {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let hosts: Vec<&str> = lines
        .filter_map(|l| l.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .collect();
    if hosts != [format!("127.0.0.1:{port}").as_str()] {
        return Reply::Status(403);
    }
    if method != "GET" {
        return Reply::Status(405);
    }
    let path = target.split(['?', '#']).next().unwrap_or("");
    let Some((id, rel)) = path.strip_prefix("/f/").and_then(|p| p.split_once('/')) else {
        return Reply::Status(404);
    };
    let Some(root) = (!id.is_empty()).then(|| root_of(id)).flatten() else {
        return Reply::Status(404);
    };
    match percent_decode(rel).map(|rel| resolve_file(&root, &rel)) {
        Some(Ok(file)) => Reply::File(file),
        _ => Reply::Status(404),
    }
}

fn root_of(id: &str) -> Option<PathBuf> {
    match review_store().get(id)?.doc.target {
        Target::File { root, .. } => Some(root),
        Target::Url { .. } => None,
    }
}

fn read_head(stream: &TcpStream) -> Option<String> {
    let mut reader = BufReader::new(std::io::Read::take(stream, MAX_HEAD as u64 + 2));
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || head.len() + line.len() > MAX_HEAD {
            return None;
        }
        let end = line == "\r\n" || line == "\n";
        head.push_str(line.trim_end_matches(['\r', '\n']));
        if end {
            return Some(head);
        }
        head.push_str("\r\n");
    }
}

fn handle(mut stream: TcpStream, port: u16) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let Some(head) = read_head(&stream) else { return };
    let reply = route(&head, port, root_of);
    let _ = match reply {
        Reply::File(path) => send_file(&mut stream, &path),
        Reply::Status(code) => send_status(&mut stream, code),
    };
    let _ = stream.flush();
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

/// The response head. No CORS or Private-Network headers, ever.
fn head(code: u16, content_type: &str, len: u64) -> String {
    let allow = if code == 405 { "Allow: GET\r\n" } else { "" };
    format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\n{allow}\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        reason(code)
    )
}

fn send_status(stream: &mut TcpStream, code: u16) -> std::io::Result<()> {
    let body = reason(code);
    stream.write_all(head(code, "text/plain; charset=utf-8", body.len() as u64).as_bytes())?;
    stream.write_all(body.as_bytes())
}

fn send_file(stream: &mut TcpStream, path: &Path) -> std::io::Result<()> {
    let Ok(mut file) = std::fs::File::open(path) else { return send_status(stream, 404) };
    let len = file.metadata()?.len();
    stream.write_all(head(200, content_type(path), len).as_bytes())?;
    std::io::copy(&mut std::io::Read::take(&mut file, len), stream).map(|_| ())
}

fn content_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("site/assets")).unwrap();
        std::fs::write(d.join("site/index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(d.join("site/assets/app.css"), "h1{}").unwrap();
        std::fs::write(d.join("secret"), "s").unwrap();
        d
    }

    #[test]
    fn resolve_file_stays_in_root() {
        let d = tmp();
        let root = d.join("site");
        assert!(resolve_file(&root, "../secret").is_err());
        assert!(resolve_file(&root, "/etc/passwd").is_err());
        assert!(resolve_file(&root, "").is_err());
        assert!(resolve_file(&root, "%2e%2e/secret").is_err());
        assert!(resolve_file(&root, "assets/../../secret").is_err());
        assert!(resolve_file(&root, "assets").is_err(), "a folder isn't a file");
        assert!(resolve_file(&root, "nope.html").is_err());
        assert_eq!(resolve_file(&root, "assets/app.css").unwrap(), root.canonicalize().unwrap().join("assets/app.css"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_file_refuses_symlinks_out() {
        let d = tmp();
        let root = d.join("site");
        std::os::unix::fs::symlink(d.join("secret"), root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("index.html"), root.join("in")).unwrap();
        assert!(resolve_file(&root, "out").is_err());
        assert!(resolve_file(&root, "in").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn get(path: &str, host: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: t\r\n\r\n")
    }

    #[test]
    fn routes_requests() {
        let d = tmp();
        let root = d.join("site");
        let r = root.clone();
        let roots = move |id: &str| (id == "rv-a").then(|| r.clone());
        let h = "127.0.0.1:4000";
        assert_eq!(route(&get("/f/rv-a/index.html", "evil.test"), 4000, &roots), Reply::Status(403));
        assert_eq!(route(&get("/f/rv-a/index.html", "127.0.0.1:4001"), 4000, &roots), Reply::Status(403));
        assert_eq!(route(&get("/f/rv-a/index.html", "localhost:4000"), 4000, &roots), Reply::Status(403));
        let no_host = "GET /f/rv-a/index.html HTTP/1.1\r\n\r\n";
        assert_eq!(route(no_host, 4000, &roots), Reply::Status(403));
        let two = format!("GET /f/rv-a/index.html HTTP/1.1\r\nHost: {h}\r\nHost: evil.test\r\n\r\n");
        assert_eq!(route(&two, 4000, &roots), Reply::Status(403));
        let post = format!("POST /f/rv-a/index.html HTTP/1.1\r\nHost: {h}\r\n\r\n");
        assert_eq!(route(&post, 4000, &roots), Reply::Status(405));
        assert_eq!(route(&get("/f/rv-b/index.html", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/f/rv-a/", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/f/rv-a", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/m", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/f/rv-a/%2e%2e/secret", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/f/rv-a/../secret", h), 4000, &roots), Reply::Status(404));
        assert_eq!(route(&get("/f/rv-a/%2Fetc%2Fpasswd", h), 4000, &roots), Reply::Status(404));
        let canon = root.canonicalize().unwrap();
        assert_eq!(route(&get("/f/rv-a/index.html?x=1#y", h), 4000, &roots), Reply::File(canon.join("index.html")));
        assert_eq!(route(&get("/f/rv-a/assets/app%2Ecss", h), 4000, &roots), Reply::File(canon.join("assets/app.css")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn urls_are_encoded() {
        let s = FileServer { port: 4000 };
        assert_eq!(s.origin(), "http://127.0.0.1:4000");
        assert_eq!(s.url("rv-a", "docs/my page#1.html"), "http://127.0.0.1:4000/f/rv-a/docs/my%20page%231.html");
    }

    fn fetch(host: &str, path: &str) -> String {
        let port = file_server().port;
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(get(path, host).as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn serves_over_loopback() {
        let d = tmp();
        let r = review_store().start(
            Target::File { root: d.join("site"), rel: "index.html".into() },
            "index.html".into(),
            None,
            "w".into(),
        );
        let host = file_server().origin().trim_start_matches("http://").to_string();
        let ok = fetch(&host, &format!("/f/{}/index.html", r.id));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("Content-Type: text/html; charset=utf-8\r\n"), "{ok}");
        assert!(ok.contains("Cache-Control: no-store\r\n"), "{ok}");
        assert!(ok.ends_with("<h1>hi</h1>"), "{ok}");
        assert!(!ok.to_ascii_lowercase().contains("access-control-"), "{ok}");
        assert!(fetch("evil.test", &format!("/f/{}/index.html", r.id)).starts_with("HTTP/1.1 403"));
        let unknown = fetch(&host, "/f/rv-unknown/index.html");
        assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
        assert!(!unknown.to_ascii_lowercase().contains("access-control-"));
        review_store().close(&r.id);
        assert!(fetch(&host, &format!("/f/{}/index.html", r.id)).starts_with("HTTP/1.1 404"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
