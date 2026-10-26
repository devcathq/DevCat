//! The MCP activity log: every tool call an agent makes through `devcat mcp` (`mcp_server.rs`),
//! with the agent, the connection, what it ran, whether it worked and how long it took. Refused
//! writes are logged too. Each `devcat mcp` process appends to `<data>/mcp-audit.jsonl` (private
//! to the user, trimmed now and then, like the remote access log in `remote/store.rs`), and the
//! app shows it under Sync MCP → Activity. Nothing leaves this computer.
use crate::error::{AppError, AppResult};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// Entries kept when the file is trimmed (about half of them; the newest).
const KEEP: usize = 2000;
/// Longest statement / error kept in an entry.
const MAX_DETAIL: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Unix ms.
    pub at: i64,
    /// The agent, as `devcat mcp` names it (`claude-code`, `codex`, …).
    pub client: String,
    pub tool: String,
    /// The connection the call named, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// What it ran (the query, the object, the memory), shortened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The DevCat project of the agent's folder, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub ok: bool,
    /// Why it failed (or was refused), shortened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: u64,
}

static DIR: OnceCell<PathBuf> = OnceCell::new();

pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir);
}

fn path() -> Option<PathBuf> {
    DIR.get().map(|d| d.join("mcp-audit.jsonl"))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The first `MAX_DETAIL` characters of `s`, on one line; `None` when empty.
pub fn short(s: &str) -> Option<String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        return None;
    }
    Some(if s.chars().count() > MAX_DETAIL { format!("{}…", s.chars().take(MAX_DETAIL).collect::<String>()) } else { s })
}

/// Append one entry (a failure to log never fails the call).
pub fn log(e: &Entry) {
    let Some(path) = path() else { return };
    let Ok(line) = serde_json::to_string(e) else { return };
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        // one write per line: agents running side by side append whole lines
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
    if std::fs::metadata(&path).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let keep = tail(KEEP / 2);
        let text: String = keep.iter().rev().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        let _ = crate::network::store::write_private(&path, text.as_bytes());
    }
}

/// The newest `n` entries, newest first.
pub fn tail(n: usize) -> Vec<Entry> {
    let text = path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().rev().filter_map(|l| serde_json::from_str(l).ok()).take(n).collect()
}

// ---------- commands

/// The MCP activity log, newest first.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn mcp_audit(limit: Option<usize>) -> AppResult<Vec<Entry>> {
    let n = limit.unwrap_or(300).clamp(1, KEEP);
    tokio::task::spawn_blocking(move || tail(n)).await.map_err(|e| AppError::msg(e.to_string()))
}

/// Empty the MCP activity log.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn mcp_audit_clear() -> AppResult<()> {
    let path = path().ok_or_else(|| AppError::msg("DevCat isn't ready yet"))?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh log folder for this test process (the log's folder is set once).
    pub(crate) fn dir() -> PathBuf {
        DIR.get_or_init(|| {
            let d = std::env::temp_dir().join(format!("devcat-mcp-audit-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).unwrap();
            d
        })
        .clone()
    }

    #[test]
    fn details_are_one_short_line() {
        assert_eq!(short("  select *\n  from t "), Some("select * from t".into()));
        assert_eq!(short(" \n "), None);
        let long = short(&"x".repeat(MAX_DETAIL + 10)).unwrap();
        assert_eq!(long.chars().count(), MAX_DETAIL + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn entries_read_back_newest_first() {
        dir();
        let e = |tool: &str, ok: bool| Entry { at: now_ms(), client: "codex".into(), tool: tool.into(), connection: Some("shop".into()), detail: None, project: None, ok, error: None, ms: 1 };
        log(&e("audit-test-a", true));
        log(&e("audit-test-b", false));
        let mine: Vec<_> = tail(KEEP).into_iter().filter(|x| x.tool.starts_with("audit-test-")).collect();
        assert_eq!(mine.iter().map(|x| x.tool.as_str()).collect::<Vec<_>>(), ["audit-test-b", "audit-test-a"]);
        assert!(!mine[0].ok);
        let line = std::fs::read_to_string(path().unwrap()).unwrap();
        assert!(!line.contains("\"detail\""), "empty fields aren't written");
    }
}
