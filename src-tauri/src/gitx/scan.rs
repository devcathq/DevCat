//! Nested repository discovery for `git_repos`.
use super::*;
use crate::claude::git::{git_raw, parse_status_v2};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub(super) const DEFAULT_DEPTH: u32 = 6;
pub(super) const MAX_DEPTH: u32 = 12;
/// Upper bound on directories read per scan.
const MAX_VISITED: usize = 50_000;
/// Concurrent `git status` processes.
const STATUS_CONCURRENCY: usize = 8;

/// Directories never descended into (besides every hidden directory).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "venv",
    "__pycache__",
    "dist",
    "build",
    "vendor",
    "Pods",
    "DerivedData",
    "bower_components",
    "site-packages",
];

/// A repo found on disk: work tree path, whether `.git` is a file (submodule / linked worktree).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Found {
    pub path: PathBuf,
    pub is_submodule: bool,
}

/// `.git` kind inside `dir`: Some(false) for a directory, Some(true) for a file.
fn git_entry(dir: &Path) -> Option<bool> {
    let meta = std::fs::symlink_metadata(dir.join(".git")).ok()?;
    if meta.is_dir() {
        Some(false)
    } else if meta.is_file() {
        Some(true)
    } else {
        None
    }
}

fn skipped(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// Breadth-first walk of `root` (depth 0) down to `max_depth`, never following symlinks.
/// Repos keep being descended into so nested repos / submodules are found.
pub(super) fn walk(root: &Path, max_depth: u32) -> Vec<Found> {
    let mut found = Vec::new();
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::from([(root.to_path_buf(), 0)]);
    let mut visited = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_VISITED {
            break;
        }
        if let Some(is_submodule) = git_entry(&dir) {
            found.push(Found { path: dir.clone(), is_submodule });
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut children: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)) // DirEntry::file_type doesn't follow symlinks
            .filter(|e| !skipped(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        children.sort();
        queue.extend(children.into_iter().map(|c| (c, depth + 1)));
    }
    found
}

/// `rel` of `path` under `root` with '/' separators ("." for the root itself).
fn rel_of(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The repo enclosing `root` when `root` is below its top level: (path, levels up).
async fn enclosing_repo(root: &Path) -> Option<(PathBuf, usize)> {
    let out = git_raw(root, ["rev-parse", "--show-toplevel"]).await.ok()?;
    if out.code != 0 {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    let canon_root = std::fs::canonicalize(root).ok()?;
    let below = canon_root.strip_prefix(&top).ok()?;
    let levels = below.components().count();
    if levels == 0 {
        return None;
    }
    // Derive the path lexically from `root` so it shares the caller's prefix (no /private/var surprises).
    let mut path = root.to_path_buf();
    for _ in 0..levels {
        path = path.parent()?.to_path_buf();
    }
    Some((path, levels))
}

async fn fill_status(repo: GitRepo) -> GitRepo {
    let dir = PathBuf::from(&repo.path);
    let out = git_raw(&dir, ["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=normal"]).await;
    with_status(repo, out)
}

/// `repo` with what its `git status --porcelain=v2 --branch -z` says.
fn with_status(mut repo: GitRepo, out: AppResult<crate::claude::git::GitOut>) -> GitRepo {
    match out {
        Ok(out) if out.code == 0 => {
            let st = parse_status_v2(&out.stdout);
            repo.head_sha = st.oid.filter(|o| !o.starts_with('('));
            repo.branch = st.head.filter(|h| h != "(detached)");
            repo.upstream = st.upstream;
            repo.ahead = st.ahead;
            repo.behind = st.behind;
            repo.changes = st.files.iter().filter(|f| f.index != "!").count() as u32;
        }
        Ok(out) => repo.error = Some(crate::claude::git::git_error(&out).to_string()),
        Err(e) => repo.error = Some(e.to_string()),
    }
    repo
}

/// Repos under (and enclosing) `root`, with branch / sync / change info.
pub(super) async fn scan(root: &Path, max_depth: u32) -> AppResult<Vec<GitRepo>> {
    // drops trailing separators / "." so result paths share one spelling
    let root: PathBuf = root.components().collect();
    let root = root.as_path();
    if !root.is_dir() {
        return Err(AppError::msg(format!("Not a directory: {}", root.display())));
    }
    let depth = max_depth.min(MAX_DEPTH);
    let walk_root = root.to_path_buf();
    let mut found = crate::rt::spawn_blocking(move || walk(&walk_root, depth))
        .await
        .map_err(|e| AppError::msg(e.to_string()))?;
    let mut rels: HashMap<PathBuf, String> = found.iter().map(|f| (f.path.clone(), rel_of(root, &f.path))).collect();
    if git_entry(root).is_none()
        && let Some((path, levels)) = enclosing_repo(root).await
    {
        rels.insert(path.clone(), vec![".."; levels].join("/"));
        found.push(Found { is_submodule: git_entry(&path) == Some(true), path });
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));

    let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
    let repos: Vec<GitRepo> = found
        .iter()
        .map(|f| {
            let parent = paths
                .iter()
                .filter(|p| **p != f.path && f.path.starts_with(p))
                .max_by_key(|p| p.components().count())
                .map(|p| p.to_string_lossy().into_owned());
            GitRepo {
                path: f.path.to_string_lossy().into_owned(),
                name: f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.path.to_string_lossy().into_owned()),
                rel: rels.get(&f.path).cloned().unwrap_or_default(),
                parent,
                is_submodule: f.is_submodule,
                branch: None,
                upstream: None,
                ahead: 0,
                behind: 0,
                changes: 0,
                head_sha: None,
                error: None,
            }
        })
        .collect();
    let mut repos: Vec<GitRepo> = futures::stream::iter(repos.into_iter().map(fill_status)).buffer_unordered(STATUS_CONCURRENCY).collect().await;
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(repos)
}

/// `path` is `dir` or inside it (`/`-separated paths on a host).
fn within(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/')) || dir.is_empty()
}

/// `scan` for a folder on an SSH host: the host walks and reads every status in one go
/// (`remote::git_scan`); paths come back as `ssh://` URIs, so each opens in a Git tab like a local one.
pub(super) async fn scan_remote(t: &crate::claude::remote::Target, max_depth: u32) -> AppResult<Vec<GitRepo>> {
    let (root, found) = crate::claude::remote::git_scan(t, max_depth.min(MAX_DEPTH), SKIP_DIRS).await?;
    let depth = |p: &str| p.split('/').filter(|s| !s.is_empty()).count();
    let paths: Vec<String> = found.iter().map(|f| f.path.clone()).collect();
    let mut repos: Vec<GitRepo> = found
        .into_iter()
        .map(|f| {
            let parent = paths.iter().filter(|p| **p != f.path && within(&f.path, p)).max_by_key(|p| p.len()).map(|p| t.uri(p));
            let rel = if f.enclosing {
                vec![".."; depth(&root).saturating_sub(depth(&f.path)).max(1)].join("/")
            } else {
                match f.path.strip_prefix(root.trim_end_matches('/')).map(|r| r.trim_start_matches('/')) {
                    Some("") => ".".to_string(),
                    Some(r) => r.to_string(),
                    None => f.path.clone(),
                }
            };
            let name = f.path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&f.path).to_string();
            let repo = GitRepo {
                path: t.uri(&f.path),
                name,
                rel,
                parent,
                is_submodule: f.is_submodule,
                branch: None,
                upstream: None,
                ahead: 0,
                behind: 0,
                changes: 0,
                head_sha: None,
                error: None,
            };
            with_status(repo, Ok(f.status))
        })
        .collect();
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(repos)
}
