//! Notifications for AI agents: "finished" and "needs your input". The rules here (which
//! changes, how long a turn must run, only when away) decide; what passes goes to DevCat Mobile
//! (`to_phones`: an `agent-notify` event to the connected phones, which show it themselves, and
//! `recent` for a phone that was offline) and to the notification channels and linked chats
//! (`integrations::deliver`, which calls `to_phones`). No DevCat server or account.
//! Agent status comes from Claude Code hooks (see hooks.rs).
pub mod hooks;

use crate::error::{AppError, AppResult};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Older files also have the ntfy fields (enabled, server, topic, token): ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifySettings {
    pub on_done: bool,
    pub on_input: bool,
    /// "Finished" only for turns that ran at least this long.
    pub min_seconds: u32,
    /// Skip notifications while a DevCat window is the active app.
    pub only_when_away: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self { on_done: true, on_input: true, min_seconds: 30, only_when_away: true }
    }
}

static DIR: OnceCell<PathBuf> = OnceCell::new();

fn path() -> AppResult<PathBuf> {
    DIR.get().map(|d| d.join("notify.json")).ok_or_else(|| AppError::msg("notifications not initialized"))
}

pub fn load() -> NotifySettings {
    path().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(s: &NotifySettings) -> AppResult<()> {
    let p = path()?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, p)?;
    Ok(())
}

pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir);
    crate::rt::spawn(async {
        if let Err(e) = hooks::start().await {
            eprintln!("agent hooks listener failed: {e}");
        }
    });
}

/// Emit to the desktop windows and phones.
pub(crate) fn emit(event: &str, payload: Value) {
    crate::events::emit(event, payload);
}

#[cfg(feature = "gui")]
pub(crate) fn app_focused() -> bool {
    use tauri::Manager;
    crate::events::app().is_some_and(|a| a.webview_windows().values().any(|w| w.is_focused().unwrap_or(false)))
}

/// No windows: nobody is looking at the computer.
#[cfg(not(feature = "gui"))]
pub(crate) fn app_focused() -> bool {
    false
}

fn project_name(cwd: Option<&str>) -> String {
    cwd.and_then(|c| std::path::Path::new(c).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "agent".into())
}

pub fn fmt_duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The notification (title, body) for a status change, if the settings want one.
pub fn message_for(s: &NotifySettings, status: &hooks::AgentStatus, change: &hooks::Change) -> Option<(String, String)> {
    let project = project_name(status.cwd.as_deref());
    match change {
        hooks::Change::Finished { ran_ms } if s.on_done && *ran_ms >= i64::from(s.min_seconds) * 1000 => {
            Some((format!("{project}: finished"), format!("The agent finished after {}.", fmt_duration(*ran_ms))))
        }
        hooks::Change::NeedsInput { message } if s.on_input => Some((format!("{project}: needs you"), message.clone())),
        _ => None,
    }
}

/// A notification as phones get it (event `agent-notify`, and `recent`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneNote {
    /// This run of DevCat (`seq` starts again when it restarts).
    pub boot: String,
    pub seq: u64,
    pub at: i64,
    pub title: String,
    pub body: String,
    pub needs_you: bool,
    /// From "Send a test": phones show it even while the app is on screen.
    pub test: bool,
    /// What tapping it opens (`integrations::Note::open`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<Value>,
}

/// The last notifications, for a phone that was offline when they went out.
const KEEP_NOTES: usize = 50;
static NOTES: Lazy<Mutex<VecDeque<PhoneNote>>> = Lazy::new(Mutex::default);
static SEQ: AtomicU64 = AtomicU64::new(0);
static BOOT: Lazy<String> = Lazy::new(|| hex::encode(crate::remote::store::random_bytes::<8>()));

/// Send a notification to the paired phones (DevCat Mobile shows it). Callers applied the
/// rules already; "finished" / "needs you" turned off here stays off for phones too.
pub(crate) fn to_phones(title: &str, body: &str, needs_you: bool, test: bool, open: Option<Value>) {
    let s = load();
    if !test && !(if needs_you { s.on_input } else { s.on_done }) {
        return;
    }
    let note = PhoneNote {
        boot: BOOT.clone(),
        seq: SEQ.fetch_add(1, Ordering::Relaxed) + 1,
        at: crate::remote::server::now_ms(),
        title: title.chars().take(200).collect(),
        body: body.chars().take(1000).collect(),
        needs_you,
        test,
        open,
    };
    {
        let mut n = NOTES.lock();
        n.push_back(note.clone());
        while n.len() > KEEP_NOTES {
            n.pop_front();
        }
    }
    emit("agent-notify", serde_json::to_value(&note).unwrap_or(Value::Null));
}

/// The kept notifications (a phone shows the ones newer than what it saw, see `to_phones`).
pub fn recent() -> Value {
    json!({ "boot": *BOOT, "notes": NOTES.lock().iter().filter(|n| !n.test).cloned().collect::<Vec<_>>() })
}

/// A scheduled DevCat Agent job finished (`needs_you` false) or waits for the user, as the
/// settings want it (agent_jobs.rs).
pub(crate) async fn job(title: &str, message: &str, needs_you: bool) {
    let s = load();
    if !(if needs_you { s.on_input } else { s.on_done }) || (s.only_when_away && app_focused()) {
        return;
    }
    // the phones, the notification channels (Discord, Slack, webhooks) and linked chats
    crate::integrations::deliver(crate::integrations::Note { title: title.into(), body: message.into(), needs_you, cwd: None, skip_links: vec![], url: None, open: None }).await;
}

pub(crate) async fn on_change(status: &hooks::AgentStatus, change: hooks::Change) {
    let s = load();
    if s.only_when_away && app_focused() {
        return;
    }
    let Some((title, body)) = message_for(&s, status, &change) else { return };
    // the phones, the notification channels and the chats linked to the agent's folder
    crate::integrations::deliver(crate::integrations::Note {
        title,
        body,
        needs_you: matches!(change, hooks::Change::NeedsInput { .. }),
        cwd: status.cwd.clone(),
        skip_links: vec![],
        url: None,
        open: Some(json!({ "kind": "terminal", "ptyId": status.pty_id })),
    })
    .await;
}

// ---------------------------------------------------------------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_settings() -> AppResult<NotifySettings> {
    Ok(load())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_save(settings: NotifySettings) -> AppResult<NotifySettings> {
    save(&settings)?;
    emit("notify-settings", serde_json::to_value(&settings)?);
    Ok(settings)
}

/// A test notification to the paired phones (the channels have their own test).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_test() -> AppResult<()> {
    to_phones("DevCat", "Test notification: you'll get these when an agent finishes or needs you.", false, true, None);
    Ok(())
}

/// Status of every agent DevCat knows about (from hooks).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agents_status() -> AppResult<Vec<hooks::AgentStatus>> {
    Ok(hooks::all())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(cwd: &str) -> hooks::AgentStatus {
        let (s, _) = hooks::apply("t-msg", &json!({ "hook_event_name": "Stop", "cwd": cwd }), 0);
        s
    }

    #[test]
    fn messages_follow_settings() {
        let s = NotifySettings::default();
        let st = status("/Users/me/Projects/storefront");
        let (title, body) = message_for(&s, &st, &hooks::Change::Finished { ran_ms: 192_000 }).unwrap();
        assert_eq!((title.as_str(), body.as_str()), ("storefront: finished", "The agent finished after 3m 12s."));
        assert!(message_for(&s, &st, &hooks::Change::Finished { ran_ms: 5_000 }).is_none(), "short turn");
        let (title, _) = message_for(&s, &st, &hooks::Change::NeedsInput { message: "Claude needs your permission to use Bash".into() }).unwrap();
        assert_eq!(title, "storefront: needs you");
        let off = NotifySettings { on_input: false, ..s };
        assert!(message_for(&off, &st, &hooks::Change::NeedsInput { message: "x".into() }).is_none());
        assert!(message_for(&off, &st, &hooks::Change::Other).is_none());
    }

    #[test]
    fn old_ntfy_settings_still_load() {
        let s: NotifySettings = serde_json::from_value(json!({ "enabled": true, "server": "https://ntfy.sh", "topic": "devcat-x", "onDone": false, "minSeconds": 5 })).unwrap();
        assert_eq!(s, NotifySettings { on_done: false, min_seconds: 5, ..Default::default() });
    }

    #[test]
    fn phones_get_numbered_notes_and_tests_stay_live_only() {
        let before = SEQ.load(Ordering::Relaxed);
        to_phones("api: finished", "done", false, false, Some(json!({ "kind": "terminal", "ptyId": "T1" })));
        to_phones("DevCat", "test", false, true, None);
        let r = recent();
        let notes = r["notes"].as_array().unwrap();
        let last = notes.last().unwrap();
        assert_eq!((last["title"].as_str(), last["test"].as_bool()), (Some("api: finished"), Some(false)));
        assert_eq!(last["open"]["ptyId"], "T1");
        assert!(last["seq"].as_u64().unwrap() > before);
        assert!(notes.iter().all(|n| n["test"] == false));
        assert_eq!(r["boot"], last["boot"]);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(45_000), "45s");
        assert_eq!(fmt_duration(3_725_000), "1h 02m");
    }
}
