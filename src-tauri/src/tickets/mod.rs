//! Tickets: connect a Jira project, Trello board or Linear team, and let an agent work through the
//! tickets in one column. For each ticket DevCat makes a branch in the board's repository, runs
//! the agent (Claude Code or DevCat Agent) on it, commits what it changed, comments on the ticket
//! and moves it on (e.g. To Do → In Progress → Testing). A board can also watch its column and
//! pick up new tickets by itself, and post what happened to a Slack webhook.
//!
//! DevCat, not the agent, talks to the tracker: the agent only gets the ticket's text and the
//! repository, so a ticket can't make it change other tickets or boards.
//!
//! Boards are in `<data>/tickets/boards.json`; their tokens and webhook in the keychain
//! (`tickets:<id>:token`, `tickets:<id>:webhook`); what was done per board in
//! `<data>/tickets/history/<id>.json`. Desktop only: not reachable from paired phones.
//!
//! Events: `tickets-job` ({ board, job | null }) while a board works, `tickets-changed`
//! ({ board }) when its history changes.
pub mod trackers;
pub mod worker;

use crate::error::{AppError, AppResult};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

static DIR: OnceCell<PathBuf> = OnceCell::new();
/// Board file writes, one at a time.
static WRITE: Mutex<()> = Mutex::new(());
/// Runs kept per board.
const HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Jira,
    Trello,
    Linear,
}

impl TrackerKind {
    pub fn label(self) -> &'static str {
        match self {
            TrackerKind::Jira => "Jira",
            TrackerKind::Trello => "Trello",
            TrackerKind::Linear => "Linear",
        }
    }
}

/// Whose tickets the agent takes: anyone's, the token owner's, or theirs and unassigned ones.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Assigned {
    #[default]
    Anyone,
    Me,
    MeOrNobody,
}

/// A column of a board: a Jira status, a Trello list, a Linear workflow state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Column {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub text: String,
}

/// A file attached to a ticket (read when the agent starts on it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub name: String,
    /// What the tracker says it is ("" when it doesn't say).
    pub mime: String,
    pub url: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    /// What the API addresses it by (Jira key, Trello card id, Linear issue id).
    pub id: String,
    /// What people call it: ABC-12, #7, ENG-4.
    pub key: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub labels: Vec<String>,
    /// Who it's assigned to (names).
    #[serde(default)]
    pub assignees: Vec<String>,
    /// Column id it's in.
    pub column: String,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

fn yes() -> bool {
    true
}
fn ten() -> u32 {
    10
}
fn three() -> u32 {
    3
}

/// A connected board and what the agent does with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: TrackerKind,
    /// Jira: the site (https://acme.atlassian.net, or a Data Center URL).
    #[serde(default)]
    pub site: String,
    /// Jira Cloud: the account's email (empty: the token is a Data Center personal access token).
    /// Trello: the API key.
    #[serde(default)]
    pub user: String,
    /// Jira: project key. Trello: board link or id. Linear: team key.
    #[serde(default)]
    pub project: String,
    /// Write-only: Some(token) stores it in the keychain, None keeps the saved one.
    #[serde(default, skip_serializing)]
    pub token: Option<String>,
    #[serde(default)]
    pub has_token: bool,
    /// Write-only, like `token`: a Slack (or Mattermost, Google Chat) incoming webhook; "" removes it.
    #[serde(default, skip_serializing)]
    pub webhook: Option<String>,
    #[serde(default)]
    pub has_webhook: bool,

    /// The git repository the agent works in, or a folder of repositories (the agent may change any
    /// of them; each gets the ticket's branch).
    #[serde(default)]
    pub repo: String,
    /// The DevCat project the board works for (None: the one whose code folder holds `repo`, if
    /// any). The agent gets its memory and connections, like a chat in that project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// AI connection that does the work (Claude Code or DevCat Agent).
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Column the agent takes tickets from.
    #[serde(default)]
    pub pick_from: Option<Column>,
    /// Where a ticket goes while the agent works on it (None: it stays put).
    #[serde(default)]
    pub working: Option<Column>,
    /// Where a fixed ticket goes (e.g. Testing).
    #[serde(default)]
    pub done_to: Option<Column>,
    /// Where a ticket goes when the agent couldn't finish it (None: back where it was).
    #[serde(default)]
    pub failed_to: Option<Column>,
    /// Only tickets with this label (empty: all of the column).
    #[serde(default)]
    pub label: String,
    /// Only tickets assigned to the token's owner (and, with MeOrNobody, unassigned ones).
    #[serde(default)]
    pub assigned: Assigned,
    /// Comment on the ticket with what was done.
    #[serde(default = "yes")]
    pub comment: bool,
    /// Push the ticket's branch to origin.
    #[serde(default)]
    pub push: bool,
    /// The agent may run commands (builds, tests) without asking.
    #[serde(default)]
    pub allow_commands: bool,
    /// Check the column every `interval_min` minutes and work on new tickets.
    #[serde(default)]
    pub watch: bool,
    #[serde(default = "ten")]
    pub interval_min: u32,
    /// Tickets per check, at most.
    #[serde(default = "three")]
    pub max_per_run: u32,
    /// Added to every ticket's prompt (conventions, how to test…).
    #[serde(default)]
    pub instructions: String,
}

/// What happened to one ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub ticket_id: String,
    pub key: String,
    pub title: String,
    pub url: String,
    /// fixed | needs_info | failed | error | stopped
    pub outcome: String,
    pub summary: String,
    pub testing: String,
    pub branch: Option<String>,
    pub files_changed: usize,
    /// A folder of repositories: the ones the agent changed, "name (n files)".
    #[serde(default)]
    pub repos: Vec<String>,
    /// Column it was moved to (name).
    pub moved_to: Option<String>,
    /// Things that went wrong around the work (a move or comment the tracker refused…).
    pub notes: Vec<String>,
    pub started: i64,
    pub finished: i64,
    pub cost_usd: Option<f64>,
}

pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir.join("tickets"));
    worker::start_watch();
}

fn dir() -> AppResult<&'static PathBuf> {
    DIR.get().ok_or_else(|| AppError::msg("Tickets are not initialised"))
}

fn boards_path() -> AppResult<PathBuf> {
    Ok(dir()?.join("boards.json"))
}

fn token_key(id: &str) -> String {
    format!("tickets:{id}:token")
}

fn webhook_key(id: &str) -> String {
    format!("tickets:{id}:webhook")
}

fn write_json<T: Serialize>(path: &PathBuf, v: &T) -> AppResult<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(v)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_boards() -> AppResult<Vec<Board>> {
    let p = boards_path()?;
    if !p.exists() {
        return Ok(vec![]);
    }
    let mut list: Vec<Board> = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    for b in &mut list {
        b.has_token = crate::secrets::get(&token_key(&b.id)).is_some();
        b.has_webhook = crate::secrets::get(&webhook_key(&b.id)).is_some();
    }
    Ok(list)
}

pub fn board(id: &str) -> AppResult<Board> {
    load_boards()?.into_iter().find(|b| b.id == id).ok_or_else(|| AppError::msg("That board is gone"))
}

pub fn token(b: &Board) -> String {
    b.token.clone().filter(|t| !t.trim().is_empty()).or_else(|| crate::secrets::get(&token_key(&b.id))).unwrap_or_default()
}

pub fn webhook(b: &Board) -> Option<String> {
    match &b.webhook {
        Some(w) => Some(w.trim().to_string()).filter(|w| !w.is_empty()),
        None => crate::secrets::get(&webhook_key(&b.id)),
    }
}

pub fn history(id: &str) -> Vec<RunRecord> {
    dir().ok().and_then(|d| std::fs::read_to_string(d.join("history").join(format!("{id}.json"))).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn record(board: &str, r: RunRecord) {
    let _g = WRITE.lock();
    let mut h = history(board);
    h.insert(0, r);
    h.truncate(HISTORY);
    if let Ok(d) = dir() {
        let _ = write_json(&d.join("history").join(format!("{board}.json")), &h);
    }
    crate::events::emit("tickets-changed", serde_json::json!({ "board": board }));
}

/// Ids end up in keychain entry names.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ---------------------------------------------------------------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_boards() -> AppResult<Vec<Board>> {
    load_boards()
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_board_save(board: Board) -> AppResult<Board> {
    let mut b = board;
    if b.id.is_empty() {
        b.id = uuid::Uuid::new_v4().to_string();
    }
    if !valid_id(&b.id) {
        return Err(AppError::msg("Bad board id"));
    }
    if b.name.trim().is_empty() {
        b.name = format!("{} {}", b.kind.label(), b.project.trim());
    }
    b.interval_min = b.interval_min.clamp(1, 24 * 60);
    b.max_per_run = b.max_per_run.clamp(1, 20);
    if let Some(t) = b.token.take().filter(|t| !t.trim().is_empty()) {
        crate::secrets::set(&token_key(&b.id), t.trim())?;
    }
    if let Some(w) = b.webhook.take() {
        let w = w.trim();
        if !w.is_empty() && !w.starts_with("https://") {
            return Err(AppError::msg("The webhook must be an https:// URL"));
        }
        crate::secrets::set(&webhook_key(&b.id), w)?;
    }
    {
        let _g = WRITE.lock();
        let mut list = load_boards()?;
        match list.iter_mut().find(|x| x.id == b.id) {
            Some(x) => *x = b.clone(),
            None => list.push(b.clone()),
        }
        let clean: Vec<Board> = list.into_iter().map(|x| Board { token: None, webhook: None, has_token: false, has_webhook: false, ..x }).collect();
        write_json(&boards_path()?, &clean)?;
    }
    self::board(&b.id)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_board_delete(id: String) -> AppResult<()> {
    worker::stop(&id);
    let _g = WRITE.lock();
    let list: Vec<Board> = load_boards()?.into_iter().filter(|b| b.id != id).map(|x| Board { has_token: false, has_webhook: false, ..x }).collect();
    write_json(&boards_path()?, &list)?;
    crate::secrets::delete(&token_key(&id));
    crate::secrets::delete(&webhook_key(&id));
    if let Ok(d) = dir() {
        let _ = std::fs::remove_file(d.join("history").join(format!("{id}.json")));
    }
    Ok(())
}

/// The board's columns. `board` may be unsaved (the connect form), with its token typed in.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_columns(board: Board) -> AppResult<Vec<Column>> {
    trackers::Tracker::new(&board, token(&board))?.columns().await
}

/// Tickets in a column of a saved board (filtered by the board's label).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_list(board_id: String, column: String) -> AppResult<Vec<Ticket>> {
    let b = board(&board_id)?;
    let list = trackers::Tracker::new(&b, token(&b))?.list(&column).await?;
    Ok(worker::with_label(&b, list))
}

/// Queue tickets for the agent (it works on them one at a time).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_fix(board_id: String, ticket_ids: Vec<String>) -> AppResult<()> {
    worker::enqueue(board(&board_id)?, ticket_ids)
}

/// Stop the ticket being worked on and drop the queue.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_stop(board_id: String) -> AppResult<()> {
    worker::stop(&board_id);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_jobs() -> AppResult<std::collections::HashMap<String, worker::JobView>> {
    Ok(worker::jobs())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_history(board_id: String) -> AppResult<Vec<RunRecord>> {
    Ok(history(&board_id))
}

/// Send a test message to the board's webhook (typed in, or the saved one).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn tickets_webhook_test(board: Board) -> AppResult<()> {
    let url = webhook(&board).ok_or_else(|| AppError::msg("No webhook set"))?;
    trackers::post_webhook(&url, &format!("DevCat is connected: updates from \"{}\" will be posted here.", board.name)).await
}
