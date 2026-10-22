//! Claude workspace: git status / diff / stage / commit via the `git` CLI.
use super::*;
use crate::error::{AppError, AppResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Max bytes per side for `git_file_versions`.
pub(crate) const VERSION_CAP: u64 = 5 * 1024 * 1024;
/// Max total size of `git_diff_text` output.
pub(crate) const DIFF_CAP: usize = 20 * 1024 * 1024;
/// Max untracked files rendered as new-file diffs.
const UNTRACKED_DIFF_FILES: usize = 200;
const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);

// ---------- git binary + process helpers ----------

pub(crate) static GIT_BIN: Lazy<PathBuf> = Lazy::new(|| {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Git\cmd\git.exe",
            r"C:\Program Files\Git\bin\git.exe",
            r"C:\Program Files (x86)\Git\cmd\git.exe",
        ]
    } else {
        &["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("git"))
});

/// PATH for git (and commit hooks) when launched from a GUI app with a minimal environment.
static GIT_PATH_ENV: Lazy<Option<OsString>> = Lazy::new(|| {
    if cfg!(windows) {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let p = PathBuf::from(extra);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    std::env::join_paths(dirs).ok()
});

pub(crate) struct GitOut {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

pub(crate) fn git_command(cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&*GIT_BIN);
    cmd.arg("-C").arg(cwd);
    // core.fsmonitor in an untrusted repo's .git/config would run an arbitrary command on `git status`
    cmd.args(["-c", "core.quotepath=false", "-c", "color.ui=false", "-c", "core.fsmonitor=false", "--no-pager"]);
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(p) = &*GIT_PATH_ENV {
        cmd.env("PATH", p);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Runs git; only fails when git can't be spawned. Non-zero exit codes are returned to the caller.
pub(crate) async fn git_raw<I, S>(cwd: &Path, args: I) -> AppResult<GitOut>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = git_command(cwd)
        .args(args)
        .output()
        .await
        .map_err(|e| AppError::msg(format!("Failed to run git ({}): {e}", GIT_BIN.display())))?;
    Ok(GitOut {
        code: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

pub(crate) fn git_error(out: &GitOut) -> AppError {
    let msg: Vec<&str> = out
        .stderr
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
        .collect();
    if msg.is_empty() {
        AppError::msg(format!("git exited with code {}", out.code))
    } else {
        AppError::msg(msg.join("\n"))
    }
}

/// Runs git and returns stdout, mapping a non-zero exit to `AppError::Msg(stderr)`.
pub(crate) async fn git_ok<I, S>(cwd: &Path, args: I) -> AppResult<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = git_raw(cwd, args).await?;
    if out.code != 0 {
        return Err(git_error(&out));
    }
    Ok(out.stdout)
}

/// Repo top-level for `cwd`, None when not inside a work tree.
pub(crate) async fn repo_root(cwd: &Path) -> AppResult<Option<PathBuf>> {
    if !cwd.is_dir() {
        return Ok(None);
    }
    let out = git_raw(cwd, ["rev-parse", "--show-toplevel"]).await?;
    if out.code != 0 {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if s.is_empty() { None } else { Some(PathBuf::from(s)) })
}

pub(crate) async fn require_root(cwd: &str) -> AppResult<PathBuf> {
    repo_root(Path::new(cwd)).await?.ok_or_else(|| AppError::msg(format!("Not a git repository: {cwd}")))
}

pub(crate) async fn has_head(root: &Path) -> AppResult<bool> {
    Ok(git_raw(root, ["rev-parse", "--verify", "-q", "HEAD"]).await?.code == 0)
}

// ---------- path validation ----------

/// Lexically resolves `.`/`..`; None if `..` climbs above the filesystem root.
fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() || out.as_os_str().is_empty() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// Canonicalizes the longest existing ancestor of `p`'s parent and re-appends the rest, so
/// symlinked prefixes (/var → /private/var) compare equal without following the final component.
fn canonical_parent(p: &Path) -> PathBuf {
    let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else { return p.to_path_buf() };
    let mut existing = parent.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = c;
            out.extend(rest.iter().rev());
            out.push(name);
            return out;
        }
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(n), Some(par)) => {
                rest.push(n);
                existing = par.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Resolves `p` (absolute, or relative to the repo root) to a `/`-separated path relative to
/// `root`. Rejects anything outside the repository. The repo root itself resolves to ".".
pub(crate) fn resolve_rel(root: &Path, p: &str) -> AppResult<String> {
    let escape = || AppError::msg(format!("Path is outside the repository: {p}"));
    if p.trim().is_empty() {
        return Err(AppError::msg("Empty path"));
    }
    let given = Path::new(p);
    let abs = if given.is_absolute() { given.to_path_buf() } else { root.join(given) };
    let norm = normalize(&abs).ok_or_else(escape)?;
    let root_c = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let rel = if let Ok(r) = norm.strip_prefix(root) {
        r.to_path_buf()
    } else {
        canonical_parent(&norm).strip_prefix(&root_c).map_err(|_| escape())?.to_path_buf()
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => Ok(s.to_string_lossy().into_owned()),
            _ => Err(escape()),
        })
        .collect::<AppResult<_>>()?;
    if parts.first().map(|s| s == ".git").unwrap_or(false) {
        return Err(AppError::msg("Refusing to operate on the .git directory"));
    }
    Ok(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

fn resolve_all(root: &Path, paths: &[String]) -> AppResult<Vec<String>> {
    paths.iter().map(|p| resolve_rel(root, p)).collect()
}

// ---------- status parsing ----------

#[derive(Debug, Default, Clone)]
pub(crate) struct ParsedStatus {
    pub head: Option<String>,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<GitFile>,
}

impl ParsedStatus {
    pub(super) fn branch(&self) -> Option<String> {
        match self.head.as_deref() {
            None => None,
            Some("(detached)") => {
                let sha = self.oid.as_deref().filter(|o| !o.starts_with('(')).unwrap_or("unknown");
                Some(format!("HEAD detached at {}", &sha[..sha.len().min(7)]))
            }
            Some(h) => Some(h.to_string()),
        }
    }
}

fn letter(c: char) -> String {
    if c == '.' { " ".into() } else { c.to_string() }
}

fn xy(s: &str) -> (String, String) {
    let mut ch = s.chars();
    (letter(ch.next().unwrap_or('.')), letter(ch.next().unwrap_or('.')))
}

/// Parses `git status --porcelain=v2 [--branch] -z` output.
pub(crate) fn parse_status_v2(out: &[u8]) -> ParsedStatus {
    let text = String::from_utf8_lossy(out);
    let mut st = ParsedStatus::default();
    let mut recs = text.split('\0').filter(|r| !r.is_empty());
    while let Some(rec) = recs.next() {
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(v) = h.strip_prefix("branch.oid ") {
                st.oid = Some(v.to_string());
            } else if let Some(v) = h.strip_prefix("branch.head ") {
                st.head = Some(v.to_string());
            } else if let Some(v) = h.strip_prefix("branch.upstream ") {
                st.upstream = Some(v.to_string());
            } else if let Some(v) = h.strip_prefix("branch.ab ") {
                for part in v.split_whitespace() {
                    if let Some(n) = part.strip_prefix('+') {
                        st.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix('-') {
                        st.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let kind = rec.as_bytes()[0];
        match kind {
            b'1' | b'2' | b'u' => {
                // fields before the path: 1 → 8, 2 → 9, u → 10
                let n = match kind { b'1' => 8, b'2' => 9, _ => 10 };
                let fields: Vec<&str> = rec.splitn(n + 1, ' ').collect();
                if fields.len() < n + 1 {
                    continue;
                }
                let (index, worktree) = xy(fields[1]);
                let old_path = if kind == b'2' { recs.next().map(|s| s.to_string()) } else { None };
                st.files.push(GitFile { path: fields[n].to_string(), old_path, index, worktree });
            }
            b'?' | b'!' if rec.len() > 2 => {
                let c = (kind as char).to_string();
                st.files.push(GitFile { path: rec[2..].to_string(), old_path: None, index: c.clone(), worktree: c });
            }
            _ => {}
        }
    }
    st
}

// ---------- status index for the file tree ----------

/// Per-path markers for `fs_list`, keyed by `/`-separated paths relative to the repo root.
#[derive(Debug, Default)]
pub(super) struct StatusIndex {
    files: HashMap<String, String>,
    dirs: HashMap<String, String>,
    /// Untracked directories reported collapsed ("dir/" with -unormal).
    untracked_dirs: HashSet<String>,
}

/// Single marker for a file: "??" untracked, else the most relevant letter (U, A, R, D, M…).
fn marker(f: &GitFile) -> String {
    let (x, y) = (f.index.as_str(), f.worktree.as_str());
    if x == "?" {
        return "??".into();
    }
    if x == "U" || y == "U" || (x == "A" && y == "A") || (x == "D" && y == "D") {
        return "U".into();
    }
    for l in ["A", "R", "C", "D"] {
        if x == l || y == l {
            return l.into();
        }
    }
    "M".into()
}

impl StatusIndex {
    pub(super) fn from_status(st: &ParsedStatus) -> Self {
        let mut idx = StatusIndex::default();
        for f in &st.files {
            if f.index == "!" {
                continue;
            }
            let m = marker(f);
            let path = f.path.trim_end_matches('/');
            if f.path.ends_with('/') {
                idx.untracked_dirs.insert(path.to_string());
                idx.dirs.insert(path.to_string(), m);
            } else {
                idx.files.insert(path.to_string(), m);
            }
            let mut cur = path;
            while let Some(pos) = cur.rfind('/') {
                cur = &cur[..pos];
                idx.dirs.entry(cur.to_string()).or_insert_with(|| "M".into());
            }
        }
        idx
    }

    pub(super) fn lookup(&self, rel: &str, is_dir: bool) -> Option<String> {
        let own = if is_dir { self.dirs.get(rel) } else { self.files.get(rel) };
        if let Some(m) = own {
            return Some(m.clone());
        }
        let mut cur = rel;
        while let Some(pos) = cur.rfind('/') {
            cur = &cur[..pos];
            if self.untracked_dirs.contains(cur) {
                return Some("??".into());
            }
        }
        None
    }
}

static STATUS_CACHE: Lazy<Mutex<HashMap<PathBuf, (Instant, Arc<StatusIndex>)>>> = Lazy::new(Default::default);

/// Cached (~2s) status markers for the repo at `root`. None if git fails.
pub(super) async fn status_index(root: &Path) -> Option<Arc<StatusIndex>> {
    {
        let mut cache = STATUS_CACHE.lock();
        cache.retain(|_, (t, _)| t.elapsed() < STATUS_CACHE_TTL);
        if let Some((_, idx)) = cache.get(root) {
            return Some(idx.clone());
        }
    }
    let out = git_ok(root, ["status", "--porcelain=v2", "-z", "-unormal"]).await.ok()?;
    let idx = Arc::new(StatusIndex::from_status(&parse_status_v2(&out)));
    STATUS_CACHE.lock().insert(root.to_path_buf(), (Instant::now(), idx.clone()));
    Some(idx)
}

pub(crate) fn invalidate_cache() {
    STATUS_CACHE.lock().clear();
}

// ---------- commands ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_status(cwd: String) -> AppResult<GitStatus> {
    if let Some(t) = super::remote::Target::parse(&cwd) {
        return super::remote::git_status(&t).await;
    }
    let Some(root) = repo_root(Path::new(&cwd)).await? else {
        return Ok(GitStatus { is_repo: false, root: None, branch: None, ahead: 0, behind: 0, files: vec![] });
    };
    let out = git_ok(&root, ["status", "--porcelain=v2", "--branch", "-z", "-uall"]).await?;
    let st = parse_status_v2(&out);
    Ok(GitStatus {
        is_repo: true,
        root: Some(root.to_string_lossy().into_owned()),
        branch: st.branch(),
        ahead: st.ahead,
        behind: st.behind,
        files: st.files,
    })
}

/// Blob contents for a revision spec like `HEAD:path` / `:path`; None if it doesn't exist.
pub(crate) async fn read_blob(root: &Path, spec: &str) -> AppResult<Option<Vec<u8>>> {
    let size = git_raw(root, ["cat-file", "-s", spec]).await?;
    if size.code != 0 {
        return Ok(None);
    }
    let n: u64 = String::from_utf8_lossy(&size.stdout).trim().parse().unwrap_or(0);
    if n > VERSION_CAP {
        return Err(AppError::msg(format!("File too large to diff ({} MB)", n / (1024 * 1024))));
    }
    Ok(Some(git_ok(root, ["cat-file", "blob", spec]).await?))
}

/// Worktree contents (symlinks as their target, like git stores them); None if missing.
fn read_worktree(path: &Path) -> AppResult<Option<Vec<u8>>> {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return Ok(None) };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        return Ok(Some(target.to_string_lossy().into_owned().into_bytes()));
    }
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > VERSION_CAP {
        return Err(AppError::msg(format!("File too large to diff ({} MB)", meta.len() / (1024 * 1024))));
    }
    Ok(Some(std::fs::read(path)?))
}

/// original = HEAD (or index when `staged` is false and the file is staged), modified = index (staged) or worktree.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_file_versions(cwd: String, path: String, staged: bool) -> AppResult<FileVersions> {
    let root = require_root(&cwd).await?;
    let rel = resolve_rel(&root, &path)?;
    let head_spec = format!("HEAD:{rel}");
    let index_spec = format!(":{rel}");
    let (original, modified) = if staged {
        (read_blob(&root, &head_spec).await?, read_blob(&root, &index_spec).await?)
    } else {
        let original = match read_blob(&root, &index_spec).await? {
            Some(b) => Some(b),
            None => read_blob(&root, &head_spec).await?,
        };
        (original, read_worktree(&root.join(&rel))?)
    };
    let (original, modified) = (original.unwrap_or_default(), modified.unwrap_or_default());
    if original.contains(&0) || modified.contains(&0) {
        return Ok(FileVersions { original: String::new(), modified: String::new(), binary: true });
    }
    Ok(FileVersions {
        original: String::from_utf8_lossy(&original).into_owned(),
        modified: String::from_utf8_lossy(&modified).into_owned(),
        binary: false,
    })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_diff_text(cwd: String, staged: bool) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
    if staged {
        args.push("--cached");
    }
    let mut out = git_ok(&root, &args).await?;
    if !staged {
        let list = git_ok(&root, ["ls-files", "--others", "--exclude-standard", "-z"]).await?;
        let list = String::from_utf8_lossy(&list).into_owned();
        let files: Vec<&str> = list.split('\0').filter(|s| !s.is_empty()).collect();
        for (i, f) in files.iter().enumerate() {
            if out.len() >= DIFF_CAP || i >= UNTRACKED_DIFF_FILES {
                out.extend_from_slice(format!("\n# … {} more untracked file(s) not shown\n", files.len() - i).as_bytes());
                break;
            }
            let too_big = std::fs::metadata(root.join(f)).map(|m| m.len() > VERSION_CAP).unwrap_or(false);
            if too_big {
                out.extend_from_slice(format!("diff --git a/{f} b/{f}\nnew file mode 100644\n# file too large to show\n").as_bytes());
                continue;
            }
            let d = git_raw(&root, ["diff", "--no-color", "--no-ext-diff", "--no-index", "--", "/dev/null", f]).await?;
            // --no-index exits 1 when the files differ
            if d.code > 1 {
                return Err(git_error(&d));
            }
            out.extend_from_slice(&d.stdout);
        }
    }
    if out.len() > DIFF_CAP {
        out.truncate(DIFF_CAP);
        out.extend_from_slice(b"\n# diff truncated\n");
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn with_paths<'a>(mut args: Vec<&'a str>, rels: &'a [String]) -> Vec<&'a str> {
    args.push("--");
    args.extend(rels.iter().map(|s| s.as_str()));
    args
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_stage(cwd: String, paths: Vec<String>) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let rels = resolve_all(&root, &paths)?;
    if rels.is_empty() {
        return Ok(());
    }
    invalidate_cache();
    git_ok(&root, with_paths(vec!["--literal-pathspecs", "add", "-A"], &rels)).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_unstage(cwd: String, paths: Vec<String>) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let rels = resolve_all(&root, &paths)?;
    if rels.is_empty() {
        return Ok(());
    }
    invalidate_cache();
    let args = if has_head(&root).await? {
        vec!["--literal-pathspecs", "restore", "--staged"]
    } else {
        vec!["--literal-pathspecs", "rm", "--cached", "-r", "-q", "--ignore-unmatch"]
    };
    git_ok(&root, with_paths(args, &rels)).await?;
    Ok(())
}

/// Removes an untracked/new file from the worktree (files and symlinks only, inside `root`).
fn delete_worktree_file(root: &Path, rel: &str) -> AppResult<()> {
    let rel = resolve_rel(root, rel)?;
    let path = root.join(&rel);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() || m.file_type().is_symlink() => Ok(std::fs::remove_file(&path)?),
        _ => Ok(()),
    }
}

/// Discard worktree changes (and delete untracked files) for `paths`.
///
/// - untracked → file deleted
/// - staged-new (index `A`) → `rm --cached` + file deleted
/// - worktree changes → restored from the index (`restore --worktree`)
/// - staged-only changes → reverted to HEAD (`restore --source=HEAD --staged --worktree`)
///
/// `include_staged`: also drop staged changes of tracked files (back to HEAD), not just the worktree part.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_discard(cwd: String, paths: Vec<String>, include_staged: Option<bool>) -> AppResult<()> {
    let include_staged = include_staged.unwrap_or(false);
    let root = require_root(&cwd).await?;
    let rels = resolve_all(&root, &paths)?;
    if rels.is_empty() {
        return Ok(());
    }
    invalidate_cache();
    let out = git_ok(&root, with_paths(vec!["--literal-pathspecs", "status", "--porcelain=v2", "-z", "-uall"], &rels)).await?;
    let st = parse_status_v2(&out);
    let (mut untracked, mut added, mut worktree, mut staged_only) = (vec![], vec![], vec![], vec![]);
    for f in st.files {
        match (f.index.as_str(), f.worktree.as_str()) {
            ("?", _) => untracked.push(f.path),
            ("!", _) => {}
            ("A", _) => added.push(f.path),
            ("R" | "C", _) if include_staged => {
                // The new name isn't in HEAD: drop it; a rename also restores the old path.
                if f.index == "R" {
                    staged_only.extend(f.old_path.clone());
                }
                added.push(f.path);
            }
            (_, " ") => staged_only.push(f.path),
            _ if include_staged => staged_only.push(f.path),
            _ => worktree.push(f.path),
        }
    }
    if !added.is_empty() {
        git_ok(&root, with_paths(vec!["--literal-pathspecs", "rm", "--cached", "-q", "-f"], &added)).await?;
    }
    if !worktree.is_empty() {
        git_ok(&root, with_paths(vec!["--literal-pathspecs", "restore", "--worktree"], &worktree)).await?;
    }
    if !staged_only.is_empty() {
        git_ok(&root, with_paths(vec!["--literal-pathspecs", "restore", "--source=HEAD", "--staged", "--worktree"], &staged_only)).await?;
    }
    for f in untracked.iter().chain(added.iter()) {
        delete_worktree_file(&root, f)?;
    }
    Ok(())
}

/// Discard every change: staged and unstaged edits go back to HEAD; with `include_untracked`
/// untracked files and folders are deleted too (ignored files and nested repos are kept).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_discard_all(cwd: String, include_untracked: bool) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    if !has_head(&root).await? {
        return Err(AppError::msg("The repository has no commits yet; nothing to go back to"));
    }
    invalidate_cache();
    git_ok(&root, ["restore", "--source=HEAD", "--staged", "--worktree", "--", ":/"]).await?;
    if include_untracked {
        git_ok(&root, ["clean", "-f", "-d", "-q", "--", ":/"]).await?;
    }
    Ok(())
}

/// Commit the index, or with `paths` only those files (their current contents, staging
/// untracked/deleted ones first); other staged changes stay staged.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_commit(cwd: String, message: String, paths: Option<Vec<String>>) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    if message.trim().is_empty() {
        return Err(AppError::msg("Commit message is empty"));
    }
    if let Some(paths) = paths.filter(|p| !p.is_empty()) {
        let rels = resolve_all(&root, &paths)?;
        invalidate_cache();
        git_ok(&root, with_paths(vec!["--literal-pathspecs", "add", "-A"], &rels)).await?;
        let out = git_raw(&root, with_paths(vec!["--literal-pathspecs", "commit", "-q", "-m", message.as_str(), "--only"], &rels)).await?;
        if out.code != 0 {
            return Err(git_error(&out));
        }
        let sha = git_ok(&root, ["rev-parse", "--short", "HEAD"]).await?;
        return Ok(String::from_utf8_lossy(&sha).trim().to_string());
    }
    let staged = git_raw(&root, ["diff", "--cached", "--quiet"]).await?;
    match staged.code {
        0 => return Err(AppError::msg("Nothing staged to commit")),
        1 => {}
        _ => return Err(git_error(&staged)),
    }
    invalidate_cache();
    git_ok(&root, ["commit", "-q", "-m", &message]).await?;
    let sha = git_ok(&root, ["rev-parse", "--short", "HEAD"]).await?;
    Ok(String::from_utf8_lossy(&sha).trim().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Throwaway repo under the system temp dir; removed on drop.
    pub struct TempRepo {
        pub dir: PathBuf,
    }

    impl TempRepo {
        pub fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("devcat-git-{name}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let dir = std::fs::canonicalize(&dir).unwrap();
            let r = TempRepo { dir };
            r.git(&["init", "-q", "-b", "main"]);
            r.git(&["config", "user.name", "DevCat Test"]);
            r.git(&["config", "user.email", "test@devcat.invalid"]);
            r.git(&["config", "commit.gpgsign", "false"]);
            r
        }
        pub fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new(&*GIT_BIN)
                .arg("-C")
                .arg(&self.dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        pub fn write(&self, rel: &str, content: &str) {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        pub fn cwd(&self) -> String {
            self.dir.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn find<'a>(st: &'a GitStatus, path: &str) -> Option<&'a GitFile> {
        st.files.iter().find(|f| f.path == path)
    }

    #[test]
    fn parses_v2_fixture() {
        let fixture = "# branch.oid 1234567890abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +3 -2\0\
1 .M N... 100644 100644 100644 aaa aaa src/a.rs\0\
1 A. N... 000000 100644 100644 000 bbb new file.txt\0\
2 R. N... 100644 100644 100644 ccc ccc R100 renamed.rs\0orig.rs\0\
u UU N... 100644 100644 100644 100644 d1 d2 d3 conflict.txt\0\
? untracked/x.txt\0! ignored.log\0";
        let st = parse_status_v2(fixture.as_bytes());
        assert_eq!(st.branch().as_deref(), Some("main"));
        assert_eq!((st.ahead, st.behind), (3, 2));
        assert_eq!(st.files.len(), 6);
        assert_eq!((st.files[0].index.as_str(), st.files[0].worktree.as_str()), (" ", "M"));
        assert_eq!(st.files[1].path, "new file.txt");
        assert_eq!(st.files[1].index, "A");
        assert_eq!(st.files[2].path, "renamed.rs");
        assert_eq!(st.files[2].old_path.as_deref(), Some("orig.rs"));
        assert_eq!(st.files[3].index, "U");
        assert_eq!((st.files[4].index.as_str(), st.files[4].worktree.as_str()), ("?", "?"));
        assert_eq!(st.files[5].index, "!");

        let detached = parse_status_v2(b"# branch.oid abcdef1234\0# branch.head (detached)\0");
        assert_eq!(detached.branch().as_deref(), Some("HEAD detached at abcdef1"));
        let initial = parse_status_v2(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(initial.branch().as_deref(), Some("main"));
    }

    #[test]
    fn status_index_markers() {
        let st = parse_status_v2(b"1 .M N... 1 1 1 a a src/deep/a.rs\0? newdir/\0? top.txt\01 A. N... 0 1 1 0 b src/b.rs\0");
        let idx = StatusIndex::from_status(&st);
        assert_eq!(idx.lookup("src/deep/a.rs", false).as_deref(), Some("M"));
        assert_eq!(idx.lookup("src/b.rs", false).as_deref(), Some("A"));
        assert_eq!(idx.lookup("src", true).as_deref(), Some("M"));
        assert_eq!(idx.lookup("src/deep", true).as_deref(), Some("M"));
        assert_eq!(idx.lookup("newdir", true).as_deref(), Some("??"));
        assert_eq!(idx.lookup("newdir/inner/f.txt", false).as_deref(), Some("??"));
        assert_eq!(idx.lookup("top.txt", false).as_deref(), Some("??"));
        assert_eq!(idx.lookup("clean.rs", false), None);
    }

    #[test]
    fn rejects_path_escapes() {
        let repo = TempRepo::new("escape");
        let root = repo.dir.clone();
        assert_eq!(resolve_rel(&root, "a/b.txt").unwrap(), "a/b.txt");
        assert_eq!(resolve_rel(&root, "a/../b.txt").unwrap(), "b.txt");
        assert_eq!(resolve_rel(&root, &root.join("x/y").to_string_lossy()).unwrap(), "x/y");
        assert!(resolve_rel(&root, "../outside.txt").is_err());
        assert!(resolve_rel(&root, "a/../../outside").is_err());
        assert!(resolve_rel(&root, "/etc/passwd").is_err());
        assert!(resolve_rel(&root, ".git/config").is_err());
        assert!(resolve_rel(&root, "").is_err());
    }

    #[tokio::test]
    async fn not_a_repo() {
        let dir = std::env::temp_dir().join(format!("devcat-norepo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = git_status(dir.to_string_lossy().into_owned()).await.unwrap();
        assert!(!st.is_repo);
        assert!(git_commit(dir.to_string_lossy().into_owned(), "x".into(), None).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_and_versions() {
        let repo = TempRepo::new("status");
        repo.write("keep.txt", "one\n");
        repo.write("gone.txt", "bye\n");
        repo.write("old.txt", "rename me\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "init"]);

        repo.write("keep.txt", "one\ntwo\n");
        repo.git(&["add", "keep.txt"]);
        repo.write("keep.txt", "one\ntwo\nthree\n");
        std::fs::remove_file(repo.dir.join("gone.txt")).unwrap();
        repo.git(&["mv", "old.txt", "new.txt"]);
        repo.write("fresh.txt", "staged new\n");
        repo.git(&["add", "fresh.txt"]);
        repo.write("dir/untracked.txt", "u\n");
        std::fs::write(repo.dir.join("bin.dat"), [0u8, 1, 2]).unwrap();

        let st = git_status(repo.cwd()).await.unwrap();
        assert!(st.is_repo);
        assert_eq!(st.branch.as_deref(), Some("main"));
        let keep = find(&st, "keep.txt").unwrap();
        assert_eq!((keep.index.as_str(), keep.worktree.as_str()), ("M", "M"));
        assert_eq!(find(&st, "gone.txt").unwrap().worktree, "D");
        let ren = find(&st, "new.txt").unwrap();
        assert_eq!(ren.index, "R");
        assert_eq!(ren.old_path.as_deref(), Some("old.txt"));
        assert_eq!(find(&st, "dir/untracked.txt").unwrap().index, "?");

        let staged = git_file_versions(repo.cwd(), "keep.txt".into(), true).await.unwrap();
        assert_eq!((staged.original.as_str(), staged.modified.as_str()), ("one\n", "one\ntwo\n"));
        let unstaged = git_file_versions(repo.cwd(), "keep.txt".into(), false).await.unwrap();
        assert_eq!((unstaged.original.as_str(), unstaged.modified.as_str()), ("one\ntwo\n", "one\ntwo\nthree\n"));
        let new = git_file_versions(repo.cwd(), "fresh.txt".into(), true).await.unwrap();
        assert_eq!((new.original.as_str(), new.modified.as_str()), ("", "staged new\n"));
        let deleted = git_file_versions(repo.cwd(), "gone.txt".into(), false).await.unwrap();
        assert_eq!((deleted.original.as_str(), deleted.modified.as_str()), ("bye\n", ""));
        let untracked = git_file_versions(repo.cwd(), "dir/untracked.txt".into(), false).await.unwrap();
        assert_eq!((untracked.original.as_str(), untracked.modified.as_str()), ("", "u\n"));
        let abs = repo.dir.join("keep.txt").to_string_lossy().into_owned();
        assert_eq!(git_file_versions(repo.cwd(), abs, false).await.unwrap().modified, "one\ntwo\nthree\n");
        assert!(git_file_versions(repo.cwd(), "bin.dat".into(), false).await.unwrap().binary);
        assert!(git_file_versions(repo.cwd(), "../x".into(), false).await.is_err());

        let unstaged_diff = git_diff_text(repo.cwd(), false).await.unwrap();
        assert!(unstaged_diff.contains("+three"));
        assert!(unstaged_diff.contains("dir/untracked.txt"));
        assert!(unstaged_diff.contains("+u"));
        let staged_diff = git_diff_text(repo.cwd(), true).await.unwrap();
        assert!(staged_diff.contains("+two"));
        assert!(!staged_diff.contains("+three"));
        assert!(staged_diff.contains("+staged new"));
    }

    #[tokio::test]
    async fn ahead_behind_from_upstream() {
        let repo = TempRepo::new("ab");
        repo.write("a.txt", "a\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.git(&["branch", "upstream"]);
        repo.git(&["branch", "--set-upstream-to=upstream"]);
        repo.write("a.txt", "b\n");
        repo.git(&["commit", "-q", "-am", "local"]);
        let st = git_status(repo.cwd()).await.unwrap();
        assert_eq!((st.ahead, st.behind), (1, 0));
        repo.git(&["checkout", "-q", "--detach"]);
        let st = git_status(repo.cwd()).await.unwrap();
        assert!(st.branch.unwrap().starts_with("HEAD detached at "));
    }

    #[tokio::test]
    async fn stage_unstage_discard_commit() {
        let repo = TempRepo::new("ops");
        // no HEAD yet: stage + unstage via rm --cached
        repo.write("first.txt", "1\n");
        git_stage(repo.cwd(), vec!["first.txt".into()]).await.unwrap();
        assert_eq!(find(&git_status(repo.cwd()).await.unwrap(), "first.txt").unwrap().index, "A");
        git_unstage(repo.cwd(), vec!["first.txt".into()]).await.unwrap();
        assert_eq!(find(&git_status(repo.cwd()).await.unwrap(), "first.txt").unwrap().index, "?");
        assert!(git_commit(repo.cwd(), "empty".into(), None).await.unwrap_err().to_string().contains("Nothing staged"));

        git_stage(repo.cwd(), vec!["first.txt".into()]).await.unwrap();
        let sha = git_commit(repo.cwd(), "first".into(), None).await.unwrap();
        assert!(sha.len() >= 7);
        assert_eq!(repo.git(&["rev-parse", "--short", "HEAD"]).trim(), sha);
        assert!(git_status(repo.cwd()).await.unwrap().files.is_empty());

        // discard: worktree edit, staged-new, untracked, staged-only
        repo.write("first.txt", "changed\n");
        repo.write("added.txt", "a\n");
        git_stage(repo.cwd(), vec!["added.txt".into()]).await.unwrap();
        repo.write("junk/untracked.txt", "x\n");
        git_discard(repo.cwd(), vec!["first.txt".into(), "added.txt".into(), "junk".into()], None).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("first.txt")).unwrap(), "1\n");
        assert!(!repo.dir.join("added.txt").exists());
        assert!(!repo.dir.join("junk/untracked.txt").exists());
        assert!(git_status(repo.cwd()).await.unwrap().files.is_empty());

        repo.write("first.txt", "staged change\n");
        git_stage(repo.cwd(), vec!["first.txt".into()]).await.unwrap();
        git_discard(repo.cwd(), vec!["first.txt".into()], None).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("first.txt")).unwrap(), "1\n");
        assert!(git_status(repo.cwd()).await.unwrap().files.is_empty());

        // escapes are rejected before git runs
        assert!(git_stage(repo.cwd(), vec!["../evil".into()]).await.is_err());
        assert!(git_discard(repo.cwd(), vec!["/etc/hosts".into()], None).await.is_err());

        // commit only selected files; other staged changes stay staged
        repo.write("a.txt", "a\n");
        repo.write("b.txt", "b\n");
        repo.write("c.txt", "c\n");
        repo.git(&["add", "c.txt"]);
        git_commit(repo.cwd(), "only a and b".into(), Some(vec!["a.txt".into(), "b.txt".into()])).await.unwrap();
        assert_eq!(repo.git(&["show", "--name-only", "--format=", "HEAD"]).trim(), "a.txt\nb.txt");
        assert!(repo.git(&["status", "--porcelain"]).contains("A  c.txt"));

        // discard selected incl. staged changes
        repo.write("a.txt", "a2\n");
        repo.git(&["add", "a.txt"]);
        repo.write("a.txt", "a3\n");
        git_discard(repo.cwd(), vec!["a.txt".into()], Some(true)).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("a.txt")).unwrap(), "a\n");
        assert!(!repo.git(&["status", "--porcelain"]).contains("a.txt"));

        // discard all: tracked edits, staged new file, untracked dir; ignored files survive
        repo.write("b.txt", "changed\n");
        repo.write(".gitignore", "*.log\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "-qm", "ignore"]);
        repo.write("keep.log", "x");
        repo.write("newdir/x.txt", "x");
        repo.write("d.txt", "d\n");
        repo.git(&["add", "d.txt"]);
        git_discard_all(repo.cwd(), false).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("b.txt")).unwrap(), "b\n");
        assert!(!repo.dir.join("d.txt").exists(), "staged new file removed");
        assert!(repo.dir.join("newdir/x.txt").exists(), "untracked kept without include_untracked");
        git_discard_all(repo.cwd(), true).await.unwrap();
        assert!(!repo.dir.join("newdir").exists());
        assert!(repo.dir.join("keep.log").exists(), "ignored files kept");
        assert_eq!(repo.git(&["status", "--porcelain"]).trim(), "");
    }
}
