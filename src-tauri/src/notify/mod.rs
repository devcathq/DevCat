//! Phone notifications for AI agents: "finished" and "needs your input", pushed through ntfy
//! (https://ntfy.sh or a self-hosted server) to the ntfy app on the user's phone. No DevCat
//! server or account: the computer posts straight to the user's private topic.
//! Agent status comes from Claude Code hooks (see hooks.rs).
pub mod hooks;

use crate::error::{AppError, AppResult};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifySettings {
    pub enabled: bool,
    /// ntfy server, e.g. https://ntfy.sh
    pub server: String,
    /// Private topic name (random; whoever knows it can read the notifications).
    pub topic: String,
    /// Access token for a protected self-hosted server.
    pub token: Option<String>,
    pub on_done: bool,
    pub on_input: bool,
    /// "Finished" only for turns that ran at least this long.
    pub min_seconds: u32,
    /// Skip notifications while a DevCat window is the active app.
    pub only_when_away: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            server: "https://ntfy.sh".into(),
            topic: String::new(),
            token: None,
            on_done: true,
            on_input: true,
            min_seconds: 30,
            only_when_away: true,
        }
    }
}

static DIR: OnceCell<PathBuf> = OnceCell::new();

fn path() -> AppResult<PathBuf> {
    DIR.get().map(|d| d.join("notify.json")).ok_or_else(|| AppError::msg("notifications not initialized"))
}

/// `devcat-` + 16 random lowercase base32 characters (80 bits): hard to guess.
pub fn new_topic() -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let bytes: [u8; 16] = crate::remote::store::random_bytes();
    let s: String = bytes.iter().map(|b| ALPHA[(*b as usize) % 32] as char).collect();
    format!("devcat-{s}")
}

pub fn load() -> NotifySettings {
    let mut s: NotifySettings = path().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if s.topic.is_empty() {
        s.topic = new_topic();
        let _ = save(&s);
    }
    s
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
fn app_focused() -> bool {
    use tauri::Manager;
    crate::events::app().is_some_and(|a| a.webview_windows().values().any(|w| w.is_focused().unwrap_or(false)))
}

/// No windows: nobody is looking at the computer.
#[cfg(not(feature = "gui"))]
fn app_focused() -> bool {
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

/// The ntfy message for a status change, if the settings want one.
pub fn message_for(s: &NotifySettings, status: &hooks::AgentStatus, change: &hooks::Change) -> Option<Value> {
    let project = project_name(status.cwd.as_deref());
    let (title, body, tags, priority) = match change {
        hooks::Change::Finished { ran_ms } if s.on_done && *ran_ms >= i64::from(s.min_seconds) * 1000 => {
            (format!("{project}: finished"), format!("The agent finished after {}.", fmt_duration(*ran_ms)), "white_check_mark", 3)
        }
        hooks::Change::NeedsInput { message } if s.on_input => (format!("{project}: needs you"), message.clone(), "raising_hand", 4),
        _ => return None,
    };
    Some(json!({ "topic": s.topic, "title": title, "message": body, "tags": [tags], "priority": priority }))
}

async fn publish(s: &NotifySettings, msg: &Value) -> AppResult<()> {
    let server = s.server.trim().trim_end_matches('/');
    if !(server.starts_with("https://") || server.starts_with("http://")) {
        return Err(AppError::msg("The ntfy server must start with https://"));
    }
    let mut req = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?.post(format!("{server}/")).json(msg);
    if let Some(t) = s.token.as_deref().filter(|t| !t.is_empty()) {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(AppError::msg(format!("ntfy answered {code}: {}", text.trim())));
    }
    Ok(())
}

/// A scheduled DevCat Agent job finished (`needs_you` false) or waits for the user, as the
/// settings want it (agent_jobs.rs).
pub(crate) async fn job(title: &str, message: &str, needs_you: bool) {
    let s = load();
    if !s.enabled || !(if needs_you { s.on_input } else { s.on_done }) || (s.only_when_away && app_focused()) {
        return;
    }
    let (tags, priority) = if needs_you { ("raising_hand", 4) } else { ("alarm_clock", 3) };
    let msg = json!({ "topic": s.topic, "title": title, "message": message, "tags": [tags], "priority": priority });
    if let Err(e) = publish(&s, &msg).await {
        eprintln!("notification failed: {e}");
    }
}

pub(crate) async fn on_change(status: &hooks::AgentStatus, change: hooks::Change) {
    let s = load();
    if !s.enabled || (s.only_when_away && app_focused()) {
        return;
    }
    if let Some(msg) = message_for(&s, status, &change) {
        if let Err(e) = publish(&s, &msg).await {
            eprintln!("notification failed: {e}");
        }
    }
}

// ---------------------------------------------------------------- commands

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_settings() -> AppResult<NotifySettings> {
    Ok(load())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_save(settings: NotifySettings) -> AppResult<NotifySettings> {
    let mut s = settings;
    s.server = s.server.trim().trim_end_matches('/').to_string();
    if s.server.is_empty() {
        s.server = NotifySettings::default().server;
    }
    if s.topic.trim().is_empty() {
        s.topic = new_topic();
    }
    s.topic = s.topic.trim().to_string();
    save(&s)?;
    emit("notify-settings", serde_json::to_value(&s)?);
    Ok(s)
}

/// A new private topic (the old one stops getting notifications).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_new_topic() -> AppResult<NotifySettings> {
    let mut s = load();
    s.topic = new_topic();
    notify_save(s).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn notify_test() -> AppResult<()> {
    let s = load();
    let msg = json!({
        "topic": s.topic,
        "title": "DevCat",
        "message": "Test notification: you'll get these when an agent finishes or needs you.",
        "tags": ["tada"],
    });
    publish(&s, &msg).await
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
        let s = NotifySettings { enabled: true, topic: "devcat-x".into(), ..Default::default() };
        let st = status("/Users/me/Projects/storefront");
        let m = message_for(&s, &st, &hooks::Change::Finished { ran_ms: 192_000 }).unwrap();
        assert_eq!(m["title"], "storefront: finished");
        assert_eq!(m["message"], "The agent finished after 3m 12s.");
        assert_eq!(m["topic"], "devcat-x");
        assert!(message_for(&s, &st, &hooks::Change::Finished { ran_ms: 5_000 }).is_none(), "short turn");
        let m = message_for(&s, &st, &hooks::Change::NeedsInput { message: "Claude needs your permission to use Bash".into() }).unwrap();
        assert_eq!((m["title"].as_str(), m["priority"].as_i64()), (Some("storefront: needs you"), Some(4)));
        let off = NotifySettings { on_input: false, ..s };
        assert!(message_for(&off, &st, &hooks::Change::NeedsInput { message: "x".into() }).is_none());
        assert!(message_for(&off, &st, &hooks::Change::Other).is_none());
    }

    #[test]
    fn topics_and_durations() {
        let t = new_topic();
        assert!(t.starts_with("devcat-") && t.len() == 23 && t != new_topic());
        assert_eq!(fmt_duration(45_000), "45s");
        assert_eq!(fmt_duration(3_725_000), "1h 02m");
    }
}
