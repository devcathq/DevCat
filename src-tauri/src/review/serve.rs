//! The loopback file server that shows local `.html` files in a review webview:
//! `GET http://127.0.0.1:<port>/f/<reviewId>/<path>`, reading only the review's folder.
//!
//! To Tauri a page served from here is a remote origin, so it gets no IPC (a custom URI scheme
//! would be a local origin with every DevCat command). The random review id is the read grant;
//! there are no CORS or Private-Network headers, so other pages can't read through it, and the
//! `Host` header must be `127.0.0.1:<port>` (DNS rebinding). Hidden files and folders (a name
//! starting with `.`) and secret-looking files are never served, even inside the folder.

use super::{review_store, Target};
use crate::error::{AppError, AppResult};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Longest request head read (request line and headers).
const MAX_HEAD: usize = 16 * 1024;
/// The whole request head must arrive within this (not per read: a byte now and then doesn't
/// keep a connection open).
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// Most connections served at once; more are closed straight away.
const MAX_CONNECTIONS: usize = 64;

pub struct FileServer {
    port: u16,
}

static SERVER: OnceLock<FileServer> = OnceLock::new();

/// The file server, started on first use (a listener on `127.0.0.1:0` and a thread).
pub fn file_server() -> &'static FileServer {
    SERVER.get_or_init(|| match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => {
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            let slots = Slots::new(MAX_CONNECTIONS);
            std::thread::Builder::new()
                .name("review-files".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        // over the cap: dropped, which closes it
                        let Some(slot) = slots.take() else { continue };
                        std::thread::spawn(move || {
                            handle(stream, port);
                            drop(slot);
                        });
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

/// Counts open connections up to a cap.
struct Slots(Arc<AtomicUsize>, usize);

/// One open connection; frees its slot when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slots {
    fn new(max: usize) -> Self {
        Slots(Arc::new(AtomicUsize::new(0)), max)
    }

    fn take(&self) -> Option<Slot> {
        let max = self.1;
        self.0.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < max).then_some(n + 1)).ok()?;
        Some(Slot(self.0.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The file server's origin if it has been started (a file review exists only then).
pub fn running_origin() -> Option<String> {
    SERVER.get().map(FileServer::origin)
}

impl FileServer {
    /// Did it start listening?
    pub fn listening(&self) -> bool {
        self.port != 0
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Where the review's file `rel` (relative to its folder, `/`-separated) is served.
    pub fn url(&self, review: &str, rel: &str) -> String {
        format!("{}/f/{review}/{}", self.origin(), encode_path(rel))
    }
}

/// `rel` (`/`-separated) as it appears in the served URL's path.
pub fn encode_path(rel: &str) -> String {
    rel.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// Never served: a hidden file or folder (`.git`, `.env`, …) anywhere in `rel`, or a file that
/// looks like a key or a password store.
pub fn hidden_or_secret(rel: &Path) -> bool {
    rel.components().any(|c| matches!(c, Component::Normal(n) if n.to_string_lossy().starts_with('.')))
        || crate::integrations::text::secret_like(rel)
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
pub fn percent_decode(s: &str) -> Option<String> {
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

/// May `rel` (already percent-decoded, `/`-separated) be asked for at all? Only plain names
/// (no `..`, no absolute path, no backslash or line break), none hidden or secret-looking.
/// Checked by name only: where the file really is gets checked where it's read.
pub fn check_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains(['\\', '\0', '\n', '\r'])
        && !rel.starts_with('/')
        && rel.split('/').all(|c| !c.is_empty() && c != "..")
        && !hidden_or_secret(Path::new(rel))
}

/// `rel` (already percent-decoded) inside `root`, refusing anything that leads out of it:
/// `..`, absolute paths, and symlinks whose target is outside. Only existing files.
pub fn resolve_file(root: &Path, rel: &str) -> AppResult<PathBuf> {
    if !check_rel(rel) {
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
    // a link may lead to a hidden file in the folder: check where it really is too
    match path.strip_prefix(&root) {
        Ok(inside) if path.is_file() && !hidden_or_secret(inside) => Ok(path),
        _ => Err(refused()),
    }
}

/// What to answer to one request.
#[derive(Debug, PartialEq)]
enum Reply {
    File(PathBuf),
    /// A file on an SSH host: review id, the folder (`ssh://…`) and the checked `rel`.
    Remote(String, String, String),
    Status(u16),
}

/// Is this review folder on an SSH host (`ssh://…`)? Such a root is the URI, not a local path.
pub fn remote_root(root: &Path) -> Option<&str> {
    root.to_str().filter(|r| r.starts_with(crate::claude::remote::SCHEME))
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
    let Some(rel) = percent_decode(rel) else { return Reply::Status(404) };
    if let Some(remote) = remote_root(&root) {
        return if check_rel(&rel) { Reply::Remote(id.to_string(), remote.to_string(), rel) } else { Reply::Status(404) };
    }
    match resolve_file(&root, &rel) {
        Ok(file) => Reply::File(file),
        _ => Reply::Status(404),
    }
}

fn root_of(id: &str) -> Option<PathBuf> {
    match review_store().get(id)?.doc.target {
        Target::File { root, .. } => Some(root),
        Target::Url { .. } => None,
    }
}

/// What is left of a deadline, `None` once it has passed.
fn time_left(until: Instant, now: Instant) -> Option<Duration> {
    until.checked_duration_since(now).filter(|d| !d.is_zero())
}

/// Reads from the stream, each read limited to what is left of one overall deadline.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = time_left(self.until, Instant::now()).ok_or(std::io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

fn read_head(stream: &TcpStream, deadline: Duration) -> Option<String> {
    let timed = Deadline { stream, until: Instant::now() + deadline };
    let mut reader = BufReader::new(timed.take(MAX_HEAD as u64 + 2));
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
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let Some(head) = read_head(&stream, HEAD_DEADLINE) else { return };
    let reply = route(&head, port, root_of);
    let _ = match reply {
        Reply::File(path) => send_file(&mut stream, &path),
        Reply::Remote(id, root, rel) => match crate::rt::block_on(super::remote::fetch(&id, &root, &rel)) {
            Ok(super::remote::Answer::File(bytes)) => send_bytes(&mut stream, content_type(Path::new(&rel)), &bytes),
            Ok(super::remote::Answer::TooBig) => send_status(&mut stream, 413),
            Ok(super::remote::Answer::Missing) => send_status(&mut stream, 404),
            Err(_) => send_status(&mut stream, 502),
        },
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
        413 => "Too Large",
        502 => "The SSH host didn't send it",
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

fn send_bytes(stream: &mut TcpStream, content_type: &str, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(head(200, content_type, bytes.len() as u64).as_bytes())?;
    stream.write_all(bytes)
}

fn send_file(stream: &mut TcpStream, path: &Path) -> std::io::Result<()> {
    let Ok(mut file) = std::fs::File::open(path) else { return send_status(stream, 404) };
    let len = file.metadata()?.len();
    stream.write_all(head(200, content_type(path), len).as_bytes())?;
    std::io::copy(&mut (&mut file).take(len), stream).map(|_| ())
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
    fn routes_ssh_folders_to_the_host() {
        let roots = |id: &str| (id == "rv-a").then(|| PathBuf::from("ssh://devcat-x/~/site"));
        let h = "127.0.0.1:4000";
        let remote = |rel: &str| Reply::Remote("rv-a".into(), "ssh://devcat-x/~/site".into(), rel.into());
        assert_eq!(route(&get("/f/rv-a/index.html", h), 4000, roots), remote("index.html"));
        assert_eq!(route(&get("/f/rv-a/a/my%20page.css?v=1", h), 4000, roots), remote("a/my page.css"));
        assert_eq!(route(&get("/f/rv-a/index.html", "evil.test"), 4000, roots), Reply::Status(403));
        for path in ["/f/rv-a/../x", "/f/rv-a/%2e%2e/x", "/f/rv-a/a/%2E%2E/%2E%2E/x", "/f/rv-a/.env", "/f/rv-a/%2Fetc%2Fpasswd",
            "/f/rv-a/a%0Ab", "/f/rv-a/a//b", "/f/rv-a/id_rsa", "/f/rv-a/a%5Cb"] {
            assert_eq!(route(&get(path, h), 4000, roots), Reply::Status(404), "{path}");
        }
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

    #[test]
    fn hidden_and_secret_files_are_refused() {
        let d = tmp();
        let root = d.join("site");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "x").unwrap();
        std::fs::write(root.join(".env"), "x").unwrap();
        std::fs::write(root.join("server.pem"), "x").unwrap();
        std::fs::write(root.join("credentials.json"), "x").unwrap();
        std::fs::create_dir_all(root.join("a/.cache")).unwrap();
        std::fs::write(root.join("a/.cache/x.js"), "x").unwrap();
        for rel in [".git/config", ".env", "server.pem", "credentials.json", "a/.cache/x.js", "./.env"] {
            assert!(resolve_file(&root, rel).is_err(), "{rel}");
        }
        let r = root.clone();
        let roots = move |id: &str| (id == "rv-a").then(|| r.clone());
        let h = "127.0.0.1:4000";
        for path in ["/f/rv-a/.env", "/f/rv-a/.git/config", "/f/rv-a/%2Eenv", "/f/rv-a/server.pem"] {
            assert_eq!(route(&get(path, h), 4000, &roots), Reply::Status(404), "{path}");
        }
        #[cfg(unix)]
        {
            // a link inside the folder to a hidden file is refused too
            std::os::unix::fs::symlink(root.join(".env"), root.join("env.txt")).unwrap();
            assert!(resolve_file(&root, "env.txt").is_err());
        }
        assert!(resolve_file(&root, "index.html").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn head_deadline_is_overall() {
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(10);
        assert_eq!(time_left(until, t0), Some(Duration::from_secs(10)));
        assert_eq!(time_left(until, t0 + Duration::from_secs(4)), Some(Duration::from_secs(6)));
        assert_eq!(time_left(until, until), None);
        assert_eq!(time_left(until, until + Duration::from_secs(1)), None);
    }

    #[test]
    fn slow_head_is_cut_at_the_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            // one byte every 50 ms: each read is quick, the whole head never ends
            for _ in 0..40 {
                if c.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (stream, _) = listener.accept().unwrap();
        let t0 = Instant::now();
        assert!(read_head(&stream, Duration::from_millis(300)).is_none());
        assert!(t0.elapsed() < Duration::from_millis(1000), "{:?}", t0.elapsed());
        drop(stream);
        let _ = client.join();
    }

    #[test]
    fn open_connections_are_capped() {
        let slots = Slots::new(2);
        let a = slots.take().unwrap();
        let _b = slots.take().unwrap();
        assert!(slots.take().is_none());
        drop(a);
        assert!(slots.take().is_some());
    }
}
