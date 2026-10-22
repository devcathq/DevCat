//! Dev-only HTTP bridge (debug builds): serves a subset of the Tauri commands over
//! `POST http://127.0.0.1:<port>/invoke {cmd, args}` so the frontend can run in a normal
//! browser (`pnpm dev:bridge`) for UI testing with browser automation.
//! Start it with `cargo run --example devbridge`. Binds to localhost only.
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn arg<T: serde::de::DeserializeOwned>(args: &Value, name: &str) -> Result<T, Value> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null)).map_err(|e| json!(format!("bad arg {name}: {e}")))
}


async fn call(cmd: &str, args: &Value) -> Result<Value, Value> {
    use crate::claude::{files, git};
    use crate::gitx::commands as gx;
    // Tuple destructuring by position: each command's args are listed in order.
    macro_rules! c {
        ($f:path) => { $f().await };
        ($f:path, $($a:literal),+) => {{
            let r = (|| -> Result<_, Value> { Ok(($(arg(args, $a)?,)+)) })();
            match r { Ok(t) => c!(@apply $f, t, $($a),+), Err(e) => return Err(e) }
        }};
        (@apply $f:path, $t:ident, $a0:literal) => { $f($t.0).await };
        (@apply $f:path, $t:ident, $a0:literal, $a1:literal) => { $f($t.0, $t.1).await };
        (@apply $f:path, $t:ident, $a0:literal, $a1:literal, $a2:literal) => { $f($t.0, $t.1, $t.2).await };
        (@apply $f:path, $t:ident, $a0:literal, $a1:literal, $a2:literal, $a3:literal) => { $f($t.0, $t.1, $t.2, $t.3).await };
    }
    fn out<T: serde::Serialize>(r: crate::error::AppResult<T>) -> Result<Value, Value> {
        r.map(|v| serde_json::to_value(v).unwrap_or(Value::Null)).map_err(|e| serde_json::to_value(e).unwrap_or_else(|_| json!("error")))
    }
    match cmd {
        "remote_qr" => out(c!(crate::remote::remote_qr, "text")),
        "git_repos" => out(c!(gx::git_repos, "root", "maxDepth")),
        "git_log" => out(c!(gx::git_log, "cwd", "query")),
        "git_show" => out(c!(gx::git_show, "cwd", "sha")),
        "git_commit_file_versions" => out(c!(gx::git_commit_file_versions, "cwd", "sha", "path", "oldPath")),
        "git_commit_diff" => out(c!(gx::git_commit_diff, "cwd", "sha")),
        "git_blame" => out(c!(gx::git_blame, "cwd", "path", "rev")),
        "git_refs" => out(c!(gx::git_refs, "cwd")),
        "git_checkout" => out(c!(gx::git_checkout, "cwd", "target", "create", "startPoint")),
        "git_branch_delete" => out(c!(gx::git_branch_delete, "cwd", "name", "force", "remote")),
        "git_branch_create" => out(c!(gx::git_branch_create, "cwd", "name", "startPoint")),
        "git_branch_rename" => out(c!(gx::git_branch_rename, "cwd", "from", "to")),
        "git_merge" => out(c!(gx::git_merge, "cwd", "branch", "noFf")),
        "git_abort" => out(c!(gx::git_abort, "cwd")),
        "git_tag_create" => out(c!(gx::git_tag_create, "cwd", "name", "sha", "message")),
        "git_tag_delete" => out(c!(gx::git_tag_delete, "cwd", "name")),
        "git_commit_action" => out(c!(gx::git_commit_action, "cwd", "sha", "action")),
        "git_stash_list" => out(c!(gx::git_stash_list, "cwd")),
        "git_stash_save" => out(c!(gx::git_stash_save, "cwd", "message", "includeUntracked")),
        "git_stash_apply" => out(c!(gx::git_stash_apply, "cwd", "index", "pop")),
        "git_stash_drop" => out(c!(gx::git_stash_drop, "cwd", "index")),
        "git_fetch" => out(c!(gx::git_fetch, "cwd", "opts")),
        "git_pull" => out(c!(gx::git_pull, "cwd", "opts")),
        "git_push" => out(c!(gx::git_push, "cwd", "opts")),
        "git_status" => out(c!(git::git_status, "cwd")),
        "git_file_versions" => out(c!(git::git_file_versions, "cwd", "path", "staged")),
        "git_diff_text" => out(c!(git::git_diff_text, "cwd", "staged")),
        "git_stage" => out(c!(git::git_stage, "cwd", "paths")),
        "git_unstage" => out(c!(git::git_unstage, "cwd", "paths")),
        "git_discard" => out(c!(git::git_discard, "cwd", "paths", "includeStaged")),
        "git_discard_all" => out(c!(git::git_discard_all, "cwd", "includeUntracked")),
        "git_commit" => out(c!(git::git_commit, "cwd", "message", "paths")),
        "fs_list" => out(c!(files::fs_list, "dir", "showIgnored")),
        "remote_resolve_dir" => out(c!(crate::claude::remote::remote_resolve_dir, "uri")),
        "fs_preview" => out(c!(files::fs_preview, "path")),
        "ai_providers_list" => out(c!(crate::ai::ai_providers_list)),
        "ai_provider_save" => out(c!(crate::ai::ai_provider_save, "provider")),
        "ai_provider_delete" => out(c!(crate::ai::ai_provider_delete, "id")),
        "ai_provider_detect" => out(c!(crate::ai::ai_provider_detect, "provider")),
        "fs_write_text" => out(c!(files::fs_write_text, "path", "content", "expectedVersion", "lineEnding")),
        // notes + projects use a throwaway store (see `bridge_stores`), never the real app data
        "notes_list" => out(bridge_stores().0.list()),
        "note_save" => out(arg(args, "note").and_then(|n| Ok(crate::notes::save_checked(&bridge_stores().0, &bridge_stores().1, n)))?),
        "note_delete" => out(arg::<String>(args, "id").map(|id| bridge_stores().0.delete(&id))?),
        "list_projects" => out(bridge_stores().1.list()),
        "save_project" => out(arg(args, "project").map(|p| bridge_stores().1.save(p))?),
        "list_bookmarks" => out(bridge_bookmarks().list()),
        "add_bookmark" => out(arg(args, "bookmark").map(|b| bridge_bookmarks().add(b))?),
        "remove_bookmark" => out(arg::<String>(args, "id").map(|id| bridge_bookmarks().remove(&id))?),
        "reorder_bookmarks" => out(arg::<Vec<String>>(args, "ids").map(|ids| bridge_bookmarks().reorder(&ids))?),
        other => Err(json!(format!("devbridge: unsupported command {other}"))),
    }
}

/// Bookmarks for the bridge, in the same throwaway folder as `bridge_stores`.
fn bridge_bookmarks() -> &'static crate::bookmarks::BookmarkStore {
    static S: std::sync::OnceLock<crate::bookmarks::BookmarkStore> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("devcat-bridge-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        crate::bookmarks::BookmarkStore::new(d)
    })
}

/// Notes / projects for the bridge, in a temp folder that lives as long as the process.
fn bridge_stores() -> &'static (crate::notes::NoteStore, crate::projects::ProjectStore) {
    static S: std::sync::OnceLock<(crate::notes::NoteStore, crate::projects::ProjectStore)> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("devcat-bridge-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        (crate::notes::NoteStore::new(d.clone()), crate::projects::ProjectStore::new(d))
    })
}

/// `host[:port]` naming this machine's loopback interface.
fn is_local_authority(a: &str) -> bool {
    let host = match a.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => a.rsplit_once(':').map_or(a, |(h, _)| h),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn is_local_origin(o: &str) -> bool {
    o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")).is_some_and(is_local_authority)
}

/// Serve until the process is killed.
pub async fn serve(port: u16) -> std::io::Result<()> {
    if let Some(d) = dirs::data_dir() {
        crate::ai::init(d.join("dev.devcat.app"));
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("devbridge listening on http://127.0.0.1:{port}");
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 64 * 1024];
            // read headers + body (Content-Length)
            let (head_end, len, head) = loop {
                let n = match sock.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                buf.extend_from_slice(&tmp[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                    let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0usize);
                    break (i + 4, len, head);
                }
            };
            // Only the local dev frontend may call in: any other web page (or a DNS-rebound
            // hostname) would otherwise get file, git and shell access through this port.
            let header = |name: &str| head.lines().find_map(|l| l.strip_prefix(name)).map(|v| v.trim().to_string());
            let origin = header("origin:");
            if !header("host:").is_some_and(|h| is_local_authority(&h)) || origin.as_deref().is_some_and(|o| !is_local_origin(o)) {
                let _ = sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                return;
            }
            let allow_origin = origin.unwrap_or_else(|| "null".into());
            while buf.len() < head_end + len {
                let n = match sock.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                buf.extend_from_slice(&tmp[..n]);
            }
            let is_options = buf.starts_with(b"OPTIONS");
            let (status, body) = if is_options {
                ("204 No Content", String::new())
            } else {
                let req: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
                let cmd = req["cmd"].as_str().unwrap_or("").to_string();
                let args = req.get("args").cloned().unwrap_or(json!({}));
                let started = std::time::Instant::now();
                let res = call(&cmd, &args).await;
                eprintln!("{cmd} {} {:?}", if res.is_ok() { "ok" } else { "ERR" }, started.elapsed());
                match res {
                    Ok(v) => ("200 OK", json!({ "ok": v }).to_string()),
                    Err(e) => ("200 OK", json!({ "err": e }).to_string()),
                }
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: {allow_origin}\r\nVary: Origin\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_callers() {
        for ok in ["127.0.0.1:1430", "localhost:1430", "localhost", "[::1]:1430"] {
            assert!(is_local_authority(ok), "{ok}");
        }
        for bad in ["evil.example:1430", "127.0.0.1.evil.example", "192.168.1.5:1430", ""] {
            assert!(!is_local_authority(bad), "{bad}");
        }
        assert!(is_local_origin("http://localhost:1425"));
        assert!(!is_local_origin("https://evil.example"));
        assert!(!is_local_origin("null"));
    }
}
