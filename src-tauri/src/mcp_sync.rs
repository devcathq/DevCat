//! Sync MCP: writes DevCat's MCP server (`devcat mcp`, see `mcp_server.rs`) into the coding
//! agents' configs, so Claude Code, Codex, Cursor and Gemini find the user's connections without
//! setting anything up.
//!
//! - Claude Code (`~/.claude.json`): one entry for every folder (`mcpServers.devcat`), and one per
//!   code folder of a project (`projects["<folder>"].mcpServers.devcat`, Claude's private "local"
//!   scope: nothing goes into the repository, and it wins over the shared entry).
//! - Codex (`~/.codex/config.toml`), Cursor (`~/.cursor/mcp.json`), Gemini (`~/.gemini/settings.json`):
//!   one entry; the server picks the project from the folder the agent works in.
//!
//! An existing DevCat entry (named `devcat`, or any `devcat mcp` command) is replaced, never
//! duplicated, and entries for folders no project has any more are removed. Other servers are
//! left alone, except look-alikes the user ticks: servers pointing at one of their connections
//! (say `server-postgres postgres://localhost:5432/shop`), which DevCat replaces.
//! Each file is backed up once (`<file>.devcat-bak`) and written atomically.
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind, Project};
use crate::state::St;
use devcat_harness::mcp::is_devcat_server;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The servers' name in every config.
pub const NAME: &str = "devcat";

/// What a sync does (preview) or did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub changes: Vec<Change>,
    pub lookalikes: Vec<Lookalike>,
    /// Agents found on this computer.
    pub agents: Vec<String>,
    pub notes: Vec<String>,
    /// Synced before: DevCat keeps the configs up to date as projects change.
    pub enabled: bool,
    /// The user said "not now" to setting it up.
    pub dismissed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub agent: String,
    pub file: String,
    /// "Every folder", or the project and its folder.
    pub scope: String,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Add,
    Update,
    Remove,
    Unchanged,
}

/// Another MCP server that reaches one of the user's connections.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lookalike {
    /// Pass back to [`mcp_sync_apply`] to replace it.
    pub id: String,
    pub agent: String,
    pub file: String,
    pub scope: String,
    /// The server's name in that config.
    pub server: String,
    pub connection: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncState {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    dismissed: bool,
}

static DATA_DIR: OnceCell<PathBuf> = OnceCell::new();
/// Where the agents' configs are looked for, if not the user's home (the dev bridge).
static HOME: OnceCell<PathBuf> = OnceCell::new();
/// One sync at a time (auto syncs after project changes can overlap with the button).
static LOCK: Mutex<()> = Mutex::new(());

pub fn init(data_dir: PathBuf) {
    let _ = DATA_DIR.set(data_dir);
}

/// Look for the agents' configs here instead of the home folder.
pub fn set_home(dir: PathBuf) {
    let _ = HOME.set(dir);
}

/// The folder the agents' configs live under (the home folder, or the dev bridge's).
pub(crate) fn home() -> Option<PathBuf> {
    HOME.get().cloned().or_else(dirs::home_dir)
}

fn state_path() -> AppResult<PathBuf> {
    DATA_DIR.get().map(|d| d.join("mcp-sync.json")).ok_or_else(|| AppError::msg("DevCat isn't ready yet"))
}

fn load_state() -> SyncState {
    state_path().ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(s: &SyncState) -> AppResult<()> {
    std::fs::write(state_path()?, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

/// Everything a sync reads.
struct Env {
    home: PathBuf,
    /// The `devcat` program the agents start.
    exe: String,
    /// `--data-dir`, when not the app's default.
    data_dir: Option<String>,
    projects: Vec<Project>,
    connections: Vec<ConnectionConfig>,
}

impl Env {
    fn current() -> AppResult<Env> {
        let st = St::try_get().ok_or_else(|| AppError::msg("DevCat isn't ready yet"))?;
        let home = HOME.get().cloned().or_else(dirs::home_dir).ok_or_else(|| AppError::msg("Can't find your home folder"))?;
        let exe = std::env::current_exe()?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe).to_string_lossy().to_string();
        let data_dir = DATA_DIR.get().filter(|d| Some(d.as_path()) != crate::cli::default_data_dir().as_deref()).map(|d| d.to_string_lossy().to_string());
        Ok(Env { home, exe, data_dir, projects: st.projects.list()?, connections: st.store.load_all()? })
    }

    fn args(&self, project: Option<&str>, extra: &[&str]) -> Vec<String> {
        let mut a = vec!["mcp".to_string()];
        if let Some(id) = project {
            a.extend(["--project".to_string(), id.to_string()]);
        }
        a.extend(extra.iter().map(|s| s.to_string()));
        if let Some(d) = &self.data_dir {
            a.extend(["--data-dir".to_string(), d.clone()]);
        }
        a
    }

    /// Code folders with their project (projects without folders have none).
    fn folders(&self) -> Vec<(&Project, &str)> {
        self.projects.iter().flat_map(|p| p.folders.iter().map(move |f| (p, f.as_str()))).collect()
    }

    fn shown(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.to_string_lossy().replace('\\', "/")),
            Err(_) => p.to_string_lossy().to_string(),
        }
    }
}

const EVERY_FOLDER: &str = "Every folder";

fn project_scope(p: &Project, folder: &str) -> String {
    format!("{} · {folder}", p.name)
}

/// What Sync MCP would change, and the look-alike servers it could replace.
pub fn preview() -> AppResult<Plan> {
    let _g = LOCK.lock();
    let mut plan = run(&Env::current()?, false, &[])?;
    let s = load_state();
    (plan.enabled, plan.dismissed) = (s.enabled, s.dismissed);
    Ok(plan)
}

/// Sync, replacing the look-alikes in `replace` (their ids); from now on DevCat keeps the configs
/// up to date as projects change.
pub fn apply(replace: &[String]) -> AppResult<Plan> {
    let _g = LOCK.lock();
    let mut plan = run(&Env::current()?, true, replace)?;
    let s = SyncState { enabled: true, dismissed: false };
    save_state(&s)?;
    (plan.enabled, plan.dismissed) = (s.enabled, s.dismissed);
    Ok(plan)
}

/// After project folders change, or at startup (DevCat may have moved): keep the configs right,
/// once the user has synced. Look-alikes are only replaced from the dialog.
pub fn auto() {
    // Dev builds: never by themselves. On the installed DevCat's data (or a copy) they'd point every
    // agent's DevCat entry at target/debug/devcat; syncing from the dialog still does.
    if cfg!(debug_assertions) || !load_state().enabled {
        return;
    }
    let _g = LOCK.lock();
    if let Err(e) = Env::current().and_then(|env| run(&env, true, &[])) {
        eprintln!("MCP sync failed: {e}");
    }
}

fn run(env: &Env, apply: bool, replace: &[String]) -> AppResult<Plan> {
    let mut plan = Plan::default();
    claude(env, apply, replace, &mut plan)?;
    servers_json(env, "Cursor", &env.home.join(".cursor"), "mcp.json", &["--folder", "${workspaceFolder}"], apply, replace, &mut plan)?;
    servers_json(env, "Gemini CLI", &env.home.join(".gemini"), "settings.json", &[], apply, replace, &mut plan)?;
    codex(env, apply, replace, &mut plan)?;
    repo_lookalikes(env, apply, replace, &mut plan)?;
    if plan.agents.is_empty() {
        plan.notes.push("No Claude Code, Codex, Cursor or Gemini CLI found on this computer (DevCat looks for their settings in your home folder; run the agent once first).".into());
    }
    Ok(plan)
}

// ---------- Claude Code

fn claude(env: &Env, apply: bool, replace: &[String], plan: &mut Plan) -> AppResult<()> {
    let agent = "Claude Code";
    let path = env.home.join(".claude.json");
    if !path.exists() {
        if env.home.join(".claude").is_dir() {
            plan.notes.push("Claude Code: start it once so it creates ~/.claude.json, then sync again.".into());
        }
        return Ok(());
    }
    plan.agents.push(agent.into());
    let file = env.shown(&path);
    let mut doc = read_json(&path)?;
    let Some(root) = doc.as_object_mut() else { return Err(AppError::msg(format!("{file} isn't a JSON object"))) };
    let mut changed = false;

    let want = json!({ "type": "stdio", "command": env.exe, "args": env.args(None, &[]), "env": {} });
    let action = set_server(servers_mut(root), Some(&want));
    changed |= note(plan, agent, &file, EVERY_FOLDER, action);

    let folders = env.folders();
    let projects = root.entry("projects").or_insert_with(|| json!({}));
    if let Some(projects) = projects.as_object_mut() {
        for (p, folder) in &folders {
            let key = projects.keys().find(|k| same_folder(k, folder)).cloned().unwrap_or_else(|| claude_key(folder));
            let entry = projects.entry(key).or_insert_with(new_claude_project);
            let Some(entry) = entry.as_object_mut() else { continue };
            let want = json!({ "type": "stdio", "command": env.exe, "args": env.args(Some(&p.id), &[]), "env": {} });
            let action = set_server(servers_mut(entry), Some(&want));
            changed |= note(plan, agent, &file, &project_scope(p, folder), action);
        }
        // folders no project has any more: DevCat's entry goes, the rest of Claude's settings stay
        for (key, entry) in projects.iter_mut() {
            if folders.iter().any(|(_, f)| same_folder(key, f)) {
                continue;
            }
            if let Some(servers) = entry.get_mut("mcpServers").and_then(Value::as_object_mut) {
                let action = set_server(servers, None);
                changed |= note(plan, agent, &file, key, action);
            }
        }
        for (key, entry) in projects.iter_mut() {
            let scope = folders.iter().find(|(_, f)| same_folder(key, f)).map(|(p, f)| project_scope(p, f)).unwrap_or_else(|| key.clone());
            if let Some(servers) = entry.get_mut("mcpServers").and_then(Value::as_object_mut) {
                changed |= lookalikes(env, servers, agent, &file, &format!("projects/{key}"), &scope, replace, plan);
            }
        }
    }
    changed |= lookalikes(env, servers_mut(root), agent, &file, "", EVERY_FOLDER, replace, plan);
    if apply && changed {
        write_json(&path, &doc)?;
    }
    Ok(())
}

/// The settings Claude Code gives a folder it hasn't seen (`claude mcp add` writes the same).
fn new_claude_project() -> Value {
    json!({
        "allowedTools": [],
        "mcpContextUris": [],
        "mcpServers": {},
        "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [],
        "hasTrustDialogAccepted": false,
        "projectOnboardingSeenCount": 0,
        "hasClaudeMdExternalIncludesApproved": false,
        "hasClaudeMdExternalIncludesWarningShown": false
    })
}

/// How Claude Code keys a folder (`/` separators, also on Windows).
fn claude_key(folder: &str) -> String {
    if cfg!(windows) { folder.replace('\\', "/") } else { folder.to_string() }
}

fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().replace('\\', "/");
        let s = if s.len() > 1 { s.trim_end_matches('/').to_string() } else { s };
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

// ---------- Cursor, Gemini: `{ "mcpServers": { … } }`

#[allow(clippy::too_many_arguments)]
fn servers_json(env: &Env, agent: &str, dir: &Path, name: &str, extra: &[&str], apply: bool, replace: &[String], plan: &mut Plan) -> AppResult<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    plan.agents.push(agent.into());
    let path = dir.join(name);
    let file = env.shown(&path);
    let mut doc = if path.exists() { read_json(&path)? } else { json!({}) };
    let Some(root) = doc.as_object_mut() else { return Err(AppError::msg(format!("{file} isn't a JSON object"))) };
    let want = json!({ "command": env.exe, "args": env.args(None, extra) });
    let action = set_server(servers_mut(root), Some(&want));
    let mut changed = note(plan, agent, &file, EVERY_FOLDER, action);
    changed |= lookalikes(env, servers_mut(root), agent, &file, "", EVERY_FOLDER, replace, plan);
    if apply && changed {
        write_json(&path, &doc)?;
    }
    Ok(())
}

/// Look-alikes in the projects' own folders (`.mcp.json`, `.cursor/mcp.json`); DevCat writes
/// nothing else there.
fn repo_lookalikes(env: &Env, apply: bool, replace: &[String], plan: &mut Plan) -> AppResult<()> {
    for (p, folder) in env.folders() {
        for (agent, rel) in [("Claude Code", ".mcp.json"), ("Cursor", ".cursor/mcp.json")] {
            let path = Path::new(folder).join(rel);
            if !path.exists() {
                continue;
            }
            let Ok(mut doc) = read_json(&path) else { continue };
            let file = env.shown(&path);
            let Some(servers) = doc.get_mut("mcpServers").and_then(Value::as_object_mut) else { continue };
            if lookalikes(env, servers, agent, &file, "", &project_scope(p, folder), replace, plan) && apply {
                write_json(&path, &doc)?;
            }
        }
    }
    Ok(())
}

fn servers_mut(obj: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let v = obj.entry("mcpServers").or_insert_with(|| json!({}));
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().expect("just made an object")
}

/// Whether `cfg` (one entry of `mcpServers`) is DevCat's.
fn is_ours(name: &str, cfg: &Value) -> bool {
    let command = cfg.get("command").and_then(Value::as_str).unwrap_or("");
    let args: Vec<String> = cfg.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
    name == NAME || is_devcat_server(command, &args)
}

/// Put DevCat's entry (`want`) in a `mcpServers` map, replacing the one there, or remove it
/// (`None`). `None` back: nothing to say (no entry to remove).
fn set_server(servers: &mut Map<String, Value>, want: Option<&Value>) -> Option<Action> {
    let ours: Vec<String> = servers.iter().filter(|(k, v)| is_ours(k, v)).map(|(k, _)| k.clone()).collect();
    let Some(want) = want else {
        for k in &ours {
            servers.shift_remove(k);
        }
        return (!ours.is_empty()).then_some(Action::Remove);
    };
    if ours == [NAME] && servers.get(NAME) == Some(want) {
        return Some(Action::Unchanged);
    }
    for k in ours.iter().filter(|k| *k != NAME) {
        servers.shift_remove(k);
    }
    let action = if ours.is_empty() { Action::Add } else { Action::Update };
    servers.insert(NAME.into(), want.clone());
    Some(action)
}

/// Report a change; whether the file changes.
fn note(plan: &mut Plan, agent: &str, file: &str, scope: &str, action: Option<Action>) -> bool {
    let Some(action) = action else { return false };
    plan.changes.push(Change { agent: agent.into(), file: file.into(), scope: scope.into(), action });
    action != Action::Unchanged
}

/// List the look-alikes in `servers`, removing those in `replace`; whether any was removed.
#[allow(clippy::too_many_arguments)]
fn lookalikes(env: &Env, servers: &mut Map<String, Value>, agent: &str, file: &str, section: &str, scope: &str, replace: &[String], plan: &mut Plan) -> bool {
    let mut removed = false;
    let found: Vec<(String, String)> =
        servers.iter().filter(|(k, v)| !is_ours(k, v)).filter_map(|(k, v)| matching_connection(env, &v.to_string()).map(|c| (k.clone(), c.name.clone()))).collect();
    for (server, connection) in found {
        let id = format!("{file}\n{section}\n{server}");
        if replace.contains(&id) {
            servers.shift_remove(&server);
            removed = true;
        }
        plan.lookalikes.push(Lookalike { id, agent: agent.into(), file: file.into(), scope: scope.into(), server, connection });
    }
    removed
}

// ---------- Codex: `[mcp_servers.devcat]` in config.toml

fn codex(env: &Env, apply: bool, replace: &[String], plan: &mut Plan) -> AppResult<()> {
    use toml_edit::{Array, DocumentMut, Item, Table, value};
    let agent = "Codex";
    let dir = env.home.join(".codex");
    if !dir.is_dir() {
        return Ok(());
    }
    plan.agents.push(agent.into());
    let path = dir.join("config.toml");
    let file = env.shown(&path);
    let text = if path.exists() { std::fs::read_to_string(&path)? } else { String::new() };
    let mut doc: DocumentMut = text.parse().map_err(|e| AppError::msg(format!("{file}: {e}")))?;
    if !doc.contains_key("mcp_servers") {
        let mut t = Table::new();
        t.set_implicit(true);
        doc.insert("mcp_servers", Item::Table(t));
    }
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_like_mut) else {
        return Err(AppError::msg(format!("{file}: mcp_servers isn't a table")));
    };
    let entry = |t: &dyn toml_edit::TableLike| -> (String, Vec<String>) {
        let command = t.get("command").and_then(Item::as_str).unwrap_or("").to_string();
        let args = t.get("args").and_then(Item::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        (command, args)
    };
    let args = env.args(None, &[]);
    let ours: Vec<String> = servers
        .iter()
        .filter(|(k, v)| *k == NAME || v.as_table_like().is_some_and(|t| { let (c, a) = entry(t); is_devcat_server(&c, &a) }))
        .map(|(k, _)| k.to_string())
        .collect();
    let same = ours == [NAME] && servers.get(NAME).and_then(Item::as_table_like).is_some_and(|t| entry(t) == (env.exe.clone(), args.clone()));
    let action = if same { Action::Unchanged } else if ours.is_empty() { Action::Add } else { Action::Update };
    let mut changed = note(plan, agent, &file, EVERY_FOLDER, Some(action));
    if !same {
        for k in ours.iter().filter(|k| *k != NAME) {
            servers.remove(k);
        }
        let mut arr = Array::new();
        args.iter().for_each(|a| arr.push(a.as_str()));
        match servers.get_mut(NAME).and_then(Item::as_table_like_mut) {
            Some(t) => {
                t.insert("command", value(env.exe.as_str()));
                t.insert("args", value(arr));
            }
            None => {
                let mut t = Table::new();
                t.insert("command", value(env.exe.as_str()));
                t.insert("args", value(arr));
                servers.insert(NAME, Item::Table(t));
            }
        }
    }
    // look-alikes: their command, args, env and url as text
    let found: Vec<(String, String)> = servers
        .iter()
        .filter(|(k, _)| *k != NAME)
        .filter_map(|(k, v)| matching_connection(env, &v.to_string()).map(|c| (k.to_string(), c.name.clone())))
        .collect();
    for (server, connection) in found {
        let id = format!("{file}\n\n{server}");
        if replace.contains(&id) {
            servers.remove(&server);
            changed = true;
        }
        plan.lookalikes.push(Lookalike { id, agent: agent.into(), file: file.clone(), scope: EVERY_FOLDER.into(), server, connection });
    }
    if apply && changed {
        write_file(&path, doc.to_string().as_bytes())?;
    }
    Ok(())
}

// ---------- look-alike servers

/// The connection a server's config (as text) points at: it names the database kind and the
/// connection's address (host:port, the URL's host, the SQLite file).
fn matching_connection<'a>(env: &'a Env, config: &str) -> Option<&'a ConnectionConfig> {
    let hay = config.to_lowercase();
    env.connections.iter().find(|c| {
        let kind = kind_words(c.kind);
        !kind.is_empty() && kind.iter().any(|w| hay.contains(w)) && addresses(c).iter().any(|a| hay.contains(a.as_str()))
    })
}

fn kind_words(k: ConnectionKind) -> &'static [&'static str] {
    match k {
        ConnectionKind::Postgres => &["postgres", "pg"],
        ConnectionKind::Mysql => &["mysql", "mariadb"],
        ConnectionKind::Sqlite => &["sqlite"],
        ConnectionKind::Mssql => &["mssql", "sqlserver", "sql-server"],
        ConnectionKind::Mongodb => &["mongo"],
        ConnectionKind::Redis => &["redis", "valkey"],
        _ => &[],
    }
}

fn default_port(k: ConnectionKind) -> Option<u16> {
    match k {
        ConnectionKind::Postgres => Some(5432),
        ConnectionKind::Mysql => Some(3306),
        ConnectionKind::Mssql => Some(1433),
        ConnectionKind::Mongodb => Some(27017),
        ConnectionKind::Redis => Some(6379),
        _ => None,
    }
}

/// Text that identifies where a connection goes (lowercase).
fn addresses(c: &ConnectionConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(url) = c.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        // scheme://user:pass@host:port/db?… → host:port
        let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        let host = rest.split(['/', '?']).next().unwrap_or("");
        if !host.is_empty() {
            out.push(host.to_lowercase());
        }
    }
    let host = c.host.trim().to_lowercase();
    if let Some(port) = c.port.or(default_port(c.kind)).filter(|_| !host.is_empty()) {
        out.push(format!("{host}:{port}"));
        if host == "localhost" {
            out.push(format!("127.0.0.1:{port}"));
        }
    }
    if let Some(f) = c.file_path.as_deref().map(str::trim).filter(|f| c.kind == ConnectionKind::Sqlite && !f.is_empty()) {
        out.push(f.to_lowercase());
    }
    out
}

// ---------- files

fn read_json(path: &Path) -> AppResult<Value> {
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| AppError::msg(format!("{} isn't valid JSON ({e}); DevCat leaves it alone.", path.display())))
}

fn write_json(path: &Path, v: &Value) -> AppResult<()> {
    let mut text = serde_json::to_string_pretty(v)?;
    text.push('\n');
    write_file(path, text.as_bytes())
}

/// Back the file up once, then replace it in one step (keeping its permissions).
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> AppResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if path.exists() {
        let backup = path.with_file_name(format!("{name}.devcat-bak"));
        if !backup.exists() {
            std::fs::copy(path, &backup)?;
        }
    }
    let tmp = path.with_file_name(format!("{name}.devcat-tmp"));
    std::fs::write(&tmp, bytes)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------- commands

/// What Sync MCP would change (nothing is written).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn mcp_sync_preview() -> AppResult<Plan> {
    tokio::task::spawn_blocking(preview).await.map_err(|e| AppError::msg(e.to_string()))?
}

/// Sync now, replacing the ticked look-alikes.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn mcp_sync_apply(replace: Vec<String>) -> AppResult<Plan> {
    tokio::task::spawn_blocking(move || apply(&replace)).await.map_err(|e| AppError::msg(e.to_string()))?
}

/// "Not now" to setting it up (the app stops suggesting it).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn mcp_sync_dismiss() -> AppResult<()> {
    let mut s = load_state();
    s.dismissed = true;
    save_state(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-mcp-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn conn(v: Value) -> ConnectionConfig {
        serde_json::from_value(v).unwrap()
    }

    fn env(home: &Path, folder: &Path) -> Env {
        Env {
            home: home.to_path_buf(),
            exe: "/Applications/DevCat.app/Contents/MacOS/devcat".into(),
            data_dir: None,
            projects: vec![Project { id: "p1".into(), name: "Shop".into(), folders: vec![folder.to_string_lossy().to_string()], ..Default::default() }],
            connections: vec![conn(json!({ "id": "c1", "name": "shop-db", "kind": "postgres", "host": "localhost", "port": 5432, "projectId": "p1" }))],
        }
    }

    #[test]
    fn claude_gets_one_entry_per_scope_and_keeps_the_rest() {
        let (home, folder) = (tmp(), tmp());
        let gone = "/old/folder";
        let claude = json!({
            "numStartups": 3,
            "mcpServers": {
                "fs": { "command": "npx", "args": ["-y", "fs"] },
                "devcat-db": { "command": "/old/place/devcat", "args": ["mcp"] },
                "pg": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-postgres", "postgres://me:pw@localhost:5432/shop"] }
            },
            "projects": { gone: { "allowedTools": ["Bash"], "mcpServers": { "devcat": { "command": "devcat", "args": ["mcp", "--project", "x"] } } } }
        });
        std::fs::write(home.join(".claude.json"), serde_json::to_string(&claude).unwrap()).unwrap();
        let env = env(&home, &folder);

        let plan = run(&env, false, &[]).unwrap();
        let actions: Vec<(&str, Action)> = plan.changes.iter().map(|c| (c.scope.as_str(), c.action)).collect();
        let shop = format!("Shop · {}", folder.display());
        assert_eq!(actions, vec![(EVERY_FOLDER, Action::Update), (shop.as_str(), Action::Add), (gone, Action::Remove)]);
        assert_eq!(plan.lookalikes.len(), 1);
        assert_eq!((plan.lookalikes[0].server.as_str(), plan.lookalikes[0].connection.as_str()), ("pg", "shop-db"));
        let untouched = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert_eq!(untouched, serde_json::to_string(&claude).unwrap(), "a preview writes nothing");

        let id = plan.lookalikes[0].id.clone();
        run(&env, true, &[id]).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        let keys: Vec<&String> = v["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["fs", "devcat"], "the old DevCat entry is replaced, the look-alike removed, the rest kept in order");
        assert_eq!(v["mcpServers"]["devcat"]["args"], json!(["mcp"]));
        assert_eq!(v["numStartups"], 3);
        let key = folder.to_string_lossy().to_string();
        assert_eq!(v["projects"][&key]["mcpServers"]["devcat"]["args"], json!(["mcp", "--project", "p1"]));
        assert_eq!(v["projects"][&key]["allowedTools"], json!([]));
        assert_eq!(v["projects"][gone]["mcpServers"], json!({}));
        assert_eq!(v["projects"][gone]["allowedTools"], json!(["Bash"]), "Claude's own settings for the folder stay");
        assert!(home.join(".claude.json.devcat-bak").exists());

        let again = run(&env, true, &[]).unwrap();
        assert!(again.changes.iter().all(|c| c.action == Action::Unchanged), "syncing twice changes nothing: {:?}", again.changes);
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(folder));
    }

    #[test]
    fn codex_cursor_and_gemini_get_the_folder_following_entry() {
        let (home, folder) = (tmp(), tmp());
        for d in [".codex", ".cursor", ".gemini"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join(".codex/config.toml"), "# mine\nmodel = \"o4\"\n\n[mcp_servers.redis]\ncommand = \"uvx\"\nargs = [\"mcp-server-redis\", \"--url\", \"redis://127.0.0.1:6379\"]\n").unwrap();
        std::fs::write(home.join(".gemini/settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let mut env = env(&home, &folder);
        env.connections.push(conn(json!({ "id": "c2", "name": "cache", "kind": "redis", "host": "localhost" })));

        let plan = run(&env, true, &[]).unwrap();
        assert_eq!(plan.agents, ["Cursor", "Gemini CLI", "Codex"]);
        let toml = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(toml.starts_with("# mine\nmodel = \"o4\""), "comments and settings stay: {toml}");
        assert!(toml.contains("[mcp_servers.devcat]\ncommand = \"/Applications/DevCat.app/Contents/MacOS/devcat\"\nargs = [\"mcp\"]"), "{toml}");
        assert_eq!(plan.lookalikes.iter().map(|l| (l.agent.as_str(), l.server.as_str(), l.connection.as_str())).collect::<Vec<_>>(), [("Codex", "redis", "cache")]);
        let cursor: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert_eq!(cursor["mcpServers"]["devcat"]["args"], json!(["mcp", "--folder", "${workspaceFolder}"]));
        let gemini: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap()).unwrap();
        assert_eq!((gemini["theme"].as_str(), gemini["mcpServers"]["devcat"]["args"].clone()), (Some("dark"), json!(["mcp"])));

        let id = plan.lookalikes[0].id.clone();
        let again = run(&env, true, &[id]).unwrap();
        assert!(again.changes.iter().all(|c| c.action == Action::Unchanged));
        let toml = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(!toml.contains("mcp-server-redis") && toml.contains("[mcp_servers.devcat]"), "{toml}");
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(folder));
    }

    #[test]
    fn agents_not_installed_are_left_alone() {
        let (home, folder) = (tmp(), tmp());
        let plan = run(&env(&home, &folder), true, &[]).unwrap();
        assert!(plan.changes.is_empty() && plan.agents.is_empty());
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(folder));
    }

    #[test]
    fn repo_configs_only_lose_ticked_lookalikes() {
        let (home, folder) = (tmp(), tmp());
        std::fs::write(folder.join(".mcp.json"), r#"{"mcpServers":{"db":{"command":"npx","args":["@modelcontextprotocol/server-postgres","postgresql://localhost:5432/shop"]},"web":{"command":"npx","args":["web"]}}}"#).unwrap();
        let env = env(&home, &folder);
        let plan = run(&env, true, &[]).unwrap();
        assert_eq!(plan.lookalikes.len(), 1);
        assert!(std::fs::read_to_string(folder.join(".mcp.json")).unwrap().contains("server-postgres"), "not ticked: kept");
        run(&env, true, &[plan.lookalikes[0].id.clone()]).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(folder.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(v["mcpServers"].as_object().unwrap().keys().collect::<Vec<_>>(), ["web"]);
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(folder));
    }
}
