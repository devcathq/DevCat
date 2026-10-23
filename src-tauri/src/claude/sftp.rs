//! The SFTP tab and getting files onto an SSH host from a terminal (desktop only: see CLAUDE.md).
//! Everything goes through the system `ssh` like the rest of `remote.rs` (one shared master
//! connection per host on macOS / Linux), streaming files with `cat` so progress is ours to count.
//!
//! Events:
//!   `sftp-progress` → { jobId, file, files, filesTotal, bytes, bytesTotal }   (~10 per second)
//!   `sftp-done`     → { jobId, error, cancelled }
use super::remote::{self, Target};
use crate::error::{AppError, AppResult};
use base64::Engine;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Pasted / dropped files bigger than this belong in the SFTP tab.
const MAX_STAGE: u64 = 100 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

fn target(uri: &str) -> AppResult<Target> {
    let t = Target::parse(uri).ok_or_else(|| AppError::msg("Not a folder on an SSH host (ssh://host/path)"))?;
    t.check()?;
    Ok(t)
}

// ---------- terminals: paste an image / drop files ----------

/// Paste an image into a terminal on an SSH host: the image (`data`, base64, or else the OS
/// clipboard's) is copied to the host and its path there returned, for typing into the terminal
/// (Claude Code attaches an image given by path). None: the clipboard holds no image.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_paste_image(cwd: String, data: Option<String>, name: Option<String>) -> AppResult<Option<String>> {
    let t = target(&cwd)?;
    let (bytes, name) = match data {
        Some(d) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(d.as_bytes()).map_err(|e| AppError::msg(format!("Couldn't read the image: {e}")))?;
            (bytes, name.unwrap_or_else(|| "pasted.png".into()))
        }
        None => match clipboard_image().await? {
            Some(x) => x,
            None => return Ok(None),
        },
    };
    if bytes.len() as u64 > MAX_STAGE {
        return Err(AppError::msg("That image is over 100 MB"));
    }
    remote::stage_bytes(&t, &name, &bytes).await.map(Some)
}

/// Copy local files to the host's paste folder (dropped on a terminal); returns their paths there.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn remote_stage_files(cwd: String, paths: Vec<String>) -> AppResult<Vec<String>> {
    let t = target(&cwd)?;
    let mut out = Vec::with_capacity(paths.len());
    for p in &paths {
        let path = Path::new(p);
        let meta = tokio::fs::metadata(path).await.map_err(|e| AppError::msg(format!("{p}: {e}")))?;
        if meta.is_dir() {
            return Err(AppError::msg(format!("{} is a folder: copy folders with the SFTP tab", file_name(path))));
        }
        if meta.len() > MAX_STAGE {
            return Err(AppError::msg(format!("{} is over 100 MB: copy it with the SFTP tab", file_name(path))));
        }
        let bytes = tokio::fs::read(path).await?;
        out.push(remote::stage_bytes(&t, &file_name(path), &bytes).await?);
    }
    Ok(out)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

const IMAGE_EXT: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// The OS clipboard's image as PNG bytes (or a copied image file), if any.
#[cfg(feature = "gui")]
pub(crate) async fn clipboard_image() -> AppResult<Option<(Vec<u8>, String)>> {
    let found = crate::rt::spawn_blocking(read_clipboard_image).await.map_err(|e| AppError::msg(e.to_string()))??;
    #[cfg(windows)]
    let found = match found {
        Some(x) => Some(x),
        None => bitmap_to_png().await.map(|b| (b, "pasted.png".to_string())),
    };
    let Some((bytes, name)) = found else { return Ok(None) };
    if name.ends_with(".tiff") {
        return tiff_to_png(bytes).await.map(|b| Some((b, "pasted.png".into())));
    }
    Ok(Some((bytes, name)))
}

#[cfg(not(feature = "gui"))]
pub(crate) async fn clipboard_image() -> AppResult<Option<(Vec<u8>, String)>> {
    Err(AppError::msg("Pasting images needs the DevCat app"))
}

#[cfg(feature = "gui")]
fn read_clipboard_image() -> AppResult<Option<(Vec<u8>, String)>> {
    use clipboard_rs::{Clipboard, ClipboardContext};
    let ctx = ClipboardContext::new().map_err(|e| AppError::msg(format!("Couldn't open the clipboard: {e}")))?;
    // Image data, under each platform's name for PNG (TIFF: what some macOS apps copy).
    let formats: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[("public.png", "pasted.png"), ("public.tiff", "pasted.tiff")]
    } else if cfg!(windows) {
        &[("PNG", "pasted.png"), ("image/png", "pasted.png")]
    } else {
        &[("image/png", "pasted.png")]
    };
    let available = ctx.available_formats().unwrap_or_default();
    for (fmt, name) in formats {
        if available.iter().any(|a| a == fmt) {
            if let Ok(b) = ctx.get_buffer(fmt) {
                if !b.is_empty() {
                    return Ok(Some((b, (*name).to_string())));
                }
            }
        }
    }
    // An image file copied in Finder / Explorer.
    if let Ok(files) = ctx.get_files() {
        for f in files {
            let p = f.strip_prefix("file://").map(|s| s.to_string()).unwrap_or(f);
            let path = PathBuf::from(&p);
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if IMAGE_EXT.contains(&ext.as_str()) && path.is_file() {
                let bytes = std::fs::read(&path)?;
                return Ok(Some((bytes, file_name(&path))));
            }
        }
    }
    Ok(None)
}

/// macOS: TIFF → PNG with the built-in `sips` (agents read PNG, not TIFF).
#[cfg(feature = "gui")]
async fn tiff_to_png(tiff: Vec<u8>) -> AppResult<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("devcat-paste-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&dir).await?;
    let (src, dst) = (dir.join("in.tiff"), dir.join("out.png"));
    tokio::fs::write(&src, tiff).await?;
    let ok = tokio::process::Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(&src)
        .arg("--out")
        .arg(&dst)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    let png = if ok { tokio::fs::read(&dst).await.ok() } else { None };
    let _ = tokio::fs::remove_dir_all(&dir).await;
    png.ok_or_else(|| AppError::msg("Couldn't convert the clipboard image to PNG"))
}

/// Windows: a screenshot (Print Screen, Win+Shift+S) is on the clipboard as a bitmap, not PNG;
/// saved as PNG by PowerShell (like `sips` on macOS), None if there's no image.
#[cfg(all(feature = "gui", windows))]
async fn bitmap_to_png() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("devcat-paste-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let dst = dir.join("out.png");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $i = [System.Windows.Forms.Clipboard]::GetImage(); if ($i) {{ $i.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png) }}",
        dst.display().to_string().replace('\'', "''")
    );
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", script.as_str()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let ok = cmd.status().await.is_ok_and(|s| s.success());
    let png = if ok { tokio::fs::read(&dst).await.ok().filter(|b| !b.is_empty()) } else { None };
    let _ = tokio::fs::remove_dir_all(&dir).await;
    png
}

// ---------- file management (local or remote paths) ----------

/// New folder.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn sftp_mkdir(path: String) -> AppResult<()> {
    match Target::parse(&path) {
        Some(t) => remote::mkdir(&t).await,
        None => Ok(tokio::fs::create_dir_all(&path).await?),
    }
}

/// Rename in place (`name` is the new file name, not a path).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn sftp_rename(path: String, name: String) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || (cfg!(windows) && name.contains('\\')) {
        return Err(AppError::msg("Enter a name without slashes"));
    }
    match Target::parse(&path) {
        Some(t) => {
            let parent = parent_of(&t.path);
            let to = format!("{}/{name}", parent.trim_end_matches('/'));
            remote::rename(&t, &to).await?;
            Ok(t.uri(&to))
        }
        None => {
            let from = PathBuf::from(&path);
            let to = from.parent().ok_or_else(|| AppError::msg("Can't rename that"))?.join(name);
            if to.exists() {
                return Err(AppError::msg(format!("{name} already exists")));
            }
            tokio::fs::rename(&from, &to).await?;
            Ok(to.to_string_lossy().into_owned())
        }
    }
}

/// Delete files and folders (folders with everything in them). All on one host, or all local.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn sftp_delete(paths: Vec<String>) -> AppResult<()> {
    let Some(first) = paths.first() else { return Ok(()) };
    match Target::parse(first) {
        Some(t) => {
            let mut rel = Vec::with_capacity(paths.len());
            for p in &paths {
                let pt = Target::parse(p).filter(|x| x.dest == t.dest && x.port == t.port).ok_or_else(|| AppError::msg("Delete files of one host at a time"))?;
                rel.push(pt.path);
            }
            remote::remove(&t, &rel).await
        }
        None => {
            for p in &paths {
                let path = Path::new(p);
                if Target::parse(p).is_some() || !path.is_absolute() || path.parent().is_none() || dirs::home_dir().is_some_and(|h| h == path) {
                    return Err(AppError::msg(format!("Refusing to delete {p}")));
                }
                let meta = tokio::fs::symlink_metadata(path).await?;
                if meta.is_dir() {
                    tokio::fs::remove_dir_all(path).await?;
                } else {
                    tokio::fs::remove_file(path).await?;
                }
            }
            Ok(())
        }
    }
}

fn parent_of(p: &str) -> String {
    match p.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => "/".into(),
        Some((parent, _)) => parent.into(),
        None if p.starts_with('~') => "~".into(),
        None => ".".into(),
    }
}

// ---------- transfers ----------

/// Copy `src` (a file or folder) into the folder `dstDir`: local → SSH host or SSH host → local.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferItem {
    pub src: String,
    pub dst_dir: String,
}

static JOBS: Lazy<Mutex<HashMap<String, Arc<AtomicBool>>>> = Lazy::new(Default::default);

/// Start copying in the background; progress and the end come as events (see top).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn sftp_transfer(job_id: String, items: Vec<TransferItem>) -> AppResult<()> {
    if job_id.is_empty() || JOBS.lock().contains_key(&job_id) {
        return Err(AppError::msg("Invalid transfer id"));
    }
    for it in &items {
        if Target::parse(&it.src).is_some() == Target::parse(&it.dst_dir).is_some() {
            return Err(AppError::msg("Copy between this computer and an SSH host"));
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    JOBS.lock().insert(job_id.clone(), cancel.clone());
    tokio::spawn(async move {
        let res = run_job(&job_id, &items, &cancel).await;
        JOBS.lock().remove(&job_id);
        let cancelled = cancel.load(Ordering::Relaxed);
        let error = match res {
            Err(e) if !cancelled => Some(e.to_string()),
            _ => None,
        };
        crate::events::emit("sftp-done", json!({ "jobId": job_id, "error": error, "cancelled": cancelled }));
    });
    Ok(())
}

/// Stop a transfer (the file being copied is left partial).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn sftp_cancel(job_id: String) -> AppResult<()> {
    if let Some(c) = JOBS.lock().get(&job_id) {
        c.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// One file to copy, and folders to create first.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Dir(String),
    File { src: String, dst: String, size: u64 },
}

struct Progress<'a> {
    job: &'a str,
    files_total: usize,
    bytes_total: u64,
    files: usize,
    bytes: u64,
    last: Instant,
}

impl Progress<'_> {
    fn report(&mut self, file: &str, force: bool) {
        if !force && self.last.elapsed() < PROGRESS_EVERY {
            return;
        }
        self.last = Instant::now();
        crate::events::emit(
            "sftp-progress",
            json!({ "jobId": self.job, "file": file, "files": self.files, "filesTotal": self.files_total, "bytes": self.bytes, "bytesTotal": self.bytes_total }),
        );
    }
}

async fn run_job(job: &str, items: &[TransferItem], cancel: &AtomicBool) -> AppResult<()> {
    // Plan everything first so progress has totals.
    let mut plans: Vec<(Target, bool, Vec<Step>)> = Vec::new();
    for it in items {
        match (Target::parse(&it.src), Target::parse(&it.dst_dir)) {
            (None, Some(dst)) => {
                dst.ready().await?;
                plans.push((dst.clone(), true, plan_upload(Path::new(&it.src), &dst.path)?));
            }
            (Some(src), None) => {
                src.ready().await?;
                let tree = remote::list_tree(&src).await?;
                plans.push((src, false, plan_download(&tree, &parent_of_remote(&it.src), Path::new(&it.dst_dir))));
            }
            _ => return Err(AppError::msg("Copy between this computer and an SSH host")),
        }
    }
    let mut p = Progress {
        job,
        files_total: plans.iter().flat_map(|(_, _, s)| s).filter(|s| matches!(s, Step::File { .. })).count(),
        bytes_total: plans.iter().flat_map(|(_, _, s)| s).map(|s| if let Step::File { size, .. } = s { *size } else { 0 }).sum(),
        files: 0,
        bytes: 0,
        last: Instant::now(),
    };
    p.report("", true);
    for (t, upload, steps) in &plans {
        // Folders in a few batches (one ssh each), then the files.
        let dirs: Vec<&str> = steps.iter().filter_map(|s| if let Step::Dir(d) = s { Some(d.as_str()) } else { None }).collect();
        if *upload {
            for chunk in dirs.chunks(200) {
                remote_mkdirs(t, chunk).await?;
            }
        } else {
            for d in &dirs {
                tokio::fs::create_dir_all(d).await?;
            }
        }
        for s in steps {
            let Step::File { src, dst, .. } = s else { continue };
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::msg("Cancelled"));
            }
            if *upload {
                upload_file(t, Path::new(src), dst, &mut p, cancel).await?;
            } else {
                download_file(t, src, Path::new(dst), &mut p, cancel).await?;
            }
            p.files += 1;
            p.report(dst, true);
        }
    }
    Ok(())
}

/// The remote parent folder of an `ssh://` item (its tree's paths are relative to it).
fn parent_of_remote(uri: &str) -> String {
    Target::parse(uri).map(|t| parent_of(&t.path)).unwrap_or_else(|| "/".into())
}

/// Local file / folder → steps with remote destinations under `dst_dir`.
fn plan_upload(src: &Path, dst_dir: &str) -> AppResult<Vec<Step>> {
    let base = format!("{}/{}", dst_dir.trim_end_matches('/'), file_name(src));
    let meta = std::fs::metadata(src).map_err(|e| AppError::msg(format!("{}: {e}", src.display())))?;
    let mut steps = Vec::new();
    if !meta.is_dir() {
        steps.push(Step::File { src: src.to_string_lossy().into_owned(), dst: base, size: meta.len() });
        return Ok(steps);
    }
    steps.push(Step::Dir(base.clone()));
    let mut stack = vec![(src.to_path_buf(), base)];
    while let Some((dir, remote_dir)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let dst = format!("{remote_dir}/{name}");
            // Follows symlinks like `cp -R` would not: skip links to avoid loops.
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                steps.push(Step::Dir(dst.clone()));
                stack.push((path, dst));
            } else if ft.is_file() {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                steps.push(Step::File { src: path.to_string_lossy().into_owned(), dst, size });
            }
        }
    }
    Ok(steps)
}

/// Is `name` (one remote path part) a plain file name here too? A host's file may be called
/// `..\\..\\x` or `C:\\x`: legal on Linux, but a path that leaves the folder on Windows.
fn plain_local_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(n)) if n == name) && parts.next().is_none()
}

/// Remote tree (paths relative to `remote_parent`) → steps with local destinations under `dst_dir`.
/// Items whose names wouldn't stay inside `dst_dir` on this computer are skipped.
fn plan_download(tree: &[remote::TreeItem], remote_parent: &str, dst_dir: &Path) -> Vec<Step> {
    let parent = remote_parent.trim_end_matches('/');
    tree.iter()
        .filter(|i| i.rel.split('/').all(plain_local_name))
        .map(|i| {
            let local = i.rel.split('/').fold(dst_dir.to_path_buf(), |p, c| p.join(c)).to_string_lossy().into_owned();
            if i.is_dir {
                Step::Dir(local)
            } else {
                Step::File { src: format!("{parent}/{}", i.rel), dst: local, size: i.size }
            }
        })
        .collect()
}

async fn remote_mkdirs(t: &Target, dirs: &[&str]) -> AppResult<()> {
    let (mut cmd, _askpass) = remote::ssh_command(t, r#"mkdir -p -- "$@""#, dirs);
    let out = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().await?;
    check_exit(t, out.status, &out.stderr)
}

fn check_exit(t: &Target, status: std::process::ExitStatus, stderr: &[u8]) -> AppResult<()> {
    if status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    if status.code() == Some(255) {
        return Err(remote::connect_error(t, &err));
    }
    Err(AppError::msg(if err.is_empty() { format!("Copying failed on {}", t.label()) } else { err }))
}

async fn upload_file(t: &Target, src: &Path, dst: &str, p: &mut Progress<'_>, cancel: &AtomicBool) -> AppResult<()> {
    let mut file = tokio::fs::File::open(src).await.map_err(|e| AppError::msg(format!("{}: {e}", src.display())))?;
    let (mut cmd, _askpass) = remote::ssh_command(t, r#"cat > "$1""#, &[dst]);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::msg(format!("Couldn't run ssh ({e})")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| AppError::msg("ssh: no input"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| AppError::msg("ssh: no error output"))?;
    let err_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b).await;
        b
    });
    let mut buf = vec![0u8; CHUNK];
    let copied: AppResult<()> = async {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::msg("Cancelled"));
            }
            let n = file.read(&mut buf).await?;
            if n == 0 {
                return Ok(());
            }
            stdin.write_all(&buf[..n]).await.map_err(|e| AppError::msg(format!("ssh: {e}")))?;
            p.bytes += n as u64;
            p.report(dst, false);
        }
    }
    .await;
    drop(stdin);
    if copied.is_err() {
        let _ = child.kill().await;
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    // ssh failing (host key, password…) also breaks the pipe: its own message says why.
    if copied.is_ok() || (!status.success() && !cancel.load(Ordering::Relaxed)) {
        check_exit(t, status, &stderr)?;
    }
    copied
}

async fn download_file(t: &Target, src: &str, dst: &Path, p: &mut Progress<'_>, cancel: &AtomicBool) -> AppResult<()> {
    let (mut cmd, _askpass) = remote::ssh_command(t, r#"cat -- "$1""#, &[src]);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::msg(format!("Couldn't run ssh ({e})")))?;
    let mut stdout = child.stdout.take().ok_or_else(|| AppError::msg("ssh: no output"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| AppError::msg("ssh: no error output"))?;
    let err_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b).await;
        b
    });
    let mut file = tokio::fs::File::create(dst).await.map_err(|e| AppError::msg(format!("{}: {e}", dst.display())))?;
    let mut buf = vec![0u8; CHUNK];
    let label = dst.to_string_lossy().into_owned();
    let copied: AppResult<()> = async {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::msg("Cancelled"));
            }
            let n = stdout.read(&mut buf).await.map_err(|e| AppError::msg(format!("ssh: {e}")))?;
            if n == 0 {
                return Ok(());
            }
            file.write_all(&buf[..n]).await?;
            p.bytes += n as u64;
            p.report(&label, false);
        }
    }
    .await;
    file.flush().await?;
    if copied.is_err() {
        let _ = child.kill().await;
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    // ssh failing (host key, password…) also breaks the pipe: its own message says why.
    if copied.is_ok() || (!status.success() && !cancel.load(Ordering::Relaxed)) {
        check_exit(t, status, &stderr)?;
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents() {
        assert_eq!(parent_of("/srv/app/x.txt"), "/srv/app");
        assert_eq!(parent_of("/x"), "/");
        assert_eq!(parent_of("~/x"), "~");
        assert_eq!(parent_of("/srv/app/"), "/srv");
    }

    #[test]
    fn local_names_stay_in_the_folder() {
        for ok in ["a.txt", "..x", "x..", "name with spaces"] {
            assert!(plain_local_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "/etc"] {
            assert!(!plain_local_name(bad), "{bad}");
        }
        // Backslashes and drive letters are path syntax on Windows only.
        for name in [r"..\..\x", r"C:\x", r"C:x", r"\\server\share"] {
            assert_eq!(plain_local_name(name), !cfg!(windows), "{name}");
        }
    }

    #[test]
    fn plans_downloads_inside_the_target_folder() {
        let tree = vec![
            remote::TreeItem { rel: "app".into(), is_dir: true, size: 0 },
            remote::TreeItem { rel: "app/a.txt".into(), is_dir: false, size: 3 },
            remote::TreeItem { rel: "app/../../etc/passwd".into(), is_dir: false, size: 1 },
            remote::TreeItem { rel: "app/".into(), is_dir: false, size: 1 },
        ];
        let steps = plan_download(&tree, "/srv/", Path::new("/tmp/dl"));
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(steps[0], Step::Dir(Path::new("/tmp/dl").join("app").to_string_lossy().into_owned()));
        assert_eq!(
            steps[1],
            Step::File { src: "/srv/app/a.txt".into(), dst: Path::new("/tmp/dl").join("app").join("a.txt").to_string_lossy().into_owned(), size: 3 }
        );
    }

    #[test]
    fn plans_uploads() {
        let dir = std::env::temp_dir().join(format!("devcat-sftp-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "abc").unwrap();
        std::fs::write(dir.join("sub/b.txt"), "de").unwrap();
        let steps = plan_upload(&dir, "~/up/").unwrap();
        let name = file_name(&dir);
        assert_eq!(steps[0], Step::Dir(format!("~/up/{name}")));
        assert!(steps.contains(&Step::File { src: dir.join("a.txt").to_string_lossy().into_owned(), dst: format!("~/up/{name}/a.txt"), size: 3 }));
        assert!(steps.contains(&Step::Dir(format!("~/up/{name}/sub"))));
        assert!(steps.contains(&Step::File { src: dir.join("sub").join("b.txt").to_string_lossy().into_owned(), dst: format!("~/up/{name}/sub/b.txt"), size: 2 }));
        let one = plan_upload(&dir.join("a.txt"), "/srv").unwrap();
        assert_eq!(one, vec![Step::File { src: dir.join("a.txt").to_string_lossy().into_owned(), dst: "/srv/a.txt".into(), size: 3 }]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// End to end against a real host, through a saved session (key login, no password: askpass
/// needs the DevCat binary). DEVCAT_SSH_SESSION_TEST="host port user /path/to/key". The host key
/// must be known, or a master connection already open (ControlPath /tmp/devcat-ssh-<uid>/%C).
#[cfg(all(test, unix))]
mod host_tests {
    use super::*;
    use crate::claude::runner::tests::channel_sink;

    #[tokio::test]
    #[ignore]
    async fn session_against_a_host() {
        let Ok(spec) = std::env::var("DEVCAT_SSH_SESSION_TEST") else { return };
        let p: Vec<&str> = spec.split_whitespace().collect();
        let data = std::env::temp_dir().join(format!("devcat-itest-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&data).unwrap();
        let conn = json!([{
            "id": "itest", "name": "Test box", "kind": "ssh", "host": p[0], "port": p[1].parse::<u16>().unwrap(), "user": p[2],
            "options": { "keyPath": p[3], "startup": "echo STARTUP-RAN" }, "readOnly": false
        }]);
        std::fs::write(data.join("connections.json"), conn.to_string()).unwrap();
        crate::state::init(data.clone());

        let home = Target::parse("ssh://devcat-itest/~").unwrap();
        let s = home.session.as_deref().expect("session resolved");
        assert_eq!((s.host.as_str(), s.user.as_deref(), s.port), (p[0], Some(p[2]), Some(p[1].parse().unwrap())));
        assert!(Target::parse("ssh://devcat-nope/~").unwrap().check().is_err());

        let cfg = crate::state::St::get().store.get("itest").unwrap();
        let who = remote::test_session(&cfg).await.unwrap();
        assert!(who.starts_with(&format!("{} on ", p[2])), "{who}");

        // a pasted image lands in ~/.cache/devcat/paste
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let staged = remote_paste_image("ssh://devcat-itest/~".into(), Some(base64::engine::general_purpose::STANDARD.encode(&png)), Some("shot one.png".into())).await.unwrap().unwrap();
        assert!(staged.contains("/.cache/devcat/paste/") && staged.ends_with("-shot_one.png"), "{staged}");
        let back = remote::list_tree(&Target::parse(&format!("ssh://devcat-itest{staged}")).unwrap()).await.unwrap();
        assert_eq!(back[0].size, png.len() as u64);

        // folder ops
        let base = "ssh://devcat-itest/~/devcat-itest";
        let _ = sftp_delete(vec![base.into()]).await;
        sftp_mkdir(format!("{base}/a/b")).await.unwrap();
        let renamed = sftp_rename(format!("{base}/a"), "c".into()).await.unwrap();
        assert_eq!(renamed, format!("{base}/c"));
        assert!(sftp_rename(format!("{base}/c"), "../x".into()).await.is_err());

        // upload a folder, download it again, compare
        let local = data.join("up");
        std::fs::create_dir_all(local.join("sub")).unwrap();
        std::fs::write(local.join("one.txt"), "first file\n").unwrap();
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(local.join("sub/big.bin"), &big).unwrap();
        let cancel = AtomicBool::new(false);
        let up = vec![TransferItem { src: local.to_string_lossy().into_owned(), dst_dir: base.into() }];
        run_job("t1", &up, &cancel).await.unwrap();
        let tree = remote::list_tree(&Target::parse(&format!("{base}/up")).unwrap()).await.unwrap();
        assert!(tree.iter().any(|i| i.rel == "up/sub/big.bin" && i.size == big.len() as u64), "{tree:?}");
        let dl = data.join("dl");
        std::fs::create_dir_all(&dl).unwrap();
        let down = vec![TransferItem { src: format!("{base}/up"), dst_dir: dl.to_string_lossy().into_owned() }];
        run_job("t2", &down, &cancel).await.unwrap();
        assert_eq!(std::fs::read(dl.join("up/sub/big.bin")).unwrap(), big);
        assert_eq!(std::fs::read_to_string(dl.join("up/one.txt")).unwrap(), "first file\n");

        // a terminal: logs in, runs the startup command, then the login shell
        let (sink, mut rx) = channel_sink();
        let id = crate::claude::pty::open(sink, "ssh://devcat-itest/~/devcat-itest", None, vec![], vec![], 100, 30, "desktop").await.unwrap();
        crate::claude::pty::pty_write(id.clone(), "echo PWD=$PWD; exit\r".into()).await.unwrap();
        let mut out = String::new();
        loop {
            let (e, v) = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("timeout").expect("closed");
            if e == "pty-output" {
                out.push_str(&String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap()));
            } else if e == "pty-exit" {
                break;
            }
        }
        assert!(out.contains("STARTUP-RAN") && out.contains("PWD=/") && out.contains("/devcat-itest"), "{out}");

        // port forwards: the host's sshd at a local port (-L), a local listener on the host (-R)
        use crate::claude::ssh_forward as fwd;
        use tokio::io::AsyncReadExt as _;
        let here = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let here_port = here.local_addr().unwrap().port();
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        let mut cfg = crate::state::St::get().store.get("itest").unwrap();
        let sshd_port = std::env::var("DEVCAT_SSH_INNER_PORT").unwrap_or_else(|_| "22".into());
        cfg.options.insert(
            "forwards".into(),
            json!([
                { "kind": "local", "localPort": free, "remoteHost": "localhost", "remotePort": sshd_port.parse::<u16>().unwrap() },
                { "kind": "remote", "localPort": here_port, "remotePort": 18080 },
            ])
            .to_string(),
        );
        crate::state::St::get().store.save(cfg).unwrap();
        let mut events = {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            crate::events::add_tap(move |e, v| {
                if e == "ssh-forward" {
                    let _ = tx.send(v.clone());
                }
            });
            rx
        };
        fwd::ssh_forward_start("itest".into()).await.unwrap();
        loop {
            let v = tokio::time::timeout(Duration::from_secs(20), events.recv()).await.expect("forward timeout").unwrap();
            assert_ne!(v["state"], "error", "{v}");
            if v["state"] == "running" {
                break;
            }
        }
        let mut banner = [0u8; 7];
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", free)).await.unwrap();
        c.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"SSH-2.0", "-L reaches the host's sshd");
        let accept = tokio::spawn(async move {
            let (mut s, _) = here.accept().await.unwrap();
            let mut b = [0u8; 13];
            s.read_exact(&mut b).await.unwrap();
            String::from_utf8_lossy(&b).into_owned()
        });
        let (mut cmd, _guard) = remote::ssh_command(&home, "printf from-the-host | nc -w 2 127.0.0.1 18080", &[]);
        let out = cmd.output().await.unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), accept).await.unwrap().unwrap(), "from-the-host", "-R reaches this computer");
        fwd::ssh_forward_stop("itest".into()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", free)).await.is_err(), "stopped");
        // a port that's taken: an error, not a silent half-start
        let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = crate::state::St::get().store.get("itest").unwrap();
        cfg.options.insert("forwards".into(), json!([{ "kind": "local", "localPort": taken.local_addr().unwrap().port(), "remoteHost": "localhost", "remotePort": 22 }]).to_string());
        crate::state::St::get().store.save(cfg).unwrap();
        fwd::ssh_forward_start("itest".into()).await.unwrap();
        loop {
            let v = tokio::time::timeout(Duration::from_secs(20), events.recv()).await.expect("forward timeout").unwrap();
            if v["state"] == "error" {
                assert!(v["error"].as_str().unwrap().contains("in use"), "{v}");
                break;
            }
        }

        sftp_delete(vec![base.into()]).await.unwrap();
        let _ = std::fs::remove_dir_all(&data);
    }
}
