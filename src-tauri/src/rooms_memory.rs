//! A room linked to a project on this computer (Room settings → "Project folder": a folder or a
//! DevCat project, `Prefs::folder` / `Prefs::project`, local to this user, never shared). This
//! user's room agent then works with that project's memory (`memory/`), the one their coding agents
//! in that folder use:
//!
//! - its turn lists that place's memories (`memory::place_brief`: not the ones for every project,
//!   which are about the user), and with `MemoryReach::Search` it gets `memory_search` /
//!   `memory_read` for that place only (`memory::tools::read_only_for`: one implementation);
//! - it never writes the memory: room content is other people's, so `memory_propose` only records
//!   a proposal here, and the user saves it (or not) in the window (`room_memory_proposals`,
//!   `room_memory_proposal_done`), like "Save to my memory" for a room file;
//! - what it reads can end up in the thread, where others read it: the setting says so.
//!
//! An `ssh://` folder has no memory on this computer (agents on SSH hosts don't get memory), so it
//! can't be linked. It gives the agent no files of the folder: only memory. Desktop only, like rooms.
use crate::error::{AppError, AppResult};
use crate::memory::{self, Place};
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use devcat_rooms::{MemoryReach, Prefs};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Proposals kept per room, at most (the agent is told to wait when it's full).
const MAX_PROPOSALS: usize = 30;

/// One writer of the proposal files at a time (the agent's runs and the window).
static LOCK: Lazy<Mutex<()>> = Lazy::new(Default::default);

/// The project a room is linked to, as the agent gets it.
#[derive(Debug, Clone)]
pub(crate) struct Linked {
    pub place: Place,
    /// How the app shows it ("~/src/shop", a project's name).
    pub label: String,
    pub search: bool,
    /// Where its memory proposals go.
    pub proposals: PathBuf,
}

/// A folder that can be linked: on this computer, absolute, a folder. Returns it as one spelling.
pub(crate) fn local_folder(folder: &str) -> Result<PathBuf, String> {
    let f = folder.trim();
    if f.contains("://") {
        return Err("An SSH folder has no memory on this computer (agents on SSH hosts don't get DevCat's memory): link a folder on this computer.".into());
    }
    let p = Path::new(f);
    if !p.is_absolute() {
        return Err(format!("{f} isn't a full path."));
    }
    if !p.is_dir() {
        return Err(format!("{f} isn't a folder on this computer."));
    }
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    Ok(PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\")))
}

/// The memory place a room's prefs link to (none: nothing linked, or it's gone).
pub(crate) fn place_of(p: &Prefs) -> Result<Option<Place>, String> {
    let projects = memory::projects();
    if let Some(id) = p.project.as_deref() {
        return match projects.iter().any(|x| x.id == id) {
            true => Ok(Some(Place::Project(id.to_string()))),
            false => Err("The DevCat project linked to this room is gone.".into()),
        };
    }
    let Some(f) = p.folder.as_deref() else { return Ok(None) };
    local_folder(f).map(|d| Some(memory::place_for_folder(&d, &projects)))
}

/// What the agent gets in room `room` (none: nothing linked, or the link doesn't resolve).
pub(crate) fn linked(room: &str, p: &Prefs) -> Option<Linked> {
    let place = place_of(p).ok()??;
    let proposals = crate::rooms::proposals_file(room).ok()?;
    Some(Linked { label: place.label(&memory::projects()), place, search: p.memory_reach == MemoryReach::Search, proposals })
}

/// Check what the window sets before it's kept (`room_prefs`): a new folder must be one on this
/// computer (kept as one spelling), a new project one DevCat has; not both. A link that was
/// already there stays as it is (changing another setting doesn't fail on a folder since moved:
/// the room header says it doesn't work any more).
pub(crate) fn checked(mut p: Prefs, now: &Prefs) -> AppResult<Prefs> {
    p.folder = p.folder.filter(|f| !f.trim().is_empty());
    p.project = p.project.filter(|f| !f.trim().is_empty());
    if p.folder.is_some() && p.project.is_some() {
        return Err(AppError::msg("Link the room to a folder or to a DevCat project, not both."));
    }
    if let Some(f) = p.folder.as_deref()
        && p.folder != now.folder
    {
        p.folder = Some(local_folder(f).map_err(AppError::msg)?.to_string_lossy().to_string());
    }
    if let Some(id) = p.project.as_deref()
        && p.project != now.project
        && !memory::projects().iter().any(|x| x.id == id)
    {
        return Err(AppError::msg("There's no DevCat project with that id."));
    }
    Ok(p)
}

// ------------------------------------------------------------------ proposals

/// A memory the room agent proposes for its user's memory: saved only if the user saves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub body: String,
    /// Where it would go (the place linked when it was proposed).
    pub place: Place,
    pub at: i64,
    /// The thread message the agent was answering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn load(file: &Path) -> Vec<Proposal> {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(file: &Path, all: &[Proposal]) -> AppResult<()> {
    if all.is_empty() {
        let _ = std::fs::remove_file(file);
        return Ok(());
    }
    devcat_rooms::store::write_atomic(file, &serde_json::to_vec_pretty(all)?)?;
    Ok(())
}

fn add(file: &Path, p: Proposal) -> Result<(), String> {
    let _g = LOCK.lock();
    let mut all = load(file);
    // proposing the same name again for the same place replaces the proposal
    all.retain(|x| !(x.place == p.place && memory::slug(&x.name) == memory::slug(&p.name)));
    if all.len() >= MAX_PROPOSALS {
        return Err(format!("{MAX_PROPOSALS} memory proposals are already waiting for your user: say what you'd remember in your answer instead."));
    }
    all.push(p);
    store(file, &all).map_err(|e| e.to_string())
}

/// The proposals waiting in a room, oldest first.
pub(crate) fn proposals(room: &str) -> Vec<Proposal> {
    crate::rooms::proposals_file(room).map(|f| load(&f)).unwrap_or_default()
}

/// The proposals waiting for the user in a room.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_memory_proposals(room: String) -> AppResult<Vec<Proposal>> {
    Ok(proposals(&room))
}

/// A proposal the user saved (with `api.memorySave`, after looking at it) or dismissed: it's gone.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_memory_proposal_done(room: String, id: String) -> AppResult<()> {
    let file = crate::rooms::proposals_file(&room).map_err(|e| AppError::msg(e.to_string()))?;
    let _g = LOCK.lock();
    let mut all = load(&file);
    all.retain(|p| p.id != id);
    store(&file, &all)?;
    crate::events::emit("rooms-changed", json!({ "room": room, "what": "proposals" }));
    Ok(())
}

/// `memory_propose`: only for the room agent, only with a project linked.
pub(crate) struct Propose {
    pub linked: Linked,
    /// The message it's answering.
    pub message: Option<String>,
}

pub(crate) const PROPOSE: &str = "memory_propose";

#[async_trait]
impl Tool for Propose {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROPOSE.into(),
            description: "Propose an entry for your user's own memory of the project linked to this room (the memory their coding agents use), e.g. a decision the room made that matters for the project. It isn't saved: your user reads it in DevCat and saves it or not. Giving the name of a memory that exists proposes replacing it. Not for the room's team memory (that's room_remember).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short kebab-case name, e.g. release-checklist" },
                    "description": { "type": "string", "description": "One line: what it says" },
                    "type": { "type": "string", "enum": memory::KINDS },
                    "body": { "type": "string", "description": "The memory (Markdown). For feedback and project: the fact, then **Why:** and **How to apply:**" }
                },
                "required": ["name", "description", "body"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        // nothing is written into the memory: a proposal waits for the user
        Kind::Read
    }
    fn summary(&self, i: &Value) -> String {
        i.get("name").and_then(Value::as_str).unwrap_or("memory").to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let get = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let (name, description, body) = (get("name"), get("description"), get("body"));
        if name.is_empty() || body.is_empty() {
            return ToolOutput::err("A memory proposal needs a `name` and a `body`.");
        }
        let kind = Some(get("type").to_lowercase()).filter(|k| memory::KINDS.contains(&k.as_str())).unwrap_or_else(|| "project".into());
        let p = Proposal {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: memory::cap(&name.split_whitespace().collect::<Vec<_>>().join(" "), 80),
            description: memory::cap(&memory::scrub(&description.split_whitespace().collect::<Vec<_>>().join(" ")), 400),
            kind,
            body: memory::cap(&memory::scrub(&body), memory::MAX_BODY),
            place: self.linked.place.clone(),
            at: devcat_rooms::now_ms(),
            message: self.message.clone(),
        };
        match add(&self.linked.proposals, p) {
            Ok(()) => ToolOutput::ok(format!("Proposed \"{name}\" for your user's memory of {}. It isn't saved until they save it in DevCat.", self.linked.label)),
            Err(e) => ToolOutput::err(e),
        }
    }
}

/// The memory tools the room agent gets for a linked project: reading (if the user allows it) and
/// proposing. Never `memory_save` / `memory_forget`.
pub(crate) fn tools(l: &Linked, message: Option<String>) -> Vec<Arc<dyn Tool>> {
    let mut out: Vec<Arc<dyn Tool>> = if l.search { memory::tools::read_only_for(l.place.clone(), "room-agent") } else { vec![] };
    out.push(Arc::new(Propose { linked: l.clone(), message }));
    out
}

/// What the agent's turn says about the linked project's memory.
pub(crate) fn turn_section(l: &Linked) -> String {
    let list = memory::place_brief(&l.place);
    let how = if l.search { "read a whole one with memory_read, find more with memory_search" } else { "you only have this list" };
    format!(
        "Your user's own memory for {label}, the project they linked to this room (from their computer; the other members don't have it, and what you write in the thread they all read: use it to answer well, quote only what the question needs, never secrets; {how}):\n{list}\n",
        label = l.label,
        list = if list.trim().is_empty() { "(no memories yet)\n".to_string() } else { list },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_folders_can_be_linked() {
        assert!(local_folder("ssh://devcat-abc/home/me/repo").unwrap_err().contains("SSH"));
        assert!(local_folder("relative/path").is_err());
        assert!(local_folder("/no/such/folder/devcat-test").is_err());
        let dir = std::env::temp_dir();
        assert!(local_folder(&dir.to_string_lossy()).is_ok());
        let none = Prefs::default();
        let both = Prefs { folder: Some(dir.to_string_lossy().into()), project: Some("p".into()), ..Prefs::default() };
        assert!(checked(both, &none).is_err());
        let blank = checked(Prefs { folder: Some("  ".into()), ..Prefs::default() }, &none).unwrap();
        assert_eq!((blank.folder, blank.project), (None, None), "nothing linked");
        let ssh = Prefs { folder: Some("ssh://devcat-abc/srv/app".into()), ..Prefs::default() };
        assert!(checked(ssh.clone(), &none).is_err(), "an SSH folder can't be linked");
        assert!(checked(Prefs { mentions: devcat_rooms::Mentions::Me, ..ssh.clone() }, &ssh).is_ok(), "a link already there doesn't block other settings");
        assert_eq!(place_of(&Prefs::default()).unwrap(), None);
    }

    #[test]
    fn proposals_wait_replace_by_name_and_are_capped() {
        let dir = std::env::temp_dir().join(format!("devcat-room-proposals-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("memory-proposals.json");
        let place = Place::Folder("/test/shop".into());
        let p = |name: &str, body: &str| Proposal { id: uuid::Uuid::new_v4().to_string(), name: name.into(), description: "d".into(), kind: "project".into(), body: body.into(), place: place.clone(), at: 1, message: None };
        add(&file, p("Release day", "Fridays")).unwrap();
        add(&file, p("release-day", "Thursdays")).unwrap();
        let all = load(&file);
        assert_eq!(all.len(), 1, "same name, same place: replaced");
        assert_eq!(all[0].body, "Thursdays");
        for i in 0..MAX_PROPOSALS - 1 {
            add(&file, p(&format!("m{i}"), "b")).unwrap();
        }
        assert!(add(&file, p("one-more", "b")).is_err(), "full");
        store(&file, &[]).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
