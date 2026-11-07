//! Files for a review of an `.html` on an SSH host (`Target::File` whose root is `ssh://…`).
//!
//! One `ssh` per review runs a small `sh` loop on the host (`SCRIPT`) that answers one file per
//! request line, so a page and all its assets load over one connection (Windows has no shared
//! master connection: a fresh `ssh` per file would mean a login per stylesheet). The file server
//! (`serve.rs`) asks through `fetch`. Rust checks the path first (`serve::check_rel`: no `..`,
//! no hidden or secret-looking names); the host checks again where the file really is: the folder
//! is resolved with `pwd -P` and must stay inside the review's folder without passing a hidden
//! folder, and the file itself may not be a symlink.

use crate::claude::remote::{self as ssh, Askpass, Target as SshTarget};
use crate::error::{AppError, AppResult};
use base64::Engine;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// Biggest file served from a host.
pub const MAX_FILE: u64 = 20 * 1024 * 1024;
/// How long the host may take to answer one file (or to connect).
const ANSWER: Duration = Duration::from_secs(30);

/// The loop on the host. `$1`: the review's folder. Prints `READY`, then answers each path line
/// with `OK <base64>`, `BIG` (over `MAX_FILE`) or `NO`.
fn script() -> String {
    format!(
        r#"cd "$1" 2>/dev/null || {{ echo "Cannot read $1 (no such folder or no permission)" >&2; exit 3; }}
command -v base64 >/dev/null 2>&1 || {{ echo "base64 isn't installed on the host" >&2; exit 4; }}
R=$(pwd -P)
echo READY
while IFS= read -r p; do
  d=$(dirname -- "$p"); f=$(basename -- "$p")
  D=$(cd -- "$R" 2>/dev/null && cd -- "$d" 2>/dev/null && pwd -P) || D=
  in=
  case "$D" in "$R"|"$R"/*) in=1 ;; esac
  [ "$R" = / ] && [ -n "$D" ] && in=1
  case "${{D#"$R"}}/" in .*|*/.*) in= ;; esac
  if [ -n "$in" ] && [ -n "$f" ] && [ ! -L "$D/$f" ] && [ -f "$D/$f" ] && [ -r "$D/$f" ]; then
    n=$(wc -c < "$D/$f" | tr -d ' ')
    if [ "$n" -le {MAX_FILE} ]; then printf 'OK '; base64 < "$D/$f" | tr -d '\r\n'; echo; else echo BIG; fi
  else
    echo NO
  fi
done
"#
    )
}

struct Conn {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    // killed when the connection is dropped (`kill_on_drop`); the password file goes with it
    _child: Child,
    _askpass: Askpass,
}

type Slot = Arc<tokio::sync::Mutex<Option<Conn>>>;

/// The open connection of each review (by review id).
static CONNS: Lazy<parking_lot::Mutex<HashMap<String, Slot>>> = Lazy::new(Default::default);

fn slot(id: &str) -> Slot {
    CONNS.lock().entry(id.to_string()).or_default().clone()
}

/// A review ended: its connection closes.
pub fn forget(id: &str) {
    CONNS.lock().remove(id);
}

async fn open(root: &str) -> AppResult<Conn> {
    let t = SshTarget::parse(root).ok_or_else(|| AppError::msg("Not a folder on an SSH host"))?;
    t.ready().await?;
    let (mut cmd, askpass) = ssh::ssh_command(&t, &script(), &[&t.path]);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("Couldn't run ssh ({e}). Install the OpenSSH client.")))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(AppError::msg("Couldn't talk to ssh"));
    };
    let mut stdout = BufReader::new(stdout);
    let mut first = String::new();
    let ready = tokio::time::timeout(ANSWER, (&mut stdout).take(64).read_line(&mut first)).await;
    if matches!(ready, Ok(Ok(_))) && first.trim_end() == "READY" {
        return Ok(Conn { stdin, stdout, _child: child, _askpass: askpass });
    }
    let _ = child.start_kill();
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = tokio::time::timeout(Duration::from_secs(2), e.read_to_string(&mut err)).await;
    }
    let err = err.trim();
    Err(match child.try_wait() {
        Ok(Some(s)) if s.code() == Some(255) => ssh::connect_error(&t, err),
        _ if ready.is_err() => AppError::msg(format!("{} didn't answer in time", t.label())),
        _ if !err.is_empty() => AppError::msg(err.to_string()),
        _ => AppError::msg(format!("Couldn't read files on {}", t.label())),
    })
}

/// What the host answered for one file.
#[derive(Debug, PartialEq)]
pub enum Answer {
    File(Vec<u8>),
    Missing,
    TooBig,
}

async fn ask(c: &mut Conn, rel: &str) -> AppResult<Answer> {
    let broken = |e: std::io::Error| AppError::msg(format!("The SSH connection broke: {e}"));
    c.stdin.write_all(format!("{rel}\n").as_bytes()).await.map_err(broken)?;
    c.stdin.flush().await.map_err(broken)?;
    let mut line = String::new();
    // base64 is 4/3 the size, plus `OK ` and the line end
    let limit = MAX_FILE / 3 * 4 + 16;
    if (&mut c.stdout).take(limit).read_line(&mut line).await.map_err(broken)? == 0 {
        return Err(AppError::msg("The SSH connection closed"));
    }
    let line = line.trim_end();
    match line {
        "NO" => Ok(Answer::Missing),
        "BIG" => Ok(Answer::TooBig),
        _ => {
            let b64 = line.strip_prefix("OK ").or_else(|| (line == "OK").then_some("")).ok_or_else(|| AppError::msg("The host answered something unexpected"))?;
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map(Answer::File)
                .map_err(|_| AppError::msg("The host answered something unexpected"))
        }
    }
}

/// The file `rel` (checked with `serve::check_rel`) in the review's folder `root` (`ssh://…`),
/// over the review's connection, opened on first use. A connection that fails or doesn't answer
/// in time is dropped, and the next file opens a new one.
pub async fn fetch(id: &str, root: &str, rel: &str) -> AppResult<Answer> {
    if rel.contains(['\n', '\r', '\0']) {
        return Ok(Answer::Missing);
    }
    let slot = slot(id);
    let mut conn = slot.lock().await;
    if conn.is_none() {
        *conn = Some(open(root).await?);
    }
    let Some(c) = conn.as_mut() else { return Err(AppError::msg("No SSH connection")) };
    match tokio::time::timeout(ANSWER, ask(c, rel)).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(e)) => {
            *conn = None;
            Err(e)
        }
        Err(_) => {
            *conn = None;
            Err(AppError::msg("The SSH host didn't answer in time"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::remote::LOCAL_TEST_HOST;

    fn site() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-review-ssh-{}", uuid::Uuid::new_v4().simple()));
        for dir in ["site/assets", "site/.git", "site/a/.cache", "outside"] {
            std::fs::create_dir_all(d.join(dir)).unwrap();
        }
        std::fs::write(d.join("site/index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(d.join("site/assets/app.css"), "h1{color:teal}").unwrap();
        std::fs::write(d.join("site/assets/my page.css"), "x").unwrap();
        std::fs::write(d.join("site/.git/config"), "secret").unwrap();
        std::fs::write(d.join("site/a/.cache/x.js"), "secret").unwrap();
        std::fs::write(d.join("outside/secret.txt"), "secret").unwrap();
        std::fs::write(d.join("site/empty.txt"), "").unwrap();
        d
    }

    fn root(d: &std::path::Path) -> String {
        format!("ssh://{LOCAL_TEST_HOST}{}", d.join("site").display())
    }

    #[tokio::test]
    async fn serves_files_inside_the_folder_only() {
        let d = site();
        let (id, root) = (format!("t-{}", uuid::Uuid::new_v4().simple()), root(&d));
        assert_eq!(fetch(&id, &root, "index.html").await.unwrap(), Answer::File(b"<h1>hi</h1>".to_vec()));
        assert_eq!(fetch(&id, &root, "assets/app.css").await.unwrap(), Answer::File(b"h1{color:teal}".to_vec()));
        assert_eq!(fetch(&id, &root, "./assets/my page.css").await.unwrap(), Answer::File(b"x".to_vec()));
        assert_eq!(fetch(&id, &root, "empty.txt").await.unwrap(), Answer::File(vec![]));
        for rel in ["nope.html", "assets", "../outside/secret.txt", "assets/../../outside/secret.txt", "a/.cache/x.js", ".git/config", "-n", "\nindex.html"] {
            assert_eq!(fetch(&id, &root, rel).await.unwrap(), Answer::Missing, "{rel:?}");
        }
        // still the same connection after refusals
        assert_eq!(fetch(&id, &root, "index.html").await.unwrap(), Answer::File(b"<h1>hi</h1>".to_vec()));
        forget(&id);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_refuses_links_out_and_into_hidden_folders() {
        let d = site();
        let s = d.join("site");
        std::os::unix::fs::symlink(d.join("outside/secret.txt"), s.join("out.txt")).unwrap();
        std::os::unix::fs::symlink(s.join("index.html"), s.join("in.html")).unwrap();
        std::os::unix::fs::symlink(d.join("outside"), s.join("outdir")).unwrap();
        std::os::unix::fs::symlink(s.join(".git"), s.join("gitdir")).unwrap();
        std::os::unix::fs::symlink(s.join("assets"), s.join("assets2")).unwrap();
        let (id, root) = (format!("t-{}", uuid::Uuid::new_v4().simple()), root(&d));
        for rel in ["out.txt", "in.html", "outdir/secret.txt", "gitdir/config"] {
            assert_eq!(fetch(&id, &root, rel).await.unwrap(), Answer::Missing, "{rel}");
        }
        // a linked folder that stays inside is fine
        assert_eq!(fetch(&id, &root, "assets2/app.css").await.unwrap(), Answer::File(b"h1{color:teal}".to_vec()));
        forget(&id);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_missing_folder_is_an_error() {
        let d = site();
        let id = format!("t-{}", uuid::Uuid::new_v4().simple());
        let root = format!("ssh://{LOCAL_TEST_HOST}{}", d.join("nope").display());
        let e = fetch(&id, &root, "index.html").await.unwrap_err().to_string();
        assert!(e.contains("Cannot read"), "{e}");
        forget(&id);
        let _ = std::fs::remove_dir_all(&d);
    }
}
