//! Rooms in the app (crates/rooms, docs/rooms-architecture.md): people on their own networks share
//! documents and a thread, and each person's DevCat Agent can join in. Desktop only, like the SSH
//! workbench: nothing here is reachable from a paired phone (`remote/dispatch.rs`).
//!
//! The rooms node starts on first use (or at launch when this DevCat is in a room), with its own
//! iroh key, and the relay choice of DevCat → Mobile.
//!
//! Events: `rooms-changed` ({ room, what: doc|members|presence|removed }), `room-join-request`
//! (JoinRequest), `room-join-resolved` ({ requestId }), `room-agent` ({ room, running, error? }),
//! `room-mention` ({ room, roomName, message }: someone named this user in a thread).
//!
//! This user's coding agents reach rooms through `rooms_tools.rs` (only rooms that allow it).
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use devcat_rooms::doc::{Activity, ActivityKind, Assignee, ChangeKind, ChangeRecord, ClaimView, FileInfo, FileKind, Message, Pin, Reaction, SuggestionView, Task, TaskPatch, clean_path};
use devcat_rooms::wire::{b64, unb64};
use devcat_rooms::{Author, Change, Config, Gone, Host, Invite, JoinRequest, MemberView, Node, Prefs, RelayConfig, Role, Room, RoomSettings, RoomSummary};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;

static DIR: OnceCell<PathBuf> = OnceCell::new();
static NODE: tokio::sync::OnceCell<Node> = tokio::sync::OnceCell::const_new();
static APPROVALS: Lazy<Mutex<HashMap<String, oneshot::Sender<Option<Role>>>>> = Lazy::new(Default::default);
static PENDING: Lazy<Mutex<Vec<JoinRequest>>> = Lazy::new(Default::default);

pub fn init(data_dir: PathBuf) {
    let dir = data_dir.join("rooms");
    let in_a_room = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.path().join("room.json").exists())).unwrap_or(false);
    crate::rooms_tools::set_dir(dir.clone());
    let _ = DIR.set(dir);
    // Only DevCats that are in a room open the rooms socket at launch.
    if in_a_room && !crate::events::headless() {
        crate::rt::spawn(async {
            if let Err(e) = node().await {
                eprintln!("rooms: {e}");
            }
        });
    }
}

pub async fn shutdown() {
    if let Some(n) = NODE.get() {
        n.shutdown().await;
    }
}

fn relay_config() -> RelayConfig {
    match crate::remote::relay_value() {
        Some(r) if r == devcat_remote_proto::RELAY_PUBLIC => RelayConfig::Public,
        Some(url) => RelayConfig::Custom(url),
        None => RelayConfig::Off,
    }
}

pub(crate) async fn node() -> AppResult<&'static Node> {
    NODE.get_or_try_init(|| async {
        let dir = DIR.get().ok_or_else(|| AppError::msg("Rooms aren't initialised"))?.clone();
        let cfg = Config { dir, relay: relay_config(), port: 0, keeper: false, keeper_owners: vec![] };
        // this user's coding agents (`devcat mcp`) reach the rooms through the app (agent_bridge.rs)
        let node = Node::start(cfg, Arc::new(AppHost)).await.map_err(AppError::msg)?;
        Ok(node)
    })
    .await
}

/// Where the agent keeps the logs of its runs in a room (the latest 50).
pub(crate) fn agent_dir(room: &str) -> std::io::Result<PathBuf> {
    let d = DIR.get().ok_or_else(|| std::io::Error::other("rooms not initialised"))?.join(room).join("agent");
    std::fs::create_dir_all(&d)?;
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&d)?.filter_map(|e| e.ok()).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if logs.len() > 50 {
        logs.sort();
        for (_, p) in &logs[..logs.len() - 50] {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(d)
}

pub(crate) async fn room(id: &str) -> AppResult<Arc<Room>> {
    node().await?.room(id).map_err(AppError::msg)
}

struct AppHost;

#[async_trait]
impl Host for AppHost {
    async fn approve_join(&self, req: JoinRequest) -> Option<Role> {
        // Nobody answering (no window open) times out as a refusal (crates/rooms: APPROVAL).
        let (tx, rx) = oneshot::channel();
        APPROVALS.lock().insert(req.request_id.clone(), tx);
        PENDING.lock().push(req.clone());
        crate::events::emit("room-join-request", &req);
        let answer = rx.await.ok().flatten();
        APPROVALS.lock().remove(&req.request_id);
        PENDING.lock().retain(|p| p.request_id != req.request_id);
        crate::events::emit("room-join-resolved", json!({ "requestId": req.request_id }));
        answer
    }

    fn changed(&self, room: &str, what: Change) {
        if what == Change::Removed {
            crate::rooms_agent::stop(room);
        }
        crate::events::emit("rooms-changed", json!({ "room": room, "what": what }));
    }

    fn message(&self, room: &str, msg: &Message) {
        crate::rooms_agent::on_message(room, msg);
        notify_mention(room, msg);
    }
}

/// Mentions older than this (arriving after being offline) are only counted, not announced.
const MENTION_FRESH_MS: i64 = 10 * 60 * 1000;

/// A message from someone else that names this DevCat's user (`@akbar`, `@everyone`): the window
/// tells them.
fn notify_mention(room: &str, msg: &Message) {
    let (room, msg) = (room.to_string(), msg.clone());
    crate::rt::spawn(async move {
        let Ok(r) = self::room(&room).await else { return };
        let me = r.me(false);
        if msg.author.member == me.member || devcat_rooms::now_ms() - msg.at > MENTION_FRESH_MS {
            return;
        }
        if r.membership().mentioned_people(&msg.text).contains(&me.member) {
            crate::events::emit("room-mention", json!({ "room": room, "roomName": r.name(), "message": msg }));
        }
    });
}

// ------------------------------------------------------------------ commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomsInfo {
    /// This DevCat's id in rooms.
    pub id: String,
    pub rooms: Vec<RoomSummary>,
    pub pending: Vec<JoinRequest>,
    /// Rooms this DevCat was removed from, and so deleted, since the window last asked.
    pub gone: Vec<Gone>,
}

/// `start`: false = don't start rooms just to look (empty when not running yet).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rooms_info(start: Option<bool>) -> AppResult<RoomsInfo> {
    if start == Some(false) && NODE.get().is_none() {
        return Ok(RoomsInfo { id: String::new(), rooms: vec![], pending: vec![], gone: vec![] });
    }
    let n = node().await?;
    Ok(RoomsInfo { id: n.id(), rooms: n.rooms(), pending: PENDING.lock().clone(), gone: n.take_gone() })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_create(name: String, my_name: String) -> AppResult<String> {
    node().await?.create(&name, &my_name).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_invite(room: String, role: Role) -> AppResult<String> {
    let n = node().await?;
    // Invites carry the relay this DevCat is on: wait briefly for it after a cold start.
    n.online(std::time::Duration::from_secs(8)).await;
    n.invite(&room, role).map_err(AppError::msg)
}

/// Ask to join with an invite link (waits for the owner's answer, up to 5 minutes).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_join(link: String, my_name: String) -> AppResult<String> {
    node().await?.join(&link, &my_name).await.map_err(AppError::msg)
}

/// The owner's answer to a join request: `role` "editor" | "viewer", or none to refuse.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_join_answer(request_id: String, role: Option<Role>) -> AppResult<()> {
    let tx = APPROVALS.lock().remove(&request_id).ok_or_else(|| AppError::msg("That request is gone (it timed out or was answered)."))?;
    let _ = tx.send(role.filter(|r| matches!(r, Role::Editor | Role::Viewer)));
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_forget(room: String) -> AppResult<()> {
    crate::rooms_agent::stop(&room);
    node().await?.forget(&room).map_err(AppError::msg)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomView {
    pub summary: RoomSummary,
    pub settings: RoomSettings,
    pub me: String,
    pub owner: bool,
    pub members: Vec<MemberView>,
    /// `@handle` of each member's agent, by member id.
    pub handles: HashMap<String, String>,
    /// `@handle` of each member themselves, by member id.
    pub people: HashMap<String, String>,
    pub invites: Vec<Invite>,
    pub prefs: Prefs,
    pub agent_running: bool,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_view(room: String) -> AppResult<RoomView> {
    let n = node().await?;
    let r = n.room(&room).map_err(AppError::msg)?;
    let m = r.membership();
    let handles = m.members.iter().filter(|x| x.role != Role::Keeper).filter_map(|x| Some((x.id.clone(), m.agent_handle(&x.id)?))).collect();
    let people = m.members.iter().filter(|x| x.role != Role::Keeper).filter_map(|x| Some((x.id.clone(), m.person_handle(&x.id)?))).collect();
    Ok(RoomView {
        summary: r.summary(),
        settings: m.settings.clone(),
        me: n.id(),
        owner: r.is_owner(),
        members: r.members(),
        handles,
        people,
        invites: if r.is_owner() { r.invites() } else { vec![] },
        prefs: r.prefs(),
        agent_running: crate::rooms_agent::running(&room),
    })
}

/// What the room's thread, suggestions, claims and history look like now.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomBoard {
    pub files: Vec<FileInfo>,
    pub messages: Vec<Message>,
    pub suggestions: Vec<SuggestionView>,
    pub claims: Vec<ClaimView>,
    pub history: Vec<HistoryItem>,
    pub tasks: Vec<Task>,
    /// Decisions: pinned messages.
    pub pins: Vec<Pin>,
    pub reactions: Vec<Reaction>,
    /// Who shared, made, moved, deleted or took out what (the latest 300).
    pub activity: Vec<Activity>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    #[serde(flatten)]
    pub change: ChangeRecord,
    pub revertible: bool,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_board(room: String) -> AppResult<RoomBoard> {
    let r = self::room(&room).await?;
    let d = r.doc();
    let history = d.history().into_iter().rev().take(200).map(|c| HistoryItem { revertible: d.revertible(&c), change: c }).collect();
    Ok(RoomBoard { files: d.files(), messages: d.messages(), suggestions: d.suggestions(), claims: d.claims(devcat_rooms::now_ms()), history, tasks: d.tasks(), pins: d.pins(), reactions: d.reactions(), activity: { let all = d.activity(); all[all.len().saturating_sub(300)..].to_vec() } })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocUpdates {
    /// Loro update bytes (base64url).
    pub updates: String,
    /// The backend copy's version (base64url).
    pub version: String,
}

/// What the window's copy of the document lacks (`version`: its own, base64url; empty for all,
/// sent as a snapshot).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_doc_updates(room: String, version: String) -> AppResult<DocUpdates> {
    let r = self::room(&room).await?;
    let v = unb64(&version).unwrap_or_default();
    Ok(DocUpdates { updates: b64(&r.doc().catch_up(&v)), version: b64(&r.doc().version()) })
}

/// Changes typed in the window's copy (Loro update bytes, base64url). `peer`: the window copy's
/// Loro peer id, recorded so its typing is attributed to this DevCat's user.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_doc_import(room: String, updates: String, peer: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    if let Some(p) = peer.and_then(|p| p.parse::<u64>().ok()) {
        r.doc().set_peer(p, &r.me(false).member);
    }
    let bytes = unb64(&updates).ok_or_else(|| AppError::msg("Bad update"))?;
    r.import_from_editor(&bytes).await.map_err(AppError::msg)
}

fn editor(r: &Room) -> AppResult<Author> {
    if !r.role().can_edit() {
        return Err(AppError::msg("You can only read this room."));
    }
    Ok(r.me(false))
}

fn member(r: &Room) -> AppResult<Author> {
    if r.removed() {
        return Err(AppError::msg("You're no longer in this room."));
    }
    Ok(r.me(false))
}

/// A message in the thread; `reply_to`: the message it answers.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_post(room: String, text: String, reply_to: Option<String>) -> AppResult<Message> {
    let r = self::room(&room).await?;
    let me = member(&r)?;
    r.doc().post(&me, &text, 0, reply_to, devcat_rooms::now_ms()).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_react(room: String, message: String, emoji: String) -> AppResult<bool> {
    let r = self::room(&room).await?;
    let me = member(&r)?;
    r.doc().react(&message, &emoji, &me, devcat_rooms::now_ms()).map_err(AppError::msg)
}

/// Pin a message as a decision (`pin` false: unpin it).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_pin(room: String, message: String, pin: bool) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    r.doc().pin(&message, pin, &me, devcat_rooms::now_ms()).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_task_add(room: String, title: String, notes: Option<String>, assignee: Option<Assignee>) -> AppResult<Task> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let t = r.doc().add_task(&title, notes, assignee, &me, devcat_rooms::now_ms()).map_err(AppError::msg)?;
    announce_task(&r, &t, &me);
    Ok(t)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_task_update(room: String, id: String, patch: TaskPatch) -> AppResult<Task> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let before = r.doc().task(&id).and_then(|t| t.assignee);
    let t = r.doc().update_task(&id, patch, &me, devcat_rooms::now_ms()).map_err(AppError::msg)?;
    if t.assignee.is_some() && t.assignee != before {
        announce_task(&r, &t, &me);
    }
    Ok(t)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_task_delete(room: String, id: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    editor(&r)?;
    r.doc().delete_task(&id).map_err(AppError::msg)
}

/// A task given to someone else, or to an agent: say so in the thread, naming them, so they're
/// told (and a named agent wakes, as for any mention).
pub(crate) fn announce_task(r: &Room, t: &Task, by: &Author) {
    let Some(a) = &t.assignee else { return };
    let m = r.membership();
    let handle = if a.agent {
        m.agent_handle(&a.member)
    } else if a.member == by.member {
        None
    } else {
        m.person_handle(&a.member)
    };
    let Some(h) = handle else { return };
    let notes = t.notes.as_deref().map(|n| format!("\n{n}")).unwrap_or_default();
    let _ = r.doc().post(by, &format!("{h}, a task for you: \"{}\"{notes}", t.title), if by.agent { 1 } else { 0 }, None, devcat_rooms::now_ms());
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_suggest(room: String, path: String, find: String, replacement: String, note: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    r.doc().suggest(&path, &find, &replacement, &me, note, devcat_rooms::now_ms()).map(|_| ()).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_decide(room: String, id: String, accept: bool, reason: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let change = r.doc().decide(&id, accept, &me, reason, devcat_rooms::now_ms()).map_err(AppError::msg)?;
    if let Some(c) = change.filter(|c| c.kind == ChangeKind::NewFile) {
        let _ = r.doc().log(ActivityKind::Created, &c.path, None, None, None, &c.by, devcat_rooms::now_ms());
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_revert(room: String, change: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    r.doc().revert(&change, &me, devcat_rooms::now_ms()).map(|_| ()).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_new(room: String, path: String, content: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let now = devcat_rooms::now_ms();
    r.doc().create_file(&path, content.as_deref().unwrap_or(""), &me, now).map_err(AppError::msg)?;
    let _ = r.doc().log(ActivityKind::Created, &clean_path(&path).unwrap_or(path), None, None, None, &me, now);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_delete(room: String, path: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    r.doc().delete_file(&path).map_err(AppError::msg)?;
    let _ = r.doc().log(ActivityKind::Deleted, &path, None, None, None, &me, devcat_rooms::now_ms());
    Ok(())
}

/// Delete a folder and everything in it. Returns how many files.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_folder_delete(room: String, path: String) -> AppResult<usize> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let n = r.doc().delete_folder(&path).map_err(AppError::msg)?;
    let _ = r.doc().log(ActivityKind::Deleted, &path, None, None, Some(n), &me, devcat_rooms::now_ms());
    Ok(n)
}

/// Rename or move a file, or a folder with what's in it. Returns the new path of each file
/// that moved, by its old one.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_rename(room: String, from: String, to: String) -> AppResult<Vec<(String, String)>> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    r.doc().rename(&from, &to, &me, devcat_rooms::now_ms()).map_err(AppError::msg)
}

/// A new, empty whiteboard.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_board_new(room: String, path: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let now = devcat_rooms::now_ms();
    r.doc().create_board(&path, &me, now).map_err(AppError::msg)?;
    let _ = r.doc().log(ActivityKind::Created, &clean_path(&path).unwrap_or(path), None, None, None, &me, now);
    Ok(())
}

/// Share text from DevCat (a note, a memory, a diff) as a file named `name` in `folder`, or the
/// next free name like it. `from`: what it was ("a note"…), for the room's activity.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_share_text(room: String, name: String, text: String, folder: Option<String>, from: Option<String>) -> AppResult<SharedFile> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let name = free_name(&r, &in_folder(folder.as_deref(), &file_name_of(&name, false))?);
    let note = share(&r, &me, &name, text.as_bytes())?;
    shared(&r, &name, from.as_deref().unwrap_or("DevCat"), None, &me);
    Ok(SharedFile { name, note })
}

/// Record in the room that `by` shared `path` (a file, or a folder of `files`) from `from`.
fn shared(r: &Room, path: &str, from: &str, files: Option<usize>, by: &Author) {
    let _ = r.doc().log(ActivityKind::Shared, path, Some(from), None, files, by, devcat_rooms::now_ms());
}

/// Record that this DevCat's user took a room file or folder out (`to`: their computer, skills,
/// memory, notes, the clipboard), for everyone in the room to see. Readers can too.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_took_out(room: String, path: String, to: String, files: Option<usize>) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = member(&r)?;
    r.doc().log(ActivityKind::TookOut, &path, None, Some(&to), files, &me, devcat_rooms::now_ms()).map_err(AppError::msg)
}

/// Someone's stretch of edits to a file, with their name.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditView {
    /// Who, as the room shows them ("Arif", "Arif's agent (Claude Code)"); a former member or an
    /// unknown DevCat when the room's list doesn't have them.
    pub who: String,
    pub member: Option<String>,
    pub agent: bool,
    pub from: i64,
    pub to: i64,
    pub inserted: usize,
    pub deleted: usize,
    pub changes: usize,
}

/// Who shared, moved, took out and edited a file, from the room's own document (every member's
/// copy tells the same story).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistory {
    /// Its names, newest first.
    pub names: Vec<String>,
    pub activity: Vec<Activity>,
    pub edits: Vec<EditView>,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_history(room: String, path: String) -> AppResult<FileHistory> {
    let r = self::room(&room).await?;
    let d = r.doc();
    let m = r.membership();
    // names from the room's list, else from what they did in it
    let activity = d.activity();
    let name_of = |id: &str| m.member(id).map(|x| x.name.clone()).or_else(|| activity.iter().rev().find(|a| a.by.member == id).map(|a| format!("{} (no longer here)", a.by.name)));
    let edits = d
        .edits(&path)
        .into_iter()
        .map(|e| {
            let name = e.member.as_deref().and_then(name_of).unwrap_or_else(|| "A DevCat the room doesn't know".into());
            let who = match (e.agent, &e.via) {
                (true, Some(v)) => format!("{name}'s agent ({v})"),
                (true, None) => format!("{name}'s agent"),
                _ => name,
            };
            EditView { who, member: e.member, agent: e.agent, from: e.from, to: e.to, inserted: e.inserted, deleted: e.deleted, changes: e.changes }
        })
        .collect();
    Ok(FileHistory { names: d.names_of(&path), activity: d.activity_of(&path), edits })
}

/// Folders a shared folder leaves out: version control, and dependencies and builds that tools
/// make again.
const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules", "target", "__pycache__", ".venv", ".DS_Store", "Thumbs.db"];
/// The most one shared folder brings in.
const MAX_FOLDER_FILES: usize = 500;
const MAX_FOLDER_BYTES: u64 = 200 * 1024 * 1024;

/// The files under `dir` (relative paths with `/`), what a shared folder brings in.
fn folder_files(dir: &std::path::Path) -> AppResult<Vec<(PathBuf, String)>> {
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(PathBuf, String)>, bytes: &mut u64) -> AppResult<()> {
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir).map_err(|e| AppError::msg(format!("Couldn't read {}: {e}", dir.display())))?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if SKIP_DIRS.contains(&name.as_str()) || ft.is_symlink() {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                walk(base, &p, out, bytes)?;
            } else if ft.is_file() {
                *bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                let rel = p.strip_prefix(base).unwrap_or(&p).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
                out.push((p, rel));
                if out.len() > MAX_FOLDER_FILES || *bytes > MAX_FOLDER_BYTES {
                    return Err(AppError::msg(format!(
                        "{} is too large to share in a room ({MAX_FOLDER_FILES} files and {} MB at most, without .git, node_modules and the like). Share a smaller folder.",
                        base.display(),
                        MAX_FOLDER_BYTES / (1024 * 1024)
                    )));
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out, &mut 0)?;
    Ok(out)
}

/// `name` in `folder` of the room (none: at the top).
fn in_folder(folder: Option<&str>, name: &str) -> AppResult<String> {
    match folder.map(str::trim).filter(|f| !f.is_empty() && *f != "/") {
        Some(f) => Ok(format!("{}/{name}", clean_path(f).map_err(AppError::msg)?)),
        None => Ok(name.to_string()),
    }
}

/// A file name alone: not a path on someone's computer. A hidden name (`.env`) only inside a
/// folder that came with it.
fn file_name_of(name: &str, in_folder: bool) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." || (base.starts_with('.') && !in_folder) { "file".into() } else { base.to_string() }
}

/// A folder name that no file in the room is in yet: `name`, or `name (2)`…
fn free_folder(r: &Room, name: &str) -> String {
    let d = r.doc();
    let mut candidate = name.to_string();
    let mut n = 1;
    while d.exists(&candidate) || !d.files_in(&candidate).is_empty() {
        n += 1;
        candidate = format!("{name} ({n})");
    }
    candidate
}

/// Share files and folders from this computer (dropped from DevCat's Files tree, or picked), into
/// `folder` of the room. A folder comes with what's in it, under its own name (or the next free
/// one). Files that can't be shared are told about in the notes; the rest are shared.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_paths_add(room: String, paths: Vec<String>, folder: Option<String>, from: Option<String>) -> AppResult<Vec<SharedFile>> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let from = from.unwrap_or_else(|| "this computer".into());
    let mut out = Vec::new();
    for p in paths {
        let local = PathBuf::from(&p);
        let base = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "folder".into());
        if local.is_dir() {
            let files = folder_files(&local)?;
            let root = free_folder(&r, &in_folder(folder.as_deref(), &base)?);
            for (path, rel) in files {
                let name = format!("{root}/{rel}");
                let shared = match tokio::fs::read(&path).await {
                    Ok(bytes) => share(&r, &me, &name, &bytes).map(|note| SharedFile { name: name.clone(), note }),
                    Err(e) => Err(AppError::msg(format!("Couldn't read {}: {e}", path.display()))),
                };
                out.push(shared.unwrap_or_else(|e| SharedFile { name: String::new(), note: Some(format!("{rel} wasn't shared: {e}")) }));
            }
            let n = out.iter().filter(|s| s.name.starts_with(&format!("{root}/"))).count();
            shared(&r, &root, &from, Some(n), &me);
        } else {
            let bytes = tokio::fs::read(&local).await.map_err(|e| AppError::msg(format!("Couldn't read {}: {e}", local.display())))?;
            let name = free_name(&r, &in_folder(folder.as_deref(), &base)?);
            let note = share(&r, &me, &name, &bytes)?;
            shared(&r, &name, &from, None, &me);
            out.push(SharedFile { name, note });
        }
    }
    Ok(out)
}

/// Write a room file to `dest` on this computer: a text file as it is now, a whiteboard as an
/// Excalidraw file, a shared file as it was shared.
async fn write_out(r: &Room, f: &FileInfo, dest: &std::path::Path) -> AppResult<()> {
    let bytes = match f.meta.kind {
        FileKind::Text => r.doc().read(&f.path).unwrap_or_default().into_bytes(),
        FileKind::Board => serde_json::to_vec_pretty(&json!({ "type": "excalidraw", "version": 2, "source": "DevCat", "elements": r.doc().board_shapes(&f.path), "appState": {}, "files": {} })).map_err(|e| AppError::msg(e.to_string()))?,
        FileKind::Blob => {
            let hash = f.meta.blob.as_ref().map(|b| b.hash.clone()).unwrap_or_default();
            r.blob(&hash).await.map_err(AppError::msg)?
        }
    };
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| AppError::msg(format!("Couldn't make {}: {e}", parent.display())))?;
    }
    tokio::fs::write(dest, bytes).await.map_err(|e| AppError::msg(format!("Couldn't save {}: {e}", dest.display())))
}

/// Save one room file to `dest` on this computer (`to`: what to tell the room, default "their
/// computer").
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_save(room: String, path: String, dest: String, to: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    let me = member(&r)?;
    let f = r.doc().files().into_iter().find(|f| f.path == path).ok_or_else(|| AppError::msg(format!("{path} isn't in the room.")))?;
    write_out(&r, &f, std::path::Path::new(&dest)).await?;
    let _ = r.doc().log(ActivityKind::TookOut, &path, None, Some(to.as_deref().unwrap_or("their computer")), None, &me, devcat_rooms::now_ms());
    Ok(())
}

/// Save a room folder (its files, with their folders) into `dest` on this computer. A `dest`
/// that already has files is refused unless `replace` (then files of the same name are
/// overwritten; others stay). Returns how many files.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_folder_save(room: String, path: String, dest: String, replace: bool, to: Option<String>) -> AppResult<usize> {
    let r = self::room(&room).await?;
    let me = member(&r)?;
    let dir = clean_path(&path).map_err(AppError::msg)?;
    let files = r.doc().files_in(&dir);
    if files.is_empty() {
        return Err(AppError::msg(format!("{dir} has no files.")));
    }
    let dest = PathBuf::from(dest);
    let full = std::fs::read_dir(&dest).map(|mut d| d.next().is_some()).unwrap_or(false);
    if full && !replace {
        return Err(AppError::msg(format!("{} already exists and has files in it.", dest.display())));
    }
    // A file's name is whatever the member who shared it wrote (any key, with `..` or a drive):
    // only plain names under the folder, checked before anything is written.
    let mut targets = Vec::with_capacity(files.len());
    for f in &files {
        let rel = clean_path(&f.path[dir.len() + 1..]).map_err(|e| AppError::msg(format!("Nothing was saved: {} can't be saved on this computer ({e})", f.path)))?;
        targets.push((f, dest.join(rel)));
    }
    for (f, target) in &targets {
        write_out(&r, f, target).await?;
    }
    let _ = r.doc().log(ActivityKind::TookOut, &dir, None, Some(to.as_deref().unwrap_or("their computer")), Some(files.len()), &me, devcat_rooms::now_ms());
    Ok(files.len())
}

/// A skill on this computer: `<dir>/SKILL.md`, from the home folder or a project folder.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// The skill's folder.
    pub dir: String,
    /// "~" (the home folder: every project), or the project folder it's in.
    pub from: String,
}

/// The skills in the home folder and in `folders` (`.claude/skills`, `.agents/skills`,
/// `.codex/skills`), to share one with a room.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn skills_list(folders: Vec<String>) -> AppResult<Vec<SkillInfo>> {
    let home = devcat_harness::prompt::home_dir();
    let mut out: Vec<SkillInfo> = Vec::new();
    let places = home.iter().map(|h| (h.clone(), "~".to_string())).chain(folders.iter().map(|f| (PathBuf::from(f), f.clone())));
    for (dir, from) in places {
        for s in devcat_harness::prompt::skills(&dir, None) {
            let Some(folder) = s.path.parent() else { continue };
            let folder = folder.to_string_lossy().into_owned();
            if !out.iter().any(|x| x.dir == folder) {
                out.push(SkillInfo { name: s.name, description: s.description, dir: folder, from: from.clone() });
            }
        }
    }
    Ok(out)
}

/// A file just shared: its name in the room, and what to tell the person (text too large to edit
/// together went in read-only).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedFile {
    pub name: String,
    pub note: Option<String>,
}

/// Share a file from this computer: text files become editable room documents, others are shared
/// as they are (images, PDFs, …), and so are text files too large to edit together
/// (`MAX_FILE_CHARS`). `path`: its name in the room (default: the file's name, or the next free
/// `name (2).ext` when the room already has one).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_add(room: String, local: String, path: Option<String>) -> AppResult<SharedFile> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let local = PathBuf::from(local);
    if local.is_dir() {
        return Err(AppError::msg(format!("{} is a folder: share the files in it.", local.display())));
    }
    let bytes = tokio::fs::read(&local).await.map_err(|e| AppError::msg(format!("Couldn't read {}: {e}", local.display())))?;
    let name = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => p,
        None => free_name(&r, &local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())),
    };
    let note = share(&r, &me, &name, &bytes)?;
    shared(&r, &name, "this computer", None, &me);
    Ok(SharedFile { name, note })
}

/// Share a file dropped or pasted into the room (the window has its bytes, base64, not a path),
/// in `folder` of the room (a folder dropped from the system: its own folders), under the next
/// free name like `name (2).ext`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_add_bytes(room: String, name: String, data: String, folder: Option<String>, from: Option<String>) -> AppResult<SharedFile> {
    use base64::Engine;
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|e| AppError::msg(format!("Couldn't read the file: {e}")))?;
    let name = free_name(&r, &in_folder(folder.as_deref(), &file_name_of(&name, folder.is_some()))?);
    let note = share(&r, &me, &name, &bytes)?;
    shared(&r, &name, from.as_deref().unwrap_or("a dropped file"), None, &me);
    Ok(SharedFile { name, note })
}

/// Text becomes a document everyone edits, up to `MAX_FILE_CHARS`; longer text is shared read-only
/// (`text/plain`), anything else as it is. Returns what to tell the person, if anything.
fn share(r: &Room, me: &Author, name: &str, bytes: &[u8]) -> AppResult<Option<String>> {
    let max = devcat_rooms::doc::MAX_FILE_CHARS;
    match std::str::from_utf8(bytes) {
        Ok(text) if text.contains('\0') => r.add_blob(name, bytes, mime_of(name), me).map_err(AppError::msg)?,
        Ok(text) if text.chars().count() <= max => r.doc().create_file(name, text, me, devcat_rooms::now_ms()).map_err(AppError::msg)?,
        Ok(_) => {
            r.add_blob(name, bytes, "text/plain", me).map_err(AppError::msg)?;
            return Ok(Some(format!(
                "{name} is too large to edit together ({} characters at most), so it's shared read-only: everyone can read it in the room, and agents read it in parts.",
                thousands(max)
            )));
        }
        Err(_) => r.add_blob(name, bytes, mime_of(name), me).map_err(AppError::msg)?,
    }
    Ok(None)
}

/// 200000 → "200,000".
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `name`, or `name (2).ext`, `name (3).ext`… when the room already has a file by that name.
fn free_name(r: &Room, name: &str) -> String {
    let d = r.doc();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut candidate = name.to_string();
    let mut n = 1;
    while d.exists(&candidate) {
        n += 1;
        candidate = format!("{stem} ({n}){ext}");
    }
    candidate
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// A shared binary file's content (fetched from a member who has it), base64url.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_blob(room: String, hash: String) -> AppResult<String> {
    let r = self::room(&room).await?;
    r.blob(&hash).await.map(|b| b64(&b)).map_err(AppError::msg)
}

/// Save a shared binary file to `dest` on this computer.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_blob_save(room: String, hash: String, dest: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    let bytes = r.blob(&hash).await.map_err(AppError::msg)?;
    tokio::fs::write(&dest, bytes).await.map_err(|e| AppError::msg(format!("Couldn't save {dest}: {e}")))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_member_remove(room: String, member: String) -> AppResult<()> {
    self::room(&room).await?.remove_member(&member).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_member_role(room: String, member: String, role: Role) -> AppResult<()> {
    self::room(&room).await?.set_role(&member, role).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_settings(room: String, name: Option<String>, agent_edits: Option<bool>) -> AppResult<()> {
    self::room(&room).await?.set_settings(name, agent_edits).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_prefs(room: String, prefs: Prefs) -> AppResult<()> {
    self::room(&room).await?.set_prefs(prefs);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_keeper_add(room: String, keeper: String, name: Option<String>) -> AppResult<()> {
    let r = self::room(&room).await?;
    r.add_keeper(keeper.trim(), name.as_deref().unwrap_or("Keeper"), None).map_err(AppError::msg)
}

/// What this DevCat's user is looking at, shown to the others.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_focus(room: String, file: Option<String>, typing: bool) -> AppResult<()> {
    self::room(&room).await?.set_focus(file, typing).await;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_agent_stop(room: String) -> AppResult<()> {
    crate::rooms_agent::stop(&room);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devcat_rooms::{Change, JoinRequest};

    struct Quiet;
    #[async_trait]
    impl Host for Quiet {
        async fn approve_join(&self, _: JoinRequest) -> Option<Role> {
            None
        }
        fn changed(&self, _: &str, _: Change) {}
        fn message(&self, _: &str, _: &Message) {}
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shared_file_never_takes_the_name_of_one_already_in_the_room() {
        let dir = std::env::temp_dir().join(format!("devcat-rooms-share-{}", devcat_rooms::now_ms()));
        let node = Node::start(Config { dir: dir.join("rooms"), relay: RelayConfig::Off, port: 0, keeper: false, keeper_owners: vec![] }, Arc::new(Quiet)).await.unwrap();
        let r = node.room(&node.create("Docs", "Arif").unwrap()).unwrap();
        let me = r.me(false);
        assert_eq!(share(&r, &me, "notes.md", b"# Notes\n").unwrap(), None);
        share(&r, &me, "shot.png", &[0x89, b'P', b'N', b'G', 0]).unwrap();
        assert_eq!(free_name(&r, "notes.md"), "notes (2).md");
        assert_eq!(free_name(&r, "shot.png"), "shot (2).png");
        assert_eq!(free_name(&r, "other.md"), "other.md");
        share(&r, &me, "notes (2).md", b"again").unwrap();
        assert_eq!(free_name(&r, "notes.md"), "notes (3).md");
        // text became a document, the image a shared file
        assert_eq!(r.doc().read("notes.md").as_deref(), Some("# Notes\n"));
        assert!(r.doc().read("shot.png").is_none());
        // text too large to edit together is shared read-only, and the person is told so
        let big = "a log line\n".repeat(devcat_rooms::doc::MAX_FILE_CHARS / 11 + 1);
        let note = share(&r, &me, "big.log", big.as_bytes()).unwrap();
        assert!(note.is_some_and(|n| n.contains("(200,000 characters at most), so it's shared read-only")));
        let f = r.doc().files().into_iter().find(|f| f.path == "big.log").unwrap();
        assert_eq!(f.meta.blob.as_ref().map(|b| (b.mime.as_str(), b.size as usize)), Some(("text/plain", big.len())));
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shared_folder_brings_its_files_but_not_dependencies() {
        let dir = std::env::temp_dir().join(format!("devcat-rooms-folder-{}", devcat_rooms::now_ms()));
        let skill = dir.join("pdf-manual");
        for (p, body) in [("SKILL.md", "---\nname: pdf-manual\n---\n"), ("scripts/build.py", "print(1)"), (".env.example", "A=1"), ("node_modules/x/index.js", "x"), (".git/HEAD", "ref")] {
            std::fs::create_dir_all(skill.join(p).parent().unwrap()).unwrap();
            std::fs::write(skill.join(p), body).unwrap();
        }
        let files: Vec<String> = folder_files(&skill).unwrap().into_iter().map(|(_, rel)| rel).collect();
        assert_eq!(files, vec![".env.example", "SKILL.md", "scripts/build.py"]);

        let node = Node::start(Config { dir: dir.join("rooms"), relay: RelayConfig::Off, port: 0, keeper: false, keeper_owners: vec![] }, Arc::new(Quiet)).await.unwrap();
        let r = node.room(&node.create("Skills", "Arif").unwrap()).unwrap();
        r.doc().create_file("skills/pdf-manual/SKILL.md", "already here", &r.me(false), 1).unwrap();
        // a folder of that name is taken: the next free one
        assert_eq!(free_folder(&r, &in_folder(Some("skills"), "pdf-manual").unwrap()), "skills/pdf-manual (2)");
        assert_eq!(in_folder(None, "a.md").unwrap(), "a.md");
        assert!(in_folder(Some("../x"), "a.md").is_err());
        assert_eq!((file_name_of("C:\\Users\\x\\.env", false), file_name_of("/a/.env", true), file_name_of("..", true)), ("file".into(), ".env".into(), "file".into()));
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
