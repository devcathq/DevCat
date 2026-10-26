//! Claude workspace: sessions. Reads Claude Code's own transcripts in
//! `<config>/projects/<encoded cwd>/<session id>.jsonl` (config = `$CLAUDE_CONFIG_DIR`
//! or `~/.claude`). The dir name is the cwd with every non-alphanumeric char replaced by
//! `-`; long paths get truncated + hashed and different cwds can collide, so a dir's real
//! cwd is always taken from the `cwd` field inside its files.
use super::*;
use crate::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::de::IgnoredAny;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const TITLE_MAX: usize = 160;

fn config_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::home_dir().unwrap_or_default().join(".claude"),
    }
}

pub(crate) fn projects_root() -> PathBuf {
    config_dir().join("projects")
}

pub(crate) fn encode_cwd(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn rfc3339(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn read_lines(path: &Path) -> std::io::Result<impl Iterator<Item = String>> {
    let mut r = BufReader::with_capacity(256 * 1024, std::fs::File::open(path)?);
    let mut buf = Vec::new();
    Ok(std::iter::from_fn(move || {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
        }
    }))
}

fn session_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl")).collect()
}

// ---------------------------------------------------------------- project dir ↔ cwd

/// project dir → real cwd (first `cwd` field found in its files). Never changes for a dir.
static DIR_CWD: Lazy<Mutex<HashMap<PathBuf, String>>> = Lazy::new(Default::default);

#[derive(serde::Deserialize)]
struct CwdOnly {
    cwd: Option<String>,
}

pub(crate) fn dir_cwd(dir: &Path) -> Option<String> {
    if let Some(c) = DIR_CWD.lock().get(dir) {
        return Some(c.clone());
    }
    let mut files = session_files(dir);
    files.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
    let found = files.iter().find_map(|f| {
        read_lines(f).ok()?.take(200).find_map(|l| {
            if !l.contains("\"cwd\"") {
                return None;
            }
            serde_json::from_str::<CwdOnly>(&l).ok()?.cwd.filter(|c| !c.is_empty())
        })
    })?;
    DIR_CWD.lock().insert(dir.to_path_buf(), found.clone());
    Some(found)
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches(['/', '\\']).to_string();
    if cfg!(any(windows, target_os = "macos")) {
        norm(a).eq_ignore_ascii_case(&norm(b))
    } else {
        norm(a) == norm(b)
    }
}

/// Every project dir holding sessions for `cwd` (raw and symlink-resolved spellings,
/// e.g. /tmp vs /private/tmp, plus truncated/hashed names found via file contents).
fn project_dirs_for(root: &Path, cwd: &str) -> Vec<PathBuf> {
    let mut spellings = vec![cwd.to_string()];
    if let Ok(c) = std::fs::canonicalize(cwd) {
        let c = c.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        if !spellings.contains(&c) {
            spellings.push(c);
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    let encoded: Vec<String> = spellings.iter().map(|s| encode_cwd(s)).collect();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        // Cheap prefilter: exact name, or a name sharing a long prefix (truncated + hash).
        let maybe = encoded.iter().any(|enc| name == *enc || (enc.len() > 100 && name.get(..100).is_some_and(|n| enc.get(..100) == Some(n))));
        if !maybe {
            continue;
        }
        match dir_cwd(&dir) {
            Some(real) if spellings.iter().any(|s| same_path(s, &real)) => out.push(dir),
            None if encoded.contains(&name) => out.push(dir),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------- session meta

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(default)]
    is_sidechain: bool,
    #[serde(default)]
    is_meta: bool,
    git_branch: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    last_prompt: Option<String>,
    summary: Option<String>,
    #[serde(rename = "totalCostUSD")]
    total_cost_usd: Option<f64>,
    message: Option<IgnoredAny>,
}

/// Text of a user line if it's a real typed prompt (not meta / tool results / command wrappers).
fn prompt_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                return None;
            }
            let parts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            if parts.is_empty() {
                if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("image")) {
                    return Some("[image]".into());
                }
                return None;
            }
            parts.join("\n")
        }
        _ => return None,
    };
    let t = text.trim();
    const WRAPPERS: &[&str] = &[
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-memory-input>",
        "<system-reminder>",
        "[Request interrupted",
        "Caveat: The messages below",
    ];
    if t.is_empty() || WRAPPERS.iter().any(|w| t.starts_with(w)) {
        return None;
    }
    Some(t.to_string())
}

fn clip(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= TITLE_MAX {
        return one_line;
    }
    let mut t: String = one_line.chars().take(TITLE_MAX - 1).collect();
    t.push('…');
    t
}

/// Build the metadata of one session file. None when it has no user/assistant messages.
pub(super) fn parse_meta(path: &Path) -> Option<SessionMeta> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let (mut first_ts, mut last_ts) = (None::<String>, None::<String>);
    let (mut custom_title, mut ai_title, mut summary, mut last_prompt_line) = (None::<String>, None, None, None);
    let (mut first_prompt, mut last_prompt) = (None::<String>, None::<String>);
    let (mut count, mut branch, mut cost) = (0usize, None, None);
    for raw in read_lines(path).ok()? {
        let Ok(l) = serde_json::from_str::<Line>(&raw) else { continue };
        if let Some(ts) = l.timestamp.filter(|t| !t.is_empty()) {
            if first_ts.is_none() {
                first_ts = Some(ts.clone());
            }
            last_ts = Some(ts);
        }
        match l.kind.as_deref() {
            Some(k @ ("user" | "assistant")) if !l.is_sidechain && l.message.is_some() => {
                count += 1;
                if let Some(b) = l.git_branch.filter(|b| !b.is_empty() && b != "HEAD") {
                    branch = Some(b);
                }
                // Only user lines that might be prompts get a full parse (tool results are big).
                if k == "user" && !l.is_meta && !raw.contains("\"tool_result\"") {
                    if let Some(t) = serde_json::from_str::<Value>(&raw).ok().as_ref().and_then(prompt_text) {
                        if first_prompt.is_none() {
                            first_prompt = Some(t.clone());
                        }
                        last_prompt = Some(t);
                    }
                }
            }
            // a name given with /rename (or in DevCat); an empty one goes back to Claude's own title
            Some("custom-title") => custom_title = l.custom_title.map(|t| t.trim().to_string()).or(custom_title),
            Some("ai-title") => ai_title = l.ai_title.filter(|t| !t.trim().is_empty()).or(ai_title),
            Some("summary") => summary = l.summary.filter(|t| !t.trim().is_empty()).or(summary),
            Some("last-prompt") => last_prompt_line = l.last_prompt.filter(|t| !t.trim().is_empty()).or(last_prompt_line),
            Some("cost-state") => cost = l.total_cost_usd.or(cost),
            _ => {}
        }
    }
    if count == 0 {
        return None;
    }
    let md = path.metadata().ok();
    let created = first_ts.or_else(|| md.as_ref().and_then(|m| m.created().ok()).map(rfc3339));
    let updated = last_ts.or_else(|| md.as_ref().and_then(|m| m.modified().ok()).map(rfc3339));
    let title = custom_title
        .filter(|t| !t.is_empty())
        .or(ai_title)
        .or(summary)
        .or_else(|| first_prompt.clone())
        .map(|t| clip(&t))
        .unwrap_or_else(|| "Untitled session".into());
    Some(SessionMeta {
        id,
        title,
        first_prompt: first_prompt.map(|t| clip(&t)),
        last_prompt: last_prompt_line.or(last_prompt).map(|t| clip(&t)),
        created_at: created,
        updated_at: updated,
        message_count: count,
        git_branch: branch,
        cost_usd: cost,
    })
}

type CacheKey = (Option<SystemTime>, u64);
/// path → (mtime, size) → parsed meta (None = empty session).
static META_CACHE: Lazy<Mutex<HashMap<PathBuf, (CacheKey, Option<SessionMeta>)>>> = Lazy::new(Default::default);

fn cached_meta(path: &Path) -> Option<SessionMeta> {
    let md = path.metadata().ok()?;
    let key = (md.modified().ok(), md.len());
    if let Some((k, m)) = META_CACHE.lock().get(path) {
        if *k == key {
            return m.clone();
        }
    }
    let m = parse_meta(path);
    META_CACHE.lock().insert(path.to_path_buf(), (key, m.clone()));
    m
}

fn sessions_in(root: &Path, cwd: &str) -> Vec<SessionMeta> {
    let mut out: Vec<SessionMeta> =
        project_dirs_for(root, cwd).iter().flat_map(|d| session_files(d)).filter_map(|f| cached_meta(&f)).collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

fn projects_in(root: &Path) -> Vec<ClaudeProject> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut by_cwd: HashMap<String, ClaudeProject> = HashMap::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let files = session_files(&dir);
        if files.is_empty() {
            continue;
        }
        let Some(cwd) = dir_cwd(&dir) else { continue };
        let newest = files.iter().filter_map(|f| f.metadata().and_then(|m| m.modified()).ok()).max();
        let p = by_cwd.entry(cwd.clone()).or_insert(ClaudeProject { cwd, session_count: 0, last_active: None });
        p.session_count += files.len();
        if let Some(t) = newest.map(rfc3339) {
            if p.last_active.as_ref().is_none_or(|cur| *cur < t) {
                p.last_active = Some(t);
            }
        }
    }
    let mut v: Vec<ClaudeProject> = by_cwd.into_values().collect();
    v.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    v
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn find_session_file(root: &Path, cwd: &str, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    project_dirs_for(root, cwd).into_iter().map(|d| d.join(&name)).find(|p| p.is_file()).or_else(|| {
        // Session started elsewhere (e.g. moved/renamed dir): search every project.
        std::fs::read_dir(root).ok()?.flatten().map(|e| e.path().join(&name)).find(|p| p.is_file())
    })
}

/// Raw lines needed to render history: non-sidechain user/assistant/system + titles, in file order.
fn transcript_from(path: &Path) -> AppResult<Vec<Value>> {
    const KEEP: &[&str] = &["user", "assistant", "system", "ai-title", "summary"];
    let mut out = Vec::new();
    for raw in read_lines(path)? {
        let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        if KEEP.contains(&kind) && v.get("isSidechain").and_then(Value::as_bool) != Some(true) {
            out.push(v);
        }
    }
    Ok(out)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::msg(e.to_string()))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_projects() -> AppResult<Vec<ClaudeProject>> {
    blocking(|| projects_in(&projects_root())).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_sessions(cwd: String) -> AppResult<Vec<SessionMeta>> {
    blocking(move || sessions_in(&projects_root(), &cwd)).await
}

/// Transcript lines of a session (only user/assistant/system/result-like entries, raw JSON).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_transcript(cwd: String, session_id: String) -> AppResult<Vec<Value>> {
    if !valid_session_id(&session_id) {
        return Err(AppError::msg(format!("Invalid session id: {session_id}")));
    }
    blocking(move || {
        let path = find_session_file(&projects_root(), &cwd, &session_id)
            .ok_or_else(|| AppError::msg(format!("Session {session_id} not found")))?;
        transcript_from(&path)
    })
    .await?
}

/// Name a session as Claude Code's `/rename` does: a `custom-title` line at the end of its
/// transcript (the last one wins, and a running Claude Code takes it from there) plus the
/// `<session id>/custom-title.json` beside it that `claude --resume` reads. An empty name goes back
/// to Claude's own title. Returns the title the list shows now.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_rename(cwd: String, session_id: String, title: String) -> AppResult<String> {
    if !valid_session_id(&session_id) {
        return Err(AppError::msg(format!("Invalid session id: {session_id}")));
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    blocking(move || {
        let path = find_session_file(&projects_root(), &cwd, &session_id)
            .ok_or_else(|| AppError::msg(format!("Session {session_id} not found")))?;
        rename_in(&path, &session_id, &title)?;
        Ok(parse_meta(&path).map(|m| m.title).unwrap_or(title))
    })
    .await?
}

fn rename_in(path: &Path, session_id: &str, title: &str) -> AppResult<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).append(true).open(path)?;
    // a last line cut short (Claude Code stopped mid-write) must not swallow this one
    let len = f.metadata()?.len();
    let mut last = [b'\n'];
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
    }
    let entry = serde_json::json!({ "type": "custom-title", "customTitle": title, "sessionId": session_id });
    let line = format!("{}{entry}\n", if last[0] == b'\n' { "" } else { "\n" });
    f.write_all(line.as_bytes())?;

    let dir = path.with_file_name(session_id);
    let sidecar = dir.join("custom-title.json");
    if title.is_empty() {
        return match std::fs::remove_file(&sidecar) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&dir)?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&sidecar)?.write_all(serde_json::json!({ "customTitle": title }).to_string().as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_root() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-sessions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_jsonl(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn user(cwd: &str, ts: &str, content: Value) -> Value {
        serde_json::json!({"type":"user","cwd":cwd,"timestamp":ts,"isSidechain":false,"gitBranch":"main",
            "message":{"role":"user","content":content}})
    }

    fn assistant(cwd: &str, ts: &str) -> Value {
        serde_json::json!({"type":"assistant","cwd":cwd,"timestamp":ts,"isSidechain":false,"gitBranch":"feat/x",
            "message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}})
    }

    #[test]
    fn encodes_cwd() {
        assert_eq!(encode_cwd("/Users/a/Projects/Interopera/.pr-worktrees/x_y"), "-Users-a-Projects-Interopera--pr-worktrees-x-y");
        assert_eq!(encode_cwd(r"C:\Users\a b"), "C--Users-a-b");
    }

    #[test]
    fn meta_extraction() {
        let root = temp_root();
        let cwd = "/work/proj";
        let f = root.join(encode_cwd(cwd)).join("s1.jsonl");
        write_jsonl(
            &f,
            &[
                serde_json::json!({"type":"queue-operation","timestamp":"2026-01-01T00:00:00.000Z"}),
                user(cwd, "2026-01-01T00:00:01.000Z", Value::String("<local-command-caveat>Caveat</local-command-caveat>".into())),
                serde_json::json!({"type":"user","isMeta":true,"timestamp":"2026-01-01T00:00:01.500Z","message":{"role":"user","content":"meta stuff"}}),
                user(cwd, "2026-01-01T00:00:02.000Z", Value::String("<command-name>/effort</command-name>".into())),
                user(cwd, "2026-01-01T00:00:03.000Z", serde_json::json!([{"type":"text","text":"  Fix the\n login bug  "}])),
                assistant(cwd, "2026-01-01T00:00:04.000Z"),
                user(cwd, "2026-01-01T00:00:05.000Z", serde_json::json!([{"type":"tool_result","tool_use_id":"t","content":"x"}])),
                serde_json::json!({"type":"assistant","isSidechain":true,"timestamp":"2026-01-01T00:00:06.000Z","message":{"role":"assistant","content":[]}}),
                user(cwd, "2026-01-01T00:00:07.000Z", Value::String("and add tests".into())),
                serde_json::json!({"type":"cost-state","totalCostUSD":0.5}),
                serde_json::json!({"type":"cost-state","totalCostUSD":1.25}),
                Value::String("garbage".into()),
            ],
        );
        std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"{not json\n").unwrap();
        let m = parse_meta(&f).unwrap();
        assert_eq!(m.id, "s1");
        assert_eq!(m.title, "Fix the login bug");
        assert_eq!(m.first_prompt.as_deref(), Some("Fix the login bug"));
        assert_eq!(m.last_prompt.as_deref(), Some("and add tests"));
        assert_eq!(m.message_count, 7); // every non-sidechain user/assistant line
        assert_eq!(m.created_at.as_deref(), Some("2026-01-01T00:00:00.000Z"));
        assert_eq!(m.updated_at.as_deref(), Some("2026-01-01T00:00:07.000Z"));
        assert_eq!(m.git_branch.as_deref(), Some("main"));
        assert_eq!(m.cost_usd, Some(1.25));

        // ai-title and last-prompt lines win
        let mut lines: Vec<Value> = vec![user(cwd, "2026-01-02T00:00:00.000Z", Value::String("first".into()))];
        lines.push(serde_json::json!({"type":"ai-title","aiTitle":"Old title"}));
        lines.push(serde_json::json!({"type":"ai-title","aiTitle":"Login bug fix"}));
        lines.push(serde_json::json!({"type":"last-prompt","lastPrompt":"latest"}));
        let f2 = root.join(encode_cwd(cwd)).join("s2.jsonl");
        write_jsonl(&f2, &lines);
        let m2 = parse_meta(&f2).unwrap();
        assert_eq!(m2.title, "Login bug fix");
        assert_eq!(m2.last_prompt.as_deref(), Some("latest"));
        assert_eq!(m2.cost_usd, None);

        // a name (Claude Code's /rename, or DevCat's) beats the ai-title; an empty one undoes it
        std::fs::OpenOptions::new().append(true).open(&f2).unwrap().write_all(b"{\"type\":\"user\",\"cut sh").unwrap();
        rename_in(&f2, "s2", "Checkout \"staging\"").unwrap();
        assert_eq!(parse_meta(&f2).unwrap().title, "Checkout \"staging\"");
        let tail = std::fs::read_to_string(&f2).unwrap();
        assert!(tail.ends_with("\n{\"type\":\"custom-title\",\"customTitle\":\"Checkout \\\"staging\\\"\",\"sessionId\":\"s2\"}\n"), "{tail}");
        let sidecar = root.join(encode_cwd(cwd)).join("s2").join("custom-title.json");
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "{\"customTitle\":\"Checkout \\\"staging\\\"\"}");
        rename_in(&f2, "s2", "").unwrap();
        assert_eq!(parse_meta(&f2).unwrap().title, "Login bug fix");
        assert!(!sidecar.exists());

        // only non-message lines → skipped
        let f3 = root.join(encode_cwd(cwd)).join("s3.jsonl");
        write_jsonl(&f3, &[serde_json::json!({"type":"queue-operation","timestamp":"2026-01-03T00:00:00.000Z"})]);
        assert!(parse_meta(&f3).is_none());

        let list = sessions_in(&root, cwd);
        assert_eq!(list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["s2", "s1"]);

        let t = transcript_from(&f).unwrap();
        assert_eq!(t.len(), 7);
        assert!(t.iter().all(|v| v["isSidechain"] != true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn long_or_hashed_dirs_match_by_content() {
        let root = temp_root();
        let cwd = format!("/very/long/{}", "segment/".repeat(30));
        let cwd = cwd.trim_end_matches('/');
        let enc = encode_cwd(cwd);
        let hashed = format!("{}-abc123", &enc[..200]);
        write_jsonl(&root.join(&hashed).join("h1.jsonl"), &[user(cwd, "2026-01-01T00:00:00.000Z", Value::String("hi".into()))]);
        // A colliding dir for a different cwd must not match.
        let other = format!("{cwd}-x");
        write_jsonl(
            &root.join(format!("{}-zzz", &enc[..200])).join("h2.jsonl"),
            &[user(&other, "2026-01-01T00:00:00.000Z", Value::String("hi".into()))],
        );
        let list = sessions_in(&root, cwd);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "h1");
        assert!(find_session_file(&root, cwd, "h2").is_some(), "falls back to a global search");

        let projects = projects_in(&root);
        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|p| p.cwd == cwd && p.session_count == 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_id_validation() {
        assert!(valid_session_id("7315f7d9-b9d5-4211-bd01-69132d379722"));
        assert!(!valid_session_id("../etc/passwd"));
        assert!(!valid_session_id(""));
    }

    /// Smoke test against the real ~/.claude (read-only); prints timings.
    #[test]
    #[ignore]
    fn real_projects_dir() {
        let t = std::time::Instant::now();
        let projects = projects_in(&projects_root());
        println!("{} projects in {:?}", projects.len(), t.elapsed());
        for p in projects.iter().take(3) {
            let t = std::time::Instant::now();
            let s = sessions_in(&projects_root(), &p.cwd);
            println!("{} → {} sessions in {:?}; first: {:?}", p.cwd, s.len(), t.elapsed(), s.first().map(|m| &m.title));
            let t = std::time::Instant::now();
            let _ = sessions_in(&projects_root(), &p.cwd);
            println!("  cached: {:?}", t.elapsed());
        }
    }
}
