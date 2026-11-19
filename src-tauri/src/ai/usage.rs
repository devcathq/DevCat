//! How much of a coding agent's plan is used: Claude Code's session (5-hour) and weekly limits, as
//! its `/usage` shows them, and Antigravity's (`agy`) 5-hour and weekly pools. For the AI
//! connections list, the title bar and the phone (`ai_usage`, a read).
//!
//! - Claude Code: the account's OAuth login (macOS keychain `Claude Code-credentials`, with
//!   `-<first 8 hex of sha256(CLAUDE_CONFIG_DIR)>` for a config folder of its own, read with
//!   `/usr/bin/security` as Claude Code writes it; `.credentials.json` elsewhere) asks
//!   `api.anthropic.com/api/oauth/usage`. The token is only read, never refreshed or written back:
//!   an expired one says so, and Claude Code renews it the next time it runs.
//! - Antigravity: `agy --print /usage` prints one tab-separated row per pool and window
//!   (`Gemini Models  Weekly Limit Remaining  99%  2026-01-09T00:00:00Z`). Cloud Code's quota
//!   endpoints report 100% left whatever is used, so the CLI is asked.
//!
//! Nothing leaves this computer but the account's own request to its own vendor; results are kept
//! a few minutes so the title bar and phones don't ask on every refresh.
use super::{AiKind, AiProvider};
use crate::error::{AppError, AppResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a result is reused.
const FRESH: Duration = Duration::from_secs(180);
/// A refresh the user asks for still reuses a result this new.
const MIN_REFRESH: Duration = Duration::from_secs(20);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const AGY_TIMEOUT: Duration = Duration::from_secs(45);

/// One limit: how much of it is used, and when it starts over.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// "5-hour", "Weekly", "Weekly · Opus", "Gemini Models · Weekly"…
    pub label: String,
    /// Used, 0–100.
    pub percent: f64,
    /// ISO 8601, when known.
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsage {
    pub provider_id: String,
    pub kind: AiKind,
    /// The plan, when the login says (Claude: "max", "pro"…).
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    /// The most used window, 0–100 (None: no windows).
    pub percent: Option<f64>,
    /// Why there are no numbers (not logged in, an API key, offline…).
    pub error: Option<String>,
    /// Unix ms the numbers were read.
    pub at: i64,
}

/// Whose usage a connection shows: several connections on one login share a result.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Account {
    /// A Claude Code config folder (None: the main one, as Claude Code in a terminal finds it).
    Claude(Option<PathBuf>),
    /// The `agy` program a connection runs.
    Antigravity(String),
}

#[derive(Debug, Clone)]
struct Read {
    plan: Option<String>,
    windows: Vec<UsageWindow>,
    error: Option<String>,
    at: i64,
}

static CACHE: Lazy<Mutex<HashMap<Account, (Instant, Read)>>> = Lazy::new(Default::default);

/// Whether a connection has plan usage to show (a login, not an API key).
pub fn shows_usage(p: &AiProvider) -> bool {
    matches!(p.kind, AiKind::Claude | AiKind::Antigravity) && !p.has_api_key && !p.env.iter().any(|e| p.key_env().as_deref() == Some(e.key.as_str()))
}

fn account_of(p: &AiProvider, dir: Option<PathBuf>) -> Option<Account> {
    match p.kind {
        AiKind::Claude => Some(Account::Claude(dir)),
        AiKind::Antigravity => Some(Account::Antigravity(p.command.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "agy".into()))),
        _ => None,
    }
}

/// The usage of each connection that has some (or of `provider_id` only).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_usage(provider_id: Option<String>, refresh: Option<bool>) -> AppResult<Vec<AiUsage>> {
    let st = super::store()?;
    let list: Vec<AiProvider> = st.list()?.into_iter().filter(|p| provider_id.as_ref().is_none_or(|id| *id == p.id)).filter(shows_usage).collect();
    let max_age = if refresh.unwrap_or(false) { MIN_REFRESH } else { FRESH };
    let mut out = Vec::new();
    for p in list {
        let Some(account) = account_of(&p, st.account_dir(&p)) else { continue };
        let r = read(&account, max_age).await;
        let percent = r.windows.iter().map(|w| w.percent).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))));
        out.push(AiUsage { provider_id: p.id.clone(), kind: p.kind, plan: r.plan, windows: r.windows, percent, error: r.error, at: r.at });
    }
    Ok(out)
}

async fn read(account: &Account, max_age: Duration) -> Read {
    if let Some((t, r)) = CACHE.lock().get(account)
        && t.elapsed() < max_age
    {
        return r.clone();
    }
    let at = chrono::Utc::now().timestamp_millis();
    let r = match account {
        Account::Claude(dir) => match claude(dir.as_deref()).await {
            Ok((plan, windows)) => Read { plan, windows, error: None, at },
            Err(e) => Read { plan: None, windows: vec![], error: Some(e.to_string()), at },
        },
        Account::Antigravity(program) => match antigravity(program).await {
            Ok(windows) => Read { plan: None, windows, error: None, at },
            Err(e) => Read { plan: None, windows: vec![], error: Some(e.to_string()), at },
        },
    };
    CACHE.lock().insert(account.clone(), (Instant::now(), r.clone()));
    r
}

// ---------------------------------------------------------------- Claude Code

/// The keychain entry Claude Code keeps a config folder's login in: its own name for
/// `CLAUDE_CONFIG_DIR` (the first 8 hex of the folder's sha-256), the plain one otherwise.
fn keychain_service(config_dir: Option<&str>) -> String {
    use sha2::Digest;
    match config_dir.filter(|d| !d.is_empty()) {
        Some(d) => format!("Claude Code-credentials-{}", &hex::encode(sha2::Sha256::digest(d.as_bytes()))[..8]),
        None => "Claude Code-credentials".into(),
    }
}

/// The OAuth part of Claude Code's credentials (`claudeAiOauth`).
async fn claude_credentials(dir: Option<&Path>) -> AppResult<serde_json::Value> {
    // the folder Claude Code is given: a connection's own, else whatever DevCat itself runs with
    let env_dir = std::env::var("CLAUDE_CONFIG_DIR").ok().filter(|v| !v.is_empty());
    let config = dir.map(|d| d.to_string_lossy().into_owned()).or(env_dir);
    let raw = if cfg!(target_os = "macos") {
        let mut cmd = tokio::process::Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", &keychain_service(config.as_deref())]);
        if let Ok(user) = std::env::var("USER") {
            cmd.args(["-a", &user]);
        }
        let out = tokio::time::timeout(HTTP_TIMEOUT, cmd.arg("-w").kill_on_drop(true).output())
            .await
            .map_err(|_| AppError::msg("The keychain didn't answer"))??;
        if !out.status.success() {
            return Err(AppError::msg("Not logged in to a Claude account (or it uses an API key)"));
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        let folder = config.map(PathBuf::from).unwrap_or_else(super::claude_account::main_dir);
        std::fs::read_to_string(folder.join(".credentials.json")).map_err(|_| AppError::msg("Not logged in to a Claude account (or it uses an API key)"))?
    };
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|_| AppError::msg("Couldn't read Claude Code's login"))?;
    v.get("claudeAiOauth").cloned().ok_or_else(|| AppError::msg("Not logged in to a Claude account (or it uses an API key)"))
}

async fn claude(dir: Option<&Path>) -> AppResult<(Option<String>, Vec<UsageWindow>)> {
    let oauth = claude_credentials(dir).await?;
    let token = oauth["accessToken"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| AppError::msg("Not logged in to a Claude account"))?;
    if oauth["expiresAt"].as_i64().is_some_and(|e| e < chrono::Utc::now().timestamp_millis()) {
        return Err(AppError::msg("The login has expired: start a chat with this connection and Claude Code renews it"));
    }
    let plan = oauth["subscriptionType"].as_str().map(str::to_string);
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().map_err(|e| AppError::msg(e.to_string()))?;
    let resp = http
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", concat!("devcat/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| AppError::msg(format!("Couldn't reach Anthropic: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::msg("Anthropic refused the login: start a chat with this connection to renew it"));
    }
    if !status.is_success() {
        return Err(AppError::msg(format!("Anthropic answered {status}")));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| AppError::msg(e.to_string()))?;
    Ok((plan, claude_windows(&v)))
}

/// The limits in an `/api/oauth/usage` answer: the session and weekly windows, then the weekly
/// limits of single models (`limits[].scope.model`).
fn claude_windows(v: &serde_json::Value) -> Vec<UsageWindow> {
    let mut out: Vec<UsageWindow> = Vec::new();
    let fixed = [("five_hour", "5-hour"), ("seven_day", "Weekly"), ("seven_day_opus", "Weekly · Opus"), ("seven_day_sonnet", "Weekly · Sonnet")];
    for (key, label) in fixed {
        let w = &v[key];
        if let Some(p) = w["utilization"].as_f64() {
            out.push(UsageWindow { label: label.into(), percent: p.clamp(0.0, 100.0), resets_at: w["resets_at"].as_str().map(str::to_string) });
        }
    }
    for l in v["limits"].as_array().into_iter().flatten() {
        let Some(model) = l.pointer("/scope/model/display_name").and_then(|m| m.as_str()).filter(|m| !m.is_empty()) else { continue };
        let Some(p) = l["percent"].as_f64() else { continue };
        let group = match l["group"].as_str() {
            Some("weekly") => "Weekly".to_string(),
            Some(g) => g.replace('_', " "),
            None => "Weekly".into(),
        };
        let label = format!("{group} · {model}");
        if !out.iter().any(|w| w.label == label) {
            out.push(UsageWindow { label, percent: p.clamp(0.0, 100.0), resets_at: l["resets_at"].as_str().map(str::to_string) });
        }
    }
    out
}

// ---------------------------------------------------------------- Antigravity

async fn antigravity(program: &str) -> AppResult<Vec<UsageWindow>> {
    let bin = crate::claude::locate::find_program(program).await.ok_or_else(|| AppError::msg(format!("{program} not found. Install it with: {}", AiKind::Antigravity.install_hint().unwrap_or_default())))?;
    let mut cmd = crate::claude::locate::tool_command(&bin.to_string_lossy()).await;
    cmd.args(["--print", "/usage"])
        .env("AGY_CLI_HIDE_LOGO", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(home) = dirs::home_dir() {
        cmd.current_dir(home);
    }
    let out = tokio::time::timeout(AGY_TIMEOUT, cmd.output()).await.map_err(|_| AppError::msg("agy /usage timed out"))??;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let windows = agy_windows(&stdout);
    if windows.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err.lines().chain(stdout.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("no usage printed (log in by running agy once)");
        return Err(AppError::msg(format!("agy: {}", why.chars().take(200).collect::<String>())));
    }
    Ok(windows)
}

/// Rows of `agy --print /usage`: pool, limit, percent (remaining, or used when the limit says so),
/// reset time; separated by tabs (or runs of 2+ spaces, in case a terminal turned them into spaces).
fn agy_windows(stdout: &str) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = strip_ansi(line);
        let cols: Vec<&str> = if line.contains('\t') { line.split('\t').map(str::trim).collect() } else { line.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect() };
        let Some(pi) = cols.iter().position(|c| c.ends_with('%') && c.trim_end_matches('%').trim().parse::<f64>().is_ok()) else { continue };
        if pi < 2 {
            continue;
        }
        let n: f64 = cols[pi].trim_end_matches('%').trim().parse().unwrap_or(0.0);
        let limit = cols[pi - 1];
        let lower = limit.to_lowercase();
        let used = if lower.contains("used") { n } else { 100.0 - n };
        let window = ["remaining", "left", "used"].iter().fold(limit.to_string(), |s, w| {
            let i = s.to_lowercase().rfind(w);
            i.map(|i| s[..i].trim_end().to_string()).unwrap_or(s)
        });
        let window = window.trim_end_matches(" Limit").trim_end_matches(" limit").trim().to_string();
        let pool = cols[..pi - 1].join(" ");
        let label = if window.is_empty() { pool } else { format!("{pool} · {window}") };
        out.push(UsageWindow { label, percent: used.clamp(0.0, 100.0), resets_at: cols.get(pi + 1).map(|s| s.to_string()).filter(|s| !s.is_empty()) });
    }
    out
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_keychain_names_follow_the_config_folder() {
        assert_eq!(keychain_service(None), "Claude Code-credentials");
        use sha2::Digest;
        let h = hex::encode(sha2::Sha256::digest(b"/tmp/x"));
        assert_eq!(keychain_service(Some("/tmp/x")), format!("Claude Code-credentials-{}", &h[..8]));
    }

    #[test]
    fn reads_claude_limits() {
        let v = json!({
            "five_hour": { "utilization": 12.0, "resets_at": "2026-11-19T10:00:00Z" },
            "seven_day": { "utilization": 58.0, "resets_at": "2026-11-24T12:00:00Z" },
            "seven_day_opus": null,
            "limits": [
                { "kind": "weekly_all", "group": "weekly", "percent": 58, "resets_at": "2026-11-24T12:00:00Z", "scope": null },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 4, "resets_at": "2026-11-24T12:00:00Z", "scope": { "model": { "display_name": "Fable" } } }
            ]
        });
        let w = claude_windows(&v);
        assert_eq!(w.iter().map(|w| (w.label.as_str(), w.percent)).collect::<Vec<_>>(), [("5-hour", 12.0), ("Weekly", 58.0), ("Weekly · Fable", 4.0)]);
        assert!(claude_windows(&json!({})).is_empty());
    }

    #[test]
    fn reads_agy_usage_rows() {
        let out = "Gemini Models\tWeekly Limit Remaining\t99%\t2026-01-09T00:00:00Z\n\
                   Gemini Models\t5-hour Limit Remaining\t40%\t2026-01-02T05:00:00Z\n\
                   \x1b[1mClaude and GPT models\x1b[0m\tWeekly Limit Remaining\t100%\t\n\
                   some banner\n";
        let w = agy_windows(out);
        assert_eq!(
            w.iter().map(|w| (w.label.as_str(), w.percent)).collect::<Vec<_>>(),
            [("Gemini Models · Weekly", 1.0), ("Gemini Models · 5-hour", 60.0), ("Claude and GPT models · Weekly", 0.0)]
        );
        assert_eq!(w[0].resets_at.as_deref(), Some("2026-01-09T00:00:00Z"));
        assert_eq!(w[2].resets_at, None);
        // spaces instead of tabs
        let w = agy_windows("Argon    Weekly Limit Remaining    75%    2026-01-09T00:00:00Z");
        assert_eq!((w[0].label.as_str(), w[0].percent), ("Argon · Weekly", 25.0));
        assert!(agy_windows("Usage: 50% of nothing").is_empty());
    }
}
