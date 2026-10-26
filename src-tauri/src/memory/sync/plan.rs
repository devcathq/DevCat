//! What memory sync compares and decides, apart from the network: which files a computer shares
//! and the versions of each it has seen (the index), what a place is called on every computer (its
//! key), what to do with a file when two computers differ, and writing what arrives.
//!
//! - **Shared files:** in each place, the memories (`*.md`) and open handoffs (`handoffs/*.md`).
//!   Not `place.json` (it holds this computer's project id or path), not taken handoffs, not dot
//!   files or symlinks.
//! - **Keys:** a place is matched across computers by what it is, not where: `global`; a project or
//!   repository by its git remote (`git:github.com/acme/shop`, the same on every clone whatever
//!   the folder); a project without a remote by its name (`project:shop`). A repository in no
//!   project and without a remote has nothing stable to match, so it stays on this computer.
//! - **Versions:** each file's entry keeps the hashes of the versions it replaced (`seen`). A
//!   version that has seen the other side's is newer (take it); neither having seen the other's
//!   is a conflict: the newer one wins and the other is kept next to it. A deleted file stays in
//!   the index as a tombstone (no hash) for [`TOMBSTONE_DAYS`], so the deletion travels, but a
//!   deletion never wins over an edit it hasn't seen: no memory is lost.
use super::super::{MAX_BODY, Place, cap, hash, scrub, slug, split_front, write_atomic};
use crate::models::Project;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Largest file taken from another computer (a memory body is capped at `MAX_BODY`; the rest is
/// frontmatter).
pub const MAX_FILE: usize = MAX_BODY + 16 * 1024;
/// Versions of a file remembered (short hashes).
const SEEN: usize = 16;
/// How long a deletion is remembered.
pub const TOMBSTONE_DAYS: i64 = 90;
/// Hex digits of a hash kept in `seen`.
const SHORT: usize = 16;

/// One shared file as a computer has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// The place's key (see the module docs).
    pub place: String,
    /// In the place's folder: `name.md` or `handoffs/name.md`.
    pub path: String,
    /// SHA-256 of the content; none: deleted (a tombstone).
    #[serde(default)]
    pub hash: Option<String>,
    /// When this version was made (ms; for a tombstone, when it was deleted).
    pub mtime: i64,
    #[serde(default)]
    pub size: u64,
    /// Short hashes of the versions this one replaced, oldest first.
    #[serde(default)]
    pub seen: Vec<String>,
}

/// A file in the index (keyed by its path under the memory folder).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub hash: Option<String>,
    pub mtime: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub seen: Vec<String>,
}

impl Entry {
    /// A new version (none: deleted) replaces the current one.
    fn advance(&mut self, hash: Option<String>, mtime: i64, size: u64) {
        if let Some(old) = self.hash.take() {
            push_seen(&mut self.seen, short(&old));
        }
        if let Some(h) = &hash {
            self.seen.retain(|s| s != short(h));
        }
        self.hash = hash;
        self.mtime = mtime;
        self.size = size;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub files: BTreeMap<String, Entry>,
}

pub fn short(h: &str) -> &str {
    &h[..h.len().min(SHORT)]
}

fn push_seen(seen: &mut Vec<String>, h: &str) {
    seen.retain(|s| s != h);
    seen.push(h.to_string());
    if seen.len() > SEEN {
        seen.drain(..seen.len() - SEEN);
    }
}

// ---------- which files

/// A file name sync shares: markdown, no folders, no dot files (`.credentials.md`, temp files).
pub fn shared_name(name: &str) -> bool {
    name.ends_with(".md") && name.len() > 3 && name.len() <= 120 && !name.starts_with('.') && !name.contains(['/', '\\', ':']) && !name.chars().any(char::is_control)
}

/// A path in a place sync shares (also checks what other computers send).
pub fn shared_path(path: &str) -> bool {
    match path.split_once('/') {
        None => shared_name(path),
        Some(("handoffs", name)) => shared_name(name),
        _ => false,
    }
}

/// The places under the memory folder: (folder relative to it, place).
pub fn place_dirs(root: &Path) -> Vec<(String, Place)> {
    let mut out = vec![("global".to_string(), Place::Global)];
    for sub in ["projects", "folders"] {
        let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
        let mut found: Vec<(String, Place)> = rd
            .flatten()
            .filter_map(|e| {
                let place: Place = serde_json::from_str(&std::fs::read_to_string(e.path().join("place.json")).ok()?).ok()?;
                Some((format!("{sub}/{}", e.file_name().to_string_lossy()), place))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(found);
    }
    out
}

/// The shared files in a place's folder (paths relative to it). Symlinks are skipped: a link
/// named like a memory could point at a key file.
pub fn files_in(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (sub, prefix) in [(dir.to_path_buf(), ""), (dir.join("handoffs"), "handoffs/")] {
        let Ok(rd) = std::fs::read_dir(&sub) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if shared_name(&name) && e.file_type().is_ok_and(|t| t.is_file()) {
                out.push(format!("{prefix}{name}"));
            }
        }
    }
    out.sort();
    out
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

/// Bring the index up to date with the files: new and edited files get a new version, missing
/// ones become tombstones, old tombstones are dropped. Whether anything changed.
pub fn scan(root: &Path, index: &mut Index, now: i64) -> bool {
    let mut changed = false;
    let mut found = BTreeSet::new();
    for (dir, _) in place_dirs(root) {
        for rel in files_in(&root.join(&dir)) {
            let local = format!("{dir}/{rel}");
            let path = root.join(&local);
            let (Ok(bytes), Ok(meta)) = (std::fs::read(&path), std::fs::symlink_metadata(&path)) else { continue };
            found.insert(local.clone());
            let h = hash(&bytes);
            let e = index.files.entry(local).or_default();
            if e.hash.as_deref() != Some(h.as_str()) {
                e.advance(Some(h), mtime_ms(&meta).unwrap_or(now), bytes.len() as u64);
                changed = true;
            }
        }
    }
    let cutoff = now - TOMBSTONE_DAYS * 24 * 3600 * 1000;
    index.files.retain(|local, e| {
        if e.hash.is_some() && !found.contains(local) {
            e.advance(None, now, 0);
            changed = true;
        }
        let keep = e.hash.is_some() || e.mtime > cutoff;
        changed |= !keep;
        keep
    });
    changed
}

/// What the files look like now, cheaply (names, sizes, times): sync runs when it changes.
pub fn fingerprint(root: &Path) -> String {
    let mut s = String::new();
    for (dir, _) in place_dirs(root) {
        for rel in files_in(&root.join(&dir)) {
            let meta = std::fs::metadata(root.join(&dir).join(&rel)).ok();
            s.push_str(&format!("{dir}/{rel}:{}:{}\n", meta.as_ref().map_or(0, |m| m.len()), meta.as_ref().and_then(mtime_ms).unwrap_or(0)));
        }
    }
    hash(s.as_bytes())
}

/// Split a path under the memory folder into the place's folder and the path in it.
pub fn split_local(local: &str) -> Option<(&str, &str)> {
    if let Some(rest) = local.strip_prefix("global/") {
        return Some(("global", rest));
    }
    let first = local.find('/')?;
    let second = first + 1 + local[first + 1..].find('/')?;
    matches!(&local[..first], "projects" | "folders").then(|| (&local[..second], &local[second + 1..]))
}

// ---------- place keys

/// A git remote as the same string on every computer: `git@github.com:Acme/Shop.git`,
/// `https://github.com/acme/shop` and `ssh://git@github.com:22/acme/shop.git` are all
/// `github.com/acme/shop`.
pub fn norm_remote(url: &str) -> Option<String> {
    let mut s = url.trim();
    let scheme = s.find("://").map(|i| {
        s = &s[i + 3..];
        i
    });
    if let Some(at) = s.find('@').filter(|at| !s[..*at].contains('/')) {
        s = &s[at + 1..];
    }
    let (host, path) = match s.find(['/', ':']) {
        // scp-like `host:path`, or `host:port/path` with a scheme
        Some(i) if s.as_bytes()[i] == b':' => {
            let rest = &s[i + 1..];
            let rest = if scheme.is_some() { rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('/') } else { rest };
            (&s[..i], rest)
        }
        Some(i) => (&s[..i], &s[i + 1..]),
        None => return None,
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{host}/{path}").to_lowercase())
}

/// The `origin` remote (else the first one) of the repository holding `dir`, normalized.
pub fn git_remote(dir: &Path) -> Option<String> {
    let repo = dir.ancestors().find(|a| a.join(".git").exists())?;
    let dot = repo.join(".git");
    let mut git_dir = dot.clone();
    if dot.is_file() {
        // a worktree or submodule: `gitdir: <path>`, its config in the common folder
        let text = std::fs::read_to_string(&dot).ok()?;
        let p = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        git_dir = if p.is_absolute() { p } else { repo.join(p) };
        if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
            let c = PathBuf::from(common.trim());
            git_dir = if c.is_absolute() { c } else { git_dir.join(c) };
        }
    }
    let config = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let (mut section, mut first, mut origin) = (String::new(), None, None);
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.to_string();
        } else if section.starts_with("[remote ") && line.split('=').next().is_some_and(|k| k.trim() == "url") {
            let url = line.split_once('=').map(|(_, v)| v.trim().to_string()).unwrap_or_default();
            if section == "[remote \"origin\"]" {
                origin.get_or_insert(url.clone());
            }
            first.get_or_insert(url);
        }
    }
    norm_remote(&origin.or(first)?)
}

/// A project's key: the remote of its first code folder that has one, else its name.
pub fn project_key(p: &Project) -> String {
    p.folders.iter().find_map(|f| git_remote(Path::new(f))).map(|r| format!("git:{r}")).unwrap_or_else(|| format!("project:{}", p.name.trim().to_lowercase()))
}

/// A place's key, if it has one (see the module docs).
pub fn place_key(place: &Place, projects: &[Project]) -> Option<String> {
    match place {
        Place::Global => Some("global".into()),
        Place::Project(id) => projects.iter().find(|p| &p.id == id).map(project_key),
        Place::Folder(path) => git_remote(Path::new(path)).map(|r| format!("git:{r}")),
    }
}

/// Where places are on this computer, by key.
pub struct Places {
    /// key → (folder under the memory folder, place)
    by_key: BTreeMap<String, (String, Place)>,
    /// folder → key
    by_dir: BTreeMap<String, String>,
}

impl Places {
    /// The places with memories here, and the projects that could have them. When two share a
    /// key (a project and a loose clone of the same repository) the project gets it.
    pub fn new(root: &Path, projects: &[Project]) -> Self {
        let mut by_key: BTreeMap<String, (String, Place)> = BTreeMap::new();
        let mut candidates: Vec<(String, Place)> = place_dirs(root);
        candidates.extend(projects.iter().map(|p| {
            let place = Place::Project(p.id.clone());
            (place.rel_dir(), place)
        }));
        // projects first, then folders (global has its own key)
        candidates.sort_by_key(|(_, p)| matches!(p, Place::Folder(_)));
        for (dir, place) in candidates {
            let Some(key) = place_key(&place, projects) else { continue };
            by_key.entry(key).or_insert((dir, place));
        }
        let by_dir = by_key.iter().map(|(k, (d, _))| (d.clone(), k.clone())).collect();
        Places { by_key, by_dir }
    }

    pub fn key_of_dir(&self, dir: &str) -> Option<&str> {
        self.by_dir.get(dir).map(String::as_str)
    }

    pub fn resolve(&self, key: &str) -> Option<&(String, Place)> {
        self.by_key.get(key)
    }
}

/// This computer's manifest: every indexed file of a place that has a key.
pub fn manifest(index: &Index, places: &Places) -> Vec<Item> {
    index
        .files
        .iter()
        .filter_map(|(local, e)| {
            let (dir, path) = split_local(local)?;
            Some(Item { place: places.key_of_dir(dir)?.to_string(), path: path.to_string(), hash: e.hash.clone(), mtime: e.mtime, size: e.size, seen: e.seen.clone() })
        })
        .collect()
}

/// Is an item another computer sent well-formed?
pub fn valid_item(i: &Item) -> bool {
    i.place.len() <= 300
        && !i.place.chars().any(char::is_control)
        && shared_path(&i.path)
        && i.hash.as_ref().is_none_or(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        && i.seen.len() <= SEEN * 2
        && i.seen.iter().all(|s| s.len() <= 64)
}

// ---------- deciding

/// What to do with one file when two computers meet (decided by the one that called).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Same,
    /// Take the other computer's version.
    Pull,
    /// Give the other computer this one.
    Push,
    /// The other computer deleted the version this one has.
    DeleteLocal,
    /// This computer deleted the version the other one has.
    DeleteRemote,
    /// Both changed it without seeing the other's change: the newer one wins, the other is kept
    /// as a conflict copy.
    Conflict { local_wins: bool },
}

/// Has `item` had the version with hash `h` (now or before)?
fn has_seen(item: &Item, h: &str) -> bool {
    item.hash.as_deref() == Some(h) || item.seen.iter().any(|s| s == short(h))
}

/// Is `a` newer than `b`? Ties go to the larger hash, so both computers decide the same.
fn newer(a: Option<&Item>, b: Option<&Item>) -> bool {
    let k = |i: Option<&Item>| i.map(|i| (i.mtime, i.hash.clone().unwrap_or_default()));
    k(a) > k(b)
}

pub fn decide(local: Option<&Item>, remote: Option<&Item>) -> Action {
    let lc = local.and_then(|i| i.hash.as_deref());
    let rc = remote.and_then(|i| i.hash.as_deref());
    if lc == rc {
        return Action::Same;
    }
    // Did the remote version come after this one (it has seen it), or the other way round?
    let remote_after = matches!((lc, remote), (Some(h), Some(r)) if has_seen(r, h));
    let local_after = matches!((rc, local), (Some(h), Some(l)) if has_seen(l, h));
    let take = if rc.is_some() { Action::Pull } else { Action::DeleteLocal };
    let give = if lc.is_some() { Action::Push } else { Action::DeleteRemote };
    match (remote_after, local_after) {
        (true, false) => take,
        (false, true) => give,
        // went back to an earlier version: the newer change wins
        (true, true) => {
            if newer(remote, local) {
                take
            } else {
                give
            }
        }
        (false, false) => match (lc, rc) {
            // a version the other side never saw beats its deletion (or its not having it)
            (Some(_), None) => Action::Push,
            (None, Some(_)) => Action::Pull,
            _ => Action::Conflict { local_wins: !newer(remote, local) },
        },
    }
}

/// The two texts differ only in the `updated:` line (saved twice, or imported on both computers):
/// not worth a conflict copy.
pub fn same_but_updated(a: &str, b: &str) -> bool {
    let strip = |t: &str| t.lines().filter(|l| !l.trim_start().starts_with("updated:")).map(str::trim_end).collect::<Vec<_>>().join("\n");
    strip(a) == strip(b)
}

// ---------- writing what arrives

/// What another computer sent, made safe: too big is refused; secrets are scrubbed (as on save)
/// and a memory's body capped at `MAX_BODY`.
pub fn clean_incoming(text: &str) -> Result<String, String> {
    if text.len() > MAX_FILE {
        return Err(format!("too big ({} KB)", text.len() / 1024));
    }
    let text = scrub(text);
    let (_, body) = split_front(&text);
    if body.len() <= MAX_BODY {
        return Ok(text);
    }
    let front = &text[..text.len() - body.len()];
    Ok(format!("{front}{}\n", cap(body.trim_end(), MAX_BODY)))
}

/// The hash of a file now (none: it isn't there).
pub fn current_hash(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| hash(&b))
}

/// Make sure a place's folder exists, with its `place.json`.
pub fn ensure_place(root: &Path, dir: &str, place: &Place) -> std::io::Result<()> {
    let d = root.join(dir);
    std::fs::create_dir_all(&d)?;
    if place != &Place::Global && !d.join("place.json").exists() {
        let json = serde_json::to_string(place).map_err(std::io::Error::other)?;
        write_atomic(&d.join("place.json"), json.as_bytes()).map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    Ok(())
}

/// Take `item` (with `text`, or delete when it has no hash) into the file at `dir/path`, if the
/// file is still at `expect`. Returns false when it changed meanwhile.
#[allow(clippy::too_many_arguments)]
pub fn apply(root: &Path, index: &mut Index, dir: &str, place: &Place, path: &str, expect: Option<&str>, item: &Item, text: Option<&str>) -> Result<bool, String> {
    let local = format!("{dir}/{path}");
    let file = root.join(&local);
    if current_hash(&file).as_deref() != expect {
        return Ok(false);
    }
    let (new_hash, size) = match (item.hash.as_ref(), text) {
        (Some(_), Some(text)) => {
            let clean = clean_incoming(text)?;
            ensure_place(root, dir, place).map_err(|e| e.to_string())?;
            write_atomic(&file, clean.as_bytes()).map_err(|e| e.to_string())?;
            (Some(hash(clean.as_bytes())), clean.len() as u64)
        }
        (Some(_), None) => return Err("no text came with the file".into()),
        (None, _) => {
            if file.exists() {
                std::fs::remove_file(&file).map_err(|e| e.to_string())?;
            }
            (None, 0)
        }
    };
    let e = index.files.entry(local).or_default();
    // What this computer had, and the other's history, are both behind the new version.
    let mut seen = e.seen.clone();
    if let Some(h) = &e.hash {
        push_seen(&mut seen, short(h));
    }
    for s in &item.seen {
        push_seen(&mut seen, s);
    }
    if let Some(h) = item.hash.as_ref().filter(|h| Some(*h) != new_hash.as_ref()) {
        // scrubbed or capped on the way in: the sender's version is behind this one
        push_seen(&mut seen, short(h));
    }
    if let Some(h) = &new_hash {
        seen.retain(|s| s != short(h));
    }
    *e = Entry { hash: new_hash, mtime: item.mtime, size, seen };
    Ok(true)
}

/// Keep the losing side of a conflict next to the winner: the memory renamed
/// `<name> (conflict from <computer>)`, so the app lists it as its own memory and can forget it.
/// Returns the new file's path in the place.
pub fn write_conflict_copy(root: &Path, dir: &str, place: &Place, path: &str, text: &str, computer: &str) -> Result<String, String> {
    let text = clean_incoming(text)?;
    let (folder, file) = match path.rsplit_once('/') {
        Some((a, b)) => (format!("{a}/"), b),
        None => (String::new(), path),
    };
    let stem = file.trim_end_matches(".md");
    let computer: String = computer.chars().take(24).collect();
    let (front, _) = split_front(&text);
    let name = front.as_ref().and_then(|f| super::super::front_str(f, "name"));
    // short enough that the slug (at most 60) never comes back to the original name
    let base = |s: &str| s.chars().take(30).collect::<String>();
    let new_name = format!("{} (conflict from {computer})", base(name.as_deref().unwrap_or(stem)));
    let renamed = if name.is_some() { rename_front(&text, &new_name) } else { text };
    let want = hash(renamed.as_bytes());
    let mut new_stem = slug(&new_name);
    let mut n = 2;
    ensure_place(root, dir, place).map_err(|e| e.to_string())?;
    let target = |s: &str| root.join(dir).join(format!("{folder}{s}.md"));
    // the same copy again is the same file; another one gets its own
    while target(&new_stem).exists() && current_hash(&target(&new_stem)).as_deref() != Some(want.as_str()) {
        new_stem = format!("{}-{n}", slug(&new_name));
        n += 1;
    }
    write_atomic(&target(&new_stem), renamed.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("{folder}{new_stem}.md"))
}

/// The text with its frontmatter `name:` set to `name`.
fn rename_front(text: &str, name: &str) -> String {
    let quoted = serde_json::to_string(name).unwrap_or_default();
    let mut out = String::with_capacity(text.len() + 32);
    let mut in_front = false;
    let mut done = false;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let bare = line.trim_end();
        if i == 0 && bare.trim_start_matches('\u{feff}') == "---" {
            in_front = true;
        } else if in_front && bare == "---" {
            in_front = false;
        } else if in_front && !done && bare.starts_with("name:") {
            out.push_str(&format!("name: {quoted}\n"));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(hash: Option<&str>, mtime: i64, seen: &[&str]) -> Item {
        Item { place: "global".into(), path: "a.md".into(), hash: hash.map(|h| h.repeat(64 / h.len())), mtime, size: 1, seen: seen.iter().map(|s| short(&s.repeat(64 / s.len())).to_string()).collect() }
    }

    #[test]
    fn decisions() {
        use Action::*;
        let a = item(Some("a"), 10, &[]);
        // same content, or both gone
        assert_eq!(decide(Some(&a), Some(&item(Some("a"), 99, &["b"]))), Same);
        assert_eq!(decide(None, Some(&item(None, 5, &["a"]))), Same);
        // only one side has it
        assert_eq!(decide(None, Some(&a)), Pull);
        assert_eq!(decide(Some(&a), None), Push);
        // edited after the other's version: fast-forward either way, whatever the clocks say
        let b_after_a = item(Some("b"), 1, &["a"]);
        assert_eq!(decide(Some(&a), Some(&b_after_a)), Pull);
        assert_eq!(decide(Some(&b_after_a), Some(&a)), Push);
        // several edits since the last sync: the history has the old version
        assert_eq!(decide(Some(&a), Some(&item(Some("d"), 20, &["a", "b", "c"]))), Pull);
        // deleted after seeing this version: the deletion travels
        let gone = item(None, 30, &["a"]);
        assert_eq!(decide(Some(&a), Some(&gone)), DeleteLocal);
        assert_eq!(decide(Some(&gone), Some(&a)), DeleteRemote);
        // deleted, but the other side edited it meanwhile: the edit is kept
        let edited = item(Some("b"), 20, &["a"]);
        assert_eq!(decide(Some(&edited), Some(&gone)), Push);
        assert_eq!(decide(Some(&gone), Some(&edited)), Pull);
        // both edited the same version: a conflict, the newer wins, both sides agree
        let mine = item(Some("b"), 20, &["a"]);
        let theirs = item(Some("c"), 25, &["a"]);
        assert_eq!(decide(Some(&mine), Some(&theirs)), Conflict { local_wins: false });
        assert_eq!(decide(Some(&theirs), Some(&mine)), Conflict { local_wins: true });
        // same time: the hash breaks the tie the same way on both sides
        let (x, y) = (item(Some("b"), 20, &[]), item(Some("c"), 20, &[]));
        assert_eq!(decide(Some(&x), Some(&y)), Conflict { local_wins: false });
        assert_eq!(decide(Some(&y), Some(&x)), Conflict { local_wins: true });
        // went back to an earlier version: each has seen the other's, the newer change wins
        let back = item(Some("a"), 40, &["a", "b"]);
        let b = item(Some("b"), 30, &["a"]);
        assert_eq!(decide(Some(&b), Some(&back)), Pull);
        assert_eq!(decide(Some(&back), Some(&b)), Push);
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-msync-plan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("global/handoffs/taken")).unwrap();
        d
    }

    #[test]
    fn scan_tracks_versions_and_tombstones() {
        let root = tmp();
        let mut idx = Index::default();
        std::fs::write(root.join("global/a.md"), "one").unwrap();
        std::fs::write(root.join("global/handoffs/h.md"), "handoff").unwrap();
        std::fs::write(root.join("global/handoffs/taken/t.md"), "taken").unwrap();
        std::fs::write(root.join("global/.secret.md"), "no").unwrap();
        std::fs::write(root.join("global/notes.txt"), "no").unwrap();
        assert!(scan(&root, &mut idx, 1000));
        assert_eq!(idx.files.keys().collect::<Vec<_>>(), ["global/a.md", "global/handoffs/h.md"]);
        assert!(!scan(&root, &mut idx, 2000), "nothing changed");
        let first = idx.files["global/a.md"].hash.clone().unwrap();

        std::fs::write(root.join("global/a.md"), "two").unwrap();
        assert!(scan(&root, &mut idx, 3000));
        assert_eq!(idx.files["global/a.md"].seen, [short(&first)]);

        std::fs::remove_file(root.join("global/a.md")).unwrap();
        scan(&root, &mut idx, 4000);
        let e = &idx.files["global/a.md"];
        assert_eq!((e.hash.as_ref(), e.mtime, e.seen.len()), (None, 4000, 2), "a tombstone with both versions seen");
        // tombstones are forgotten after a while
        scan(&root, &mut idx, 4000 + TOMBSTONE_DAYS * 24 * 3600 * 1000 + 1);
        assert!(!idx.files.contains_key("global/a.md"));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("global/handoffs/h.md"), root.join("global/link.md")).unwrap();
            scan(&root, &mut idx, 5000);
            assert!(!idx.files.contains_key("global/link.md"), "symlinks aren't shared");
        }
    }

    #[test]
    fn paths_and_local_split() {
        assert!(shared_path("a.md") && shared_path("handoffs/x.md"));
        for bad in ["../a.md", "handoffs/taken/x.md", ".env.md", "a.txt", "x/a.md", "place.json", "a\\b.md", ".md", "handoffs/../a.md"] {
            assert!(!shared_path(bad), "{bad}");
        }
        assert_eq!(split_local("global/a.md"), Some(("global", "a.md")));
        assert_eq!(split_local("projects/p1/handoffs/h.md"), Some(("projects/p1", "handoffs/h.md")));
        assert_eq!(split_local("other/x/a.md"), None);
    }

    #[test]
    fn remotes_normalize() {
        let want = Some("github.com/acme/shop".to_string());
        for url in ["git@github.com:Acme/Shop.git", "https://github.com/acme/shop", "https://user:tok@github.com/acme/shop.git/", "ssh://git@github.com:22/acme/shop.git", "github.com:acme/shop"] {
            assert_eq!(norm_remote(url), want, "{url}");
        }
        assert_eq!(norm_remote("nothing"), None);

        let repo = tmp().join("shop");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/config"), "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://github.com/other/shop\n[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n").unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        assert_eq!(git_remote(&repo.join("src")), want, "origin, from a subfolder");
        let p = Project { id: "p".into(), name: "Shop".into(), color: None, description: None, order: 0, folders: vec![repo.to_string_lossy().into()], mcp_writes: false };
        assert_eq!(project_key(&p), "git:github.com/acme/shop");
        assert_eq!(project_key(&Project { folders: vec![], ..p }), "project:shop");
    }

    #[test]
    fn incoming_is_cleaned_and_conflicts_keep_both() {
        assert!(clean_incoming(&"x".repeat(MAX_FILE + 1)).is_err());
        let s = clean_incoming("---\nname: db\n---\n\npassword = hunter22\n").unwrap();
        assert!(!s.contains("hunter22"), "{s}");
        let long = clean_incoming(&format!("---\nname: big\n---\n\n{}", "y".repeat(MAX_BODY + 100))).unwrap();
        assert!(long.starts_with("---\nname: big\n---") && long.len() < MAX_BODY + 100, "body capped, frontmatter kept");

        let root = tmp();
        let text = "---\nname: Deploy steps\ndescription: d\n---\n\nrun make deploy\n";
        let p = write_conflict_copy(&root, "global", &Place::Global, "deploy-steps.md", text, "Arif's MacBook").unwrap();
        assert_eq!(p, "deploy-steps-conflict-from-arif-s-macbook.md");
        let copy = std::fs::read_to_string(root.join("global").join(&p)).unwrap();
        assert!(copy.contains("name: \"Deploy steps (conflict from Arif's MacBook)\"") && copy.contains("run make deploy"), "{copy}");
        assert_eq!(slug("Deploy steps (conflict from Arif's MacBook)") + ".md", p, "the app finds it by its name");
        // the same copy again is the same file; a different one gets its own
        assert_eq!(write_conflict_copy(&root, "global", &Place::Global, "deploy-steps.md", text, "Arif's MacBook").unwrap(), p);
        let other = write_conflict_copy(&root, "global", &Place::Global, "deploy-steps.md", &text.replace("make", "just"), "Arif's MacBook").unwrap();
        assert_eq!(other, "deploy-steps-conflict-from-arif-s-macbook-2.md");
        // a very long name never lands on the original file
        let long_name = "a".repeat(80);
        let p = write_conflict_copy(&root, "global", &Place::Global, &format!("{}.md", slug(&long_name)), &format!("---\nname: {long_name}\n---\nx"), "Desk").unwrap();
        assert_ne!(p, format!("{}.md", slug(&long_name)));
        assert!(same_but_updated("a\n  updated: 1\nb", "a\n  updated: 2\nb") && !same_but_updated("a", "b"));
    }

    #[test]
    fn apply_checks_the_expected_version_and_merges_history() {
        let root = tmp();
        let mut idx = Index::default();
        let text = "hello";
        let h = hash(text.as_bytes());
        let it = Item { place: "global".into(), path: "a.md".into(), hash: Some(h.clone()), mtime: 77, size: 5, seen: vec!["0000".into()] };
        assert!(apply(&root, &mut idx, "global", &Place::Global, "a.md", None, &it, Some(text)).unwrap());
        assert_eq!(std::fs::read_to_string(root.join("global/a.md")).unwrap(), "hello");
        assert_eq!(idx.files["global/a.md"], Entry { hash: Some(h.clone()), mtime: 77, size: 5, seen: vec!["0000".into()] });
        assert!(!scan(&root, &mut idx, 100), "what arrived isn't a local change");
        // the file changed since the other computer looked: left alone
        assert!(!apply(&root, &mut idx, "global", &Place::Global, "a.md", None, &it, Some("other")).unwrap());
        // a deletion of the version it has
        let gone = Item { hash: None, mtime: 90, ..it.clone() };
        assert!(apply(&root, &mut idx, "global", &Place::Global, "a.md", Some(&h), &gone, None).unwrap());
        assert!(!root.join("global/a.md").exists());
        assert!(idx.files["global/a.md"].seen.contains(&short(&h).to_string()));
        // a new project place gets its marker
        let place = Place::Project("p9".into());
        assert!(apply(&root, &mut idx, "projects/p9", &place, "b.md", None, &Item { path: "b.md".into(), ..it.clone() }, Some(text)).unwrap());
        assert!(place_dirs(&root).iter().any(|(d, p)| d == "projects/p9" && p == &place));
    }
}
