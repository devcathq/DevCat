//! "Sync Claude memory to all agents": Claude Code keeps its own memory in
//! `~/.claude/projects/<folder>/memory/*.md` (one folder per working directory). This copies those
//! memories into DevCat's shared memory, so Codex, Gemini, Cursor and the DevCat Agent know what
//! Claude learned. One way only: Claude reads the shared memory through `devcat mcp`, and nothing
//! is written into Claude's folder.
//!
//! - A Claude folder's memories go to that folder's place (its DevCat project, else its
//!   repository); `user` memories go to every project. The user can move any of them.
//! - What was copied is recorded (`sync.json`: the source's hash, where it went, the hash of what
//!   was written), so syncing again copies only what changed, never brings back a memory deleted
//!   in DevCat, and shows a conflict when both sides changed. A memory deleted in Claude is only
//!   removed from DevCat when the user ticks it.
//! - With "keep in sync" on, new and changed memories are copied when DevCat starts and whenever
//!   an agent starts `devcat mcp`, so they reach the next session even with the window closed.
use super::{Place, hash, place_for_folder};
use crate::claude::sessions::{dir_cwd, encode_cwd, projects_root};
use crate::error::{AppError, AppResult};
use crate::models::Project;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// What the memories record as their origin.
pub const ORIGIN: &str = "claude-code";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SyncState {
    /// Synced from the dialog at least once.
    #[serde(default)]
    pub synced: bool,
    /// Copy new and changed memories without asking.
    #[serde(default)]
    pub auto: bool,
    /// Keep a memory block in the agents' user rules files (`rules.rs`).
    #[serde(default)]
    pub rules: bool,
    /// The Claude memory files as last seen (paths, sizes, times), to skip unchanged scans.
    #[serde(default)]
    fingerprint: String,
    /// Source file → what was copied.
    #[serde(default)]
    claude: BTreeMap<String, Copied>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Copied {
    /// The source file's hash when copied.
    hash: String,
    place: Place,
    name: String,
    /// Hash of the memory file DevCat wrote (changed since: someone edited it in DevCat).
    written: String,
}

/// One sync at a time in this process.
static LOCK: Mutex<()> = Mutex::new(());

fn state_path() -> AppResult<PathBuf> {
    Ok(super::root()?.join("sync.json"))
}

pub(super) fn load_state() -> SyncState {
    state_path().ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(s: &SyncState) -> AppResult<()> {
    super::write_atomic(&state_path()?, serde_json::to_string_pretty(s)?.as_bytes())
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    New,
    Changed,
    Same,
    Conflict,
    Removed,
}

/// One Claude memory and what a sync does with it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// The source file (pass back to apply it).
    pub id: String,
    /// Claude's working directory the memory belongs to.
    pub folder: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    /// Goes to every project (else to its folder's project).
    pub global: bool,
    /// Its folder's project or repository, as the app shows it.
    pub target: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    pub items: Vec<Item>,
    pub notes: Vec<String>,
    pub synced: bool,
    pub auto: bool,
    pub rules: bool,
    /// The rules files the memory block goes in (agents found on this computer).
    pub rules_files: Vec<String>,
}

/// A Claude memory file found on disk.
struct Found {
    source: PathBuf,
    folder: String,
    /// Its folder's place.
    place: Place,
    memory: super::Memory,
    hash: String,
}

/// Where Claude keeps its memory folders, and DevCat's projects (tests pass their own).
struct Env {
    claude_root: PathBuf,
    projects: Vec<Project>,
}

impl Env {
    fn current() -> Env {
        Env { claude_root: projects_root(), projects: super::projects() }
    }
}

/// Every Claude memory file, with the folder it belongs to.
fn discover(env: &Env, notes: &mut Vec<String>) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&env.claude_root) else { return out };
    let (mut unknown, mut gone) = (0, 0);
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|d| d.join("memory").is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let files = memory_files(&dir.join("memory"));
        if files.is_empty() {
            continue;
        }
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // the folder: from Claude's transcripts there, else a project folder with that encoding
        let folder = dir_cwd(&dir).or_else(|| env.projects.iter().flat_map(|p| p.folders.iter()).find(|f| encode_cwd(f) == name).cloned());
        let Some(folder) = folder else {
            unknown += 1;
            continue;
        };
        if !Path::new(&folder).is_dir() {
            gone += 1;
            continue;
        }
        let place = place_for_folder(Path::new(&folder), &env.projects);
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let memory = super::parse(&text, &place, &file);
            out.push(Found { hash: hash(text.as_bytes()), source: file, folder: folder.clone(), place: place.clone(), memory });
        }
    }
    if unknown > 0 {
        notes.push(format!("{unknown} Claude memory folder{} skipped: DevCat can't tell which folder {} belong{} to (no sessions left there).", plural(unknown), if unknown == 1 { "it" } else { "they" }, if unknown == 1 { "s" } else { "" }));
    }
    if gone > 0 {
        notes.push(format!("{gone} Claude memory folder{} skipped: {} folder no longer exists (temporary checkouts).", plural(gone), if gone == 1 { "its" } else { "their" }));
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The memories in a Claude memory folder (not its `MEMORY.md` index).
fn memory_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md") && !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("MEMORY.md")))
        .collect();
    files.sort();
    files
}

fn file_hash(p: &Path) -> Option<String> {
    std::fs::read(p).ok().map(|b| hash(&b))
}

/// What a sync would do with every Claude memory.
fn plan(env: &Env, state: &SyncState) -> (Vec<Item>, Vec<Found>, Vec<String>) {
    let mut notes = Vec::new();
    let found = discover(env, &mut notes);
    let mut items = Vec::new();
    let mut taken: HashSet<(Place, String)> = HashSet::new();
    for f in &found {
        let id = f.source.to_string_lossy().to_string();
        let copied = state.claude.get(&id);
        let global = copied.map(|c| c.place == Place::Global).unwrap_or(f.memory.kind == "user");
        let place = if global { Place::Global } else { f.place.clone() };
        let target = super::file_of(&place, &f.memory.name).ok();
        let current = target.as_deref().and_then(file_hash);
        let mut note = None;
        let mut status = match copied {
            None => match target.as_deref().filter(|t| t.exists()) {
                Some(t) => {
                    let mine = std::fs::read_to_string(t).ok().map(|text| super::parse(&text, &place, t)).is_some_and(|m| m.source.as_deref() == Some(id.as_str()));
                    if mine {
                        Status::Changed
                    } else {
                        note = Some("DevCat already has a memory with this name there; syncing replaces it.".into());
                        Status::Conflict
                    }
                }
                None => Status::New,
            },
            Some(c) => {
                let edited = current.as_ref().is_some_and(|h| *h != c.written);
                if c.hash == f.hash && c.place == place {
                    Status::Same
                } else if edited {
                    note = Some("Changed in Claude and in DevCat since the last sync; syncing keeps Claude's.".into());
                    Status::Conflict
                } else {
                    Status::Changed
                }
            }
        };
        if !taken.insert((place.clone(), super::slug(&f.memory.name))) && status != Status::Same {
            note = Some("Another Claude memory with this name goes to the same place; only one can be kept.".into());
            status = Status::Conflict;
        }
        items.push(Item {
            id,
            folder: super::tilde(&f.folder),
            name: f.memory.name.clone(),
            description: f.memory.description.clone(),
            kind: f.memory.kind.clone(),
            global,
            target: f.place.label(&env.projects),
            status,
            note,
        });
    }
    // copied before, gone from Claude since
    let present: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    for (id, c) in &state.claude {
        if present.contains(id) || Path::new(id).exists() {
            continue;
        }
        items.push(Item {
            id: id.clone(),
            folder: super::tilde(&Path::new(id).parent().and_then(Path::parent).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
            name: c.name.clone(),
            description: String::new(),
            kind: String::new(),
            global: c.place == Place::Global,
            target: c.place.label(&env.projects),
            status: Status::Removed,
            note: Some("Deleted in Claude; syncing removes it from DevCat too.".into()),
        });
    }
    (items, found, notes)
}

/// Copy the chosen memories (`id` → to every project or not), remove the chosen deleted ones.
fn apply(env: &Env, state: &mut SyncState, chosen: &[(String, bool)]) -> AppResult<Report> {
    let (items, found, _) = plan(env, state);
    let mut report = Report::default();
    for (id, global) in chosen {
        let Some(item) = items.iter().find(|i| &i.id == id) else { continue };
        if item.status == Status::Removed {
            if let Some(c) = state.claude.remove(id) {
                if let Ok(file) = super::file_of(&c.place, &c.name)
                    && file_hash(&file).is_some_and(|h| h == c.written)
                {
                    std::fs::remove_file(file)?;
                }
                report.removed += 1;
            }
            continue;
        }
        let Some(f) = found.iter().find(|f| f.source.to_string_lossy() == id.as_str()) else { continue };
        let place = if *global { Place::Global } else { f.place.clone() };
        let before = state.claude.get(id).cloned();
        if item.status == Status::Same && before.as_ref().is_some_and(|c| c.place == place) {
            continue;
        }
        let m = &f.memory;
        let saved = super::save(&place, super::NewMemory { name: m.name.clone(), description: m.description.clone(), kind: m.kind.clone(), body: m.body.clone(), origin: ORIGIN.into(), source: Some(id.clone()) })?;
        let file = super::file_of(&place, &saved.name)?;
        // moved (to every project, or back): the old copy goes, unless it was edited in DevCat
        if let Some(c) = &before
            && (c.place != place || super::slug(&c.name) != super::slug(&saved.name))
            && let Ok(old) = super::file_of(&c.place, &c.name)
            && old != file
            && file_hash(&old).is_some_and(|h| h == c.written)
        {
            std::fs::remove_file(old)?;
        }
        state.claude.insert(id.clone(), Copied { hash: f.hash.clone(), place, name: saved.name, written: file_hash(&file).unwrap_or_default() });
        if before.is_some() { report.updated += 1 } else { report.imported += 1 }
    }
    Ok(report)
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub imported: usize,
    pub updated: usize,
    pub removed: usize,
}

/// The Claude memory files' paths, sizes and times, hashed: changes when any of them does.
fn fingerprint(env: &Env) -> String {
    let mut parts = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&env.claude_root) {
        for dir in rd.flatten().map(|e| e.path().join("memory")).filter(|d| d.is_dir()) {
            for f in memory_files(&dir) {
                let meta = std::fs::metadata(&f).ok();
                let time = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
                parts.push(format!("{}\t{}\t{time}", f.display(), meta.map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    parts.sort();
    hash(parts.join("\n").as_bytes())
}

/// What the dialog shows.
pub fn preview() -> AppResult<ImportPlan> {
    let _g = LOCK.lock();
    let state = load_state();
    let (items, _, notes) = plan(&Env::current(), &state);
    Ok(ImportPlan { items, notes, synced: state.synced, auto: state.auto, rules: state.rules, rules_files: super::rules::files().iter().map(|p| super::tilde(&p.to_string_lossy())).collect() })
}

/// The dialog's sync: copy the chosen memories and remember the choices.
pub fn sync(chosen: &[(String, bool)], auto: bool, rules: bool) -> AppResult<Report> {
    let _g = LOCK.lock();
    let env = Env::current();
    let mut state = load_state();
    let report = apply(&env, &mut state, chosen)?;
    (state.synced, state.auto, state.rules) = (true, auto, rules);
    state.fingerprint = fingerprint(&env);
    save_state(&state)?;
    Ok(report)
}

/// Keep in sync: copy what's new or changed in Claude (never conflicts or removals: those wait
/// for the dialog). Cheap when nothing changed.
pub fn auto() {
    let _g = LOCK.lock();
    let mut state = load_state();
    if !state.auto {
        return;
    }
    let env = Env::current();
    let print = fingerprint(&env);
    if print == state.fingerprint {
        return;
    }
    let (items, _, _) = plan(&env, &state);
    let chosen: Vec<(String, bool)> = items.iter().filter(|i| matches!(i.status, Status::New | Status::Changed)).map(|i| (i.id.clone(), i.global)).collect();
    match apply(&env, &mut state, &chosen) {
        Ok(r) => {
            state.fingerprint = print;
            if let Err(e) = save_state(&state) {
                eprintln!("Memory: couldn't save the Claude sync state: {e}");
            }
            if r.imported + r.updated > 0 && state.rules {
                super::rules::refresh();
            }
        }
        Err(e) => eprintln!("Memory: copying Claude's memory failed: {e}"),
    }
}

/// Claude's memory folder for an agent working in `dir` (where Claude's own copies of these
/// memories live): the folder's, else its repository's.
pub fn memory_dir_for(dir: &Path) -> Option<String> {
    let root = projects_root();
    let repo = dir.ancestors().position(|a| a.join(".git").exists()).map(|i| i + 1).unwrap_or(1);
    dir.ancestors().take(repo).map(|a| root.join(encode_cwd(&a.to_string_lossy())).join("memory")).find(|c| c.is_dir()).map(|c| c.to_string_lossy().to_string())
}

// ---------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_claude_preview() -> AppResult<ImportPlan> {
    tokio::task::spawn_blocking(preview).await.map_err(|e| AppError::msg(e.to_string()))?
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub global: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    #[serde(flatten)]
    pub copied: Report,
    /// The agents whose MCP config now has DevCat's server.
    pub agents: Vec<String>,
    /// Rules files with the memory block.
    pub rules_files: Vec<String>,
    pub notes: Vec<String>,
}

/// "Sync Claude memory to all agents": copy the chosen memories, put DevCat's MCP server in every
/// agent's config (Sync MCP), and keep (or remove) the memory block in their rules files.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_all(choices: Vec<Choice>, auto: bool, rules: bool) -> AppResult<SyncReport> {
    tokio::task::spawn_blocking(move || {
        let chosen: Vec<(String, bool)> = choices.into_iter().map(|c| (c.id, c.global)).collect();
        let copied = sync(&chosen, auto, rules)?;
        let mut report = SyncReport { copied, ..SyncReport::default() };
        match crate::mcp_sync::apply(&[]) {
            Ok(plan) => {
                report.agents = plan.agents;
                report.notes.extend(plan.notes);
            }
            Err(e) => report.notes.push(format!("Couldn't add DevCat's MCP server to the agents: {e}")),
        }
        match super::rules::apply(rules) {
            Ok(files) => report.rules_files = files,
            Err(e) => report.notes.push(format!("Couldn't update the agents' rules files: {e}")),
        }
        Ok(report)
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-claude-mem-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A Claude memory folder for `folder` holding `files`, and one transcript line naming it.
    fn claude_folder(root: &Path, folder: &Path, files: &[(&str, &str)]) -> PathBuf {
        let dir = root.join(encode_cwd(&folder.to_string_lossy()));
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(dir.join("s1.jsonl"), format!("{{\"cwd\":{}}}\n", serde_json::to_string(&folder.to_string_lossy()).unwrap())).unwrap();
        std::fs::write(dir.join("memory/MEMORY.md"), "- index").unwrap();
        for (name, text) in files {
            std::fs::write(dir.join("memory").join(name), text).unwrap();
        }
        dir.join("memory")
    }

    fn mem(name: &str, kind: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: about {name}\nmetadata:\n  type: {kind}\n---\n\n{body}\n")
    }

    fn status(items: &[Item], name: &str) -> Status {
        items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name}: {items:?}")).status
    }

    fn all(items: &[Item]) -> Vec<(String, bool)> {
        items.iter().filter(|i| i.status != Status::Same).map(|i| (i.id.clone(), i.global)).collect()
    }

    #[test]
    fn claude_memories_are_copied_once_and_follow_changes() {
        super::super::tests::data();
        let (root, repo) = (tmp("root"), tmp("repo"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let uniq = uuid::Uuid::new_v4().simple().to_string();
        let role = format!("role-{uniq}");
        let mdir = claude_folder(&root, &repo, &[("testing.md", &mem("real-db-tests", "feedback", "Integration tests hit a real database.")), ("role.md", &mem(&role, "user", "Backend engineer"))]);
        let env = Env { claude_root: root.clone(), projects: vec![] };
        let mut state = SyncState::default();

        let (items, _, notes) = plan(&env, &state);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(items.len(), 2, "MEMORY.md is Claude's index, not a memory");
        assert_eq!(status(&items, "real-db-tests"), Status::New);
        let user = items.iter().find(|i| i.name == role).unwrap();
        assert!(user.global, "user memories go to every project");
        assert!(!items.iter().find(|i| i.name == "real-db-tests").unwrap().global, "feedback stays with its project");

        let r = apply(&env, &mut state, &all(&items)).unwrap();
        assert_eq!((r.imported, r.updated), (2, 0));
        let place = place_for_folder(&repo, &[]);
        let copied = super::super::read(&place, "real-db-tests").unwrap();
        assert_eq!((copied.origin.as_str(), copied.body.as_str()), (ORIGIN, "Integration tests hit a real database."));
        assert!(super::super::read(&Place::Global, &role).is_some());

        // nothing changed: nothing to do
        let (items, _, _) = plan(&env, &state);
        assert!(items.iter().all(|i| i.status == Status::Same), "{items:?}");

        // changed in Claude: updated
        std::fs::write(mdir.join("testing.md"), mem("real-db-tests", "feedback", "Integration tests hit a real database, never mocks.")).unwrap();
        let (items, _, _) = plan(&env, &state);
        assert_eq!(status(&items, "real-db-tests"), Status::Changed);
        apply(&env, &mut state, &all(&items)).unwrap();
        assert!(super::super::read(&place, "real-db-tests").unwrap().body.ends_with("never mocks."));

        // edited in DevCat and in Claude: a conflict
        super::super::save(&place, super::super::NewMemory { name: "real-db-tests".into(), description: "d".into(), kind: "feedback".into(), body: "edited by codex".into(), origin: "codex".into(), source: None }).unwrap();
        std::fs::write(mdir.join("testing.md"), mem("real-db-tests", "feedback", "v3")).unwrap();
        let (items, _, _) = plan(&env, &state);
        assert_eq!(status(&items, "real-db-tests"), Status::Conflict);

        // moved to every project: the old copy goes only if it wasn't edited (it was: it stays)
        let r = apply(&env, &mut state, &[(mdir.join("testing.md").to_string_lossy().to_string(), true)]).unwrap();
        assert_eq!(r.updated, 1);
        assert_eq!(super::super::read(&Place::Global, "real-db-tests").unwrap().body, "v3");
        assert_eq!(super::super::read(&place, "real-db-tests").unwrap().body, "edited by codex");
        super::super::forget(&Place::Global, "real-db-tests").unwrap();

        // deleted in Claude: offered for removal, removed only when chosen
        std::fs::remove_file(mdir.join("role.md")).unwrap();
        let (items, _, _) = plan(&env, &state);
        assert_eq!(status(&items, &role), Status::Removed);
        let removed = items.iter().find(|i| i.name == role).unwrap().id.clone();
        let r = apply(&env, &mut state, &[(removed, true)]).unwrap();
        assert_eq!(r.removed, 1);
        assert!(super::super::read(&Place::Global, &role).is_none());
        let _ = (std::fs::remove_dir_all(root), std::fs::remove_dir_all(repo));
    }

    #[test]
    fn folders_that_are_gone_or_unknown_are_skipped() {
        let root = tmp("root2");
        claude_folder(&root, Path::new("/no/such/folder/anymore"), &[("a.md", &mem("a", "project", "b"))]);
        let orphan = root.join("-somewhere");
        std::fs::create_dir_all(orphan.join("memory")).unwrap();
        std::fs::write(orphan.join("memory/b.md"), mem("b", "project", "b")).unwrap();
        let env = Env { claude_root: root.clone(), projects: vec![] };
        let (items, _, notes) = plan(&env, &SyncState::default());
        assert!(items.is_empty());
        assert_eq!(notes.len(), 2, "{notes:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_project_folder_is_found_without_transcripts() {
        super::super::tests::data();
        let (root, repo) = (tmp("root3"), tmp("repo3"));
        let dir = root.join(encode_cwd(&repo.to_string_lossy()));
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(dir.join("memory/x.md"), mem("x", "project", "b")).unwrap();
        let projects = vec![Project { id: "p-mem".into(), name: "Shop".into(), folders: vec![repo.to_string_lossy().to_string()], ..Default::default() }];
        let env = Env { claude_root: root.clone(), projects };
        let (items, found, _) = plan(&env, &SyncState::default());
        assert_eq!(items.len(), 1);
        assert_eq!((found[0].place.clone(), items[0].target.as_str()), (Place::Project("p-mem".into()), "Shop"));
        let _ = (std::fs::remove_dir_all(root), std::fs::remove_dir_all(repo));
    }
}

