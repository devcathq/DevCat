//! Rooms in the app (crates/rooms, docs/rooms-architecture.md): people on their own networks share
//! documents and a thread, and each person's DevCat Agent can join in. Desktop only, like the SSH
//! workbench: nothing here is reachable from a paired phone (`remote/dispatch.rs`).
//!
//! The rooms node starts on first use (or at launch when this DevCat is in a room), with its own
//! iroh key, and the relay choice of DevCat → Mobile.
//!
//! Events: `rooms-changed` ({ room, what: doc|members|presence|removed }), `room-join-request`
//! (JoinRequest), `room-join-resolved` ({ requestId }), `room-agent` ({ room, running, error? }).
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use devcat_rooms::doc::{ChangeRecord, ClaimView, FileInfo, Message, SuggestionView};
use devcat_rooms::wire::{b64, unb64};
use devcat_rooms::{Author, Change, Config, Host, Invite, JoinRequest, MemberView, Node, Prefs, RelayConfig, Role, Room, RoomSettings, RoomSummary};
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
        Node::start(cfg, Arc::new(AppHost)).await.map_err(AppError::msg)
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
        crate::events::emit("rooms-changed", json!({ "room": room, "what": what }));
    }

    fn message(&self, room: &str, msg: &Message) {
        crate::rooms_agent::on_message(room, msg);
    }
}

// ------------------------------------------------------------------ commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomsInfo {
    /// This DevCat's id in rooms.
    pub id: String,
    pub rooms: Vec<RoomSummary>,
    pub pending: Vec<JoinRequest>,
}

/// `start`: false = don't start rooms just to look (empty when not running yet).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rooms_info(start: Option<bool>) -> AppResult<RoomsInfo> {
    if start == Some(false) && NODE.get().is_none() {
        return Ok(RoomsInfo { id: String::new(), rooms: vec![], pending: vec![] });
    }
    let n = node().await?;
    Ok(RoomsInfo { id: n.id(), rooms: n.rooms(), pending: PENDING.lock().clone() })
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
    Ok(RoomView {
        summary: r.summary(),
        settings: m.settings.clone(),
        me: n.id(),
        owner: r.is_owner(),
        members: r.members(),
        handles,
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
    Ok(RoomBoard { files: d.files(), messages: d.messages(), suggestions: d.suggestions(), claims: d.claims(devcat_rooms::now_ms()), history })
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

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_post(room: String, text: String) -> AppResult<Message> {
    let r = self::room(&room).await?;
    if r.removed() {
        return Err(AppError::msg("You're no longer in this room."));
    }
    r.doc().post(&r.me(false), &text, 0, None, devcat_rooms::now_ms()).map_err(AppError::msg)
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
    r.doc().decide(&id, accept, &me, reason, devcat_rooms::now_ms()).map(|_| ()).map_err(AppError::msg)
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
    r.doc().create_file(&path, content.as_deref().unwrap_or(""), &me, devcat_rooms::now_ms()).map_err(AppError::msg)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_delete(room: String, path: String) -> AppResult<()> {
    let r = self::room(&room).await?;
    editor(&r)?;
    r.doc().delete_file(&path).map_err(AppError::msg)
}

/// Share a file from this computer: text files become editable room documents, others are shared
/// as they are (images, PDFs, …), and so are text files too large to edit together
/// (`MAX_FILE_CHARS`). `path`: its name in the room (default: the file's name).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn room_file_add(room: String, local: String, path: Option<String>) -> AppResult<String> {
    let r = self::room(&room).await?;
    let me = editor(&r)?;
    let local = PathBuf::from(local);
    let name = path.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into()));
    let bytes = tokio::fs::read(&local).await.map_err(|e| AppError::msg(format!("Couldn't read {}: {e}", local.display())))?;
    let now = devcat_rooms::now_ms();
    match std::str::from_utf8(&bytes) {
        Ok(text) if text.contains('\0') => r.add_blob(&name, &bytes, mime_of(&name), &me).map_err(AppError::msg)?,
        Ok(text) if text.chars().count() <= devcat_rooms::doc::MAX_FILE_CHARS => r.doc().create_file(&name, text, &me, now).map_err(AppError::msg)?,
        Ok(_) => r.add_blob(&name, &bytes, "text/plain", &me).map_err(AppError::msg)?,
        Err(_) => r.add_blob(&name, &bytes, mime_of(&name), &me).map_err(AppError::msg)?,
    }
    Ok(name)
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
