//! Memory sync: your computers share one memory, peer to peer over iroh, with no DevCat server
//! and no account. Pair two computers once (a one-time code from one, pasted on the other, allowed
//! there); from then on their memory folders converge whenever both are online.
//!
//! - [`plan`]: what's shared and how two versions of a file are decided (no network).
//! - [`node`]: the sync endpoint, pairing and sessions; [`wire`]: what computers say;
//!   [`store`]: peers, the index and the audit log in `<app data>/memory-sync/`.
//!
//! Personal devices only: every paired computer reads and writes all of the memory. Teams (roles,
//! a signed member list, content encryption) are a later step. Like the rest of memory it's
//! desktop only (and `devcat serve`): the phone dispatcher doesn't offer it (AGENTS.md).
//!
//! Events: `memory-sync` (SyncStatus), `memory-sync-request` (PairRequest),
//! `memory-sync-resolved` ({ requestId }), `memory-changed` (memories arrived).
pub mod node;
pub mod plan;
pub mod store;
pub mod wire;

use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use node::{Config, Host, Node, PairRequest, Report};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use store::{Peer, Store};
use tokio::sync::oneshot;

/// (sync's folder, the memory folder)
static DIRS: OnceCell<(PathBuf, PathBuf)> = OnceCell::new();
static NODE: tokio::sync::OnceCell<Node> = tokio::sync::OnceCell::const_new();
static APPROVALS: Lazy<Mutex<HashMap<String, oneshot::Sender<bool>>>> = Lazy::new(Default::default);
static PENDING: Lazy<Mutex<Vec<PairRequest>>> = Lazy::new(Default::default);
/// The code shown for pairing, so the dialog shows it again when reopened.
static TICKET: Mutex<Option<Ticket>> = Mutex::new(None);

pub fn init(data_dir: PathBuf) {
    let _ = DIRS.set((data_dir.join("memory-sync"), data_dir.join("memory")));
    // Only DevCats with a paired computer open the sync socket at launch (`devcat serve` starts it
    // itself, `devcat mcp` never).
    if !crate::events::headless() && has_peers() {
        crate::rt::spawn(async {
            if let Err(e) = node().await {
                eprintln!("memory sync: {e}");
            }
        });
    }
}

fn store() -> AppResult<Store> {
    DIRS.get().map(|(d, _)| Store::new(d)).ok_or_else(|| AppError::msg("Memory sync isn't initialised"))
}

fn has_peers() -> bool {
    store().is_ok_and(|s| !s.peers().is_empty())
}

async fn node() -> AppResult<&'static Node> {
    NODE.get_or_try_init(|| async {
        let (dir, memory) = DIRS.get().ok_or_else(|| AppError::msg("Memory sync isn't initialised"))?.clone();
        let cfg = Config { dir, memory, relay: crate::remote::relay_value(), port: 0, auto: true };
        let n = Node::start(cfg, Arc::new(AppHost)).await.map_err(AppError::msg)?;
        n.sync_all();
        Ok(n)
    })
    .await
}

/// `devcat serve`: sync with the paired computers while it runs. The number of them.
pub async fn start_headless() -> AppResult<usize> {
    let n = store()?.peers().len();
    if n > 0 {
        node().await?;
    }
    Ok(n)
}

pub async fn shutdown() {
    if let Some(n) = NODE.get() {
        n.shutdown().await;
    }
}

struct AppHost;

#[async_trait]
impl Host for AppHost {
    async fn approve(&self, req: PairRequest) -> bool {
        // nobody answering (no window open) times out as a refusal (node::APPROVAL)
        let (tx, rx) = oneshot::channel();
        APPROVALS.lock().insert(req.request_id.clone(), tx);
        PENDING.lock().push(req.clone());
        crate::events::emit("memory-sync-request", &req);
        push_status();
        let answer = rx.await.unwrap_or(false);
        APPROVALS.lock().remove(&req.request_id);
        PENDING.lock().retain(|p| p.request_id != req.request_id);
        *TICKET.lock() = None;
        crate::events::emit("memory-sync-resolved", json!({ "requestId": req.request_id }));
        push_status();
        answer
    }

    fn name(&self) -> String {
        crate::remote::this_computer()
    }

    fn projects(&self) -> Vec<crate::models::Project> {
        super::projects()
    }

    fn changed(&self) {
        push_status();
    }

    fn memories_changed(&self) {
        crate::events::emit("memory-changed", json!({}));
        // the rules files list the memories for every project
        crate::rt::spawn_blocking(super::rules::refresh);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    /// `devcat://memory-sync?…`: paste it on the other computer.
    pub ticket: String,
    pub code: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    #[serde(flatten)]
    pub peer: Peer,
    /// A sync with it is running now.
    pub syncing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Sync is running on this computer (it starts with the first pairing).
    pub running: bool,
    /// This computer's name, as the others show it.
    pub name: String,
    pub peers: Vec<PeerView>,
    /// Pairing requests waiting for an answer here.
    pub pending: Vec<PairRequest>,
    /// The code being shown, while it's valid.
    pub ticket: Option<Ticket>,
}

fn status_now() -> AppResult<SyncStatus> {
    let node = NODE.get();
    let syncing = node.map(Node::syncing).unwrap_or_default();
    let peers = store()?.peers().into_iter().map(|p| PeerView { syncing: syncing.contains(&p.id), peer: p }).collect();
    let ticket = TICKET.lock().clone().filter(|t| t.expires_at > node::now_ms() && node.is_some_and(Node::pairing_live));
    Ok(SyncStatus { running: node.is_some(), name: crate::remote::this_computer(), peers, pending: PENDING.lock().clone(), ticket })
}

fn push_status() {
    if let Ok(s) = status_now() {
        crate::events::emit("memory-sync", s);
    }
}

// ---------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_status() -> AppResult<SyncStatus> {
    status_now()
}

/// A one-time code for pairing another computer with this one (valid 10 minutes).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_pair_start() -> AppResult<Ticket> {
    let (ticket, code, expires_at) = node().await?.pair_start().await;
    let t = Ticket { ticket, code, expires_at };
    *TICKET.lock() = Some(t.clone());
    push_status();
    Ok(t)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_pair_cancel() -> AppResult<()> {
    if let Some(n) = NODE.get() {
        n.pair_cancel();
    }
    *TICKET.lock() = None;
    push_status();
    Ok(())
}

/// Pair with the computer that showed this code (waits for its user to allow it).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_join(ticket: String) -> AppResult<Peer> {
    let peer = node().await?.join(&ticket).await.map_err(AppError::msg)?;
    push_status();
    Ok(peer)
}

/// Answer a pairing request here.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_answer(request_id: String, allow: bool) -> AppResult<()> {
    let tx = APPROVALS.lock().remove(&request_id).ok_or_else(|| AppError::msg("That request is gone (it timed out or was answered)."))?;
    let _ = tx.send(allow);
    Ok(())
}

/// Unpair a computer. What it already has stays there.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_remove(id: String) -> AppResult<SyncStatus> {
    match NODE.get() {
        Some(n) => {
            n.remove(&id);
        }
        None => {
            store()?.remove_peer(&id);
        }
    }
    status_now()
}

/// Sync now: with one computer (waits, returns what moved) or with all (in the background).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_now(id: Option<String>) -> AppResult<Option<Report>> {
    let n = node().await?;
    match id {
        Some(id) => n.sync_peer(&id).await.map(Some).map_err(AppError::msg),
        None => {
            n.sync_all();
            Ok(None)
        }
    }
}

/// Pairings, syncs that moved something, and failures (newest first).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn memory_sync_activity(limit: Option<usize>) -> AppResult<Vec<crate::remote::store::AuditEntry>> {
    Ok(store()?.audit_tail(limit.unwrap_or(50).min(500)))
}

#[cfg(test)]
mod tests;
