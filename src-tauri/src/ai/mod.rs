//! AI connections for the AI workspace: saved profiles for the coding-agent CLI that runs in
//! the workspace terminal (Claude Code, Cursor, Codex, Gemini, Antigravity, Copilot, Aider, opencode, Hermes or a
//! custom command). `pty_open(provider_id)` resolves program / args / env here, so API keys go
//! from the keychain straight into the child's environment and never reach the frontend.
pub mod claude_account;
pub mod store;
pub mod usage;

use crate::claude::locate;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use store::AiStore;

/// Id of the implicit Claude Code profile.
pub const BUILTIN_ID: &str = "claude";

const VERSION_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiKind {
    Claude,
    /// Cursor's CLI agent (`cursor-agent`, also installed as `agent`).
    Cursor,
    Codex,
    Gemini,
    /// Google Antigravity's terminal agent (`agy`), signed in with a Google account.
    Antigravity,
    Copilot,
    Aider,
    Opencode,
    /// Nous Research's Hermes Agent (`hermes`): any provider it knows (Nous Portal, whose free plan
    /// runs the `:free` models, OpenRouter, a local server…), with the model and provider the
    /// connection chooses ([`HermesSettings`]).
    Hermes,
    Custom,
    /// DevCat's own agent (crates/harness): talks to a model API directly, no CLI.
    Agent,
}

impl AiKind {
    pub const BUILT_IN: [AiKind; 9] = [AiKind::Claude, AiKind::Cursor, AiKind::Codex, AiKind::Gemini, AiKind::Antigravity, AiKind::Copilot, AiKind::Aider, AiKind::Opencode, AiKind::Hermes];

    pub fn label(self) -> &'static str {
        match self {
            AiKind::Claude => "Claude Code",
            AiKind::Cursor => "Cursor",
            AiKind::Codex => "OpenAI Codex",
            AiKind::Gemini => "Gemini CLI",
            AiKind::Antigravity => "Antigravity",
            AiKind::Copilot => "GitHub Copilot",
            AiKind::Aider => "Aider",
            AiKind::Opencode => "opencode",
            AiKind::Hermes => "Hermes Agent",
            AiKind::Custom => "Custom",
            AiKind::Agent => "DevCat Agent",
        }
    }

    /// Default binary name (None for custom).
    pub fn program(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("claude"),
            AiKind::Cursor => Some("cursor-agent"),
            AiKind::Codex => Some("codex"),
            AiKind::Gemini => Some("gemini"),
            AiKind::Antigravity => Some("agy"),
            AiKind::Copilot => Some("copilot"),
            AiKind::Aider => Some("aider"),
            AiKind::Opencode => Some("opencode"),
            AiKind::Hermes => Some("hermes"),
            AiKind::Custom | AiKind::Agent => None,
        }
    }

    /// Conventional env var for an API key (None = the user must choose).
    pub fn default_key_env(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("ANTHROPIC_API_KEY"),
            AiKind::Cursor => Some("CURSOR_API_KEY"),
            AiKind::Codex | AiKind::Aider | AiKind::Opencode => Some("OPENAI_API_KEY"),
            AiKind::Gemini | AiKind::Antigravity => Some("GEMINI_API_KEY"),
            AiKind::Copilot => Some("GH_TOKEN"),
            // Nous Portal logs in with `hermes setup --portal` (no key); a key is most often OpenRouter's
            AiKind::Hermes => Some("OPENROUTER_API_KEY"),
            // not an env var: the agent sends the key itself
            AiKind::Agent => Some("DEVCAT_AGENT_API_KEY"),
            AiKind::Custom => None,
        }
    }

    pub fn install_hint(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("curl -fsSL https://claude.ai/install.sh | bash"),
            AiKind::Cursor => Some("curl https://cursor.com/install -fsS | bash"),
            AiKind::Codex => Some("npm i -g @openai/codex"),
            AiKind::Gemini => Some("npm i -g @google/gemini-cli"),
            AiKind::Antigravity => Some("curl -fsSL https://antigravity.google/cli/install.sh | bash"),
            AiKind::Copilot => Some("npm i -g @github/copilot"),
            AiKind::Aider => Some("pip install aider-chat"),
            AiKind::Opencode => Some("npm i -g opencode-ai"),
            AiKind::Hermes => Some("curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash"),
            AiKind::Custom | AiKind::Agent => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiEnvVar {
    pub key: String,
    pub value: String,
}

/// A saved AI connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: AiKind,
    /// Program override (name on PATH or absolute path); None = the kind's default binary.
    #[serde(default)]
    pub command: Option<String>,
    /// Extra arguments, put before the ones the workspace adds (e.g. claude's --resume).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<AiEnvVar>,
    /// Variable the API key is exported as (None = the kind's default).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Write-only: Some(key) stores it in the keychain, Some("") removes it, None keeps it.
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    /// Read-only: an API key is stored in the keychain.
    #[serde(default)]
    pub has_api_key: bool,
    /// Read-only: the implicit Claude Code profile (delete = reset).
    #[serde(default)]
    pub builtin: bool,
    /// DevCat Agent only: which model API to talk to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSettings>,
    /// Hermes Agent only: the model and provider it runs with (none: what Hermes has set up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes: Option<HermesSettings>,
    /// Claude Code only (not the built-in one): a login of its own, kept in its own config folder
    /// ([`claude_account`]), so several Claude accounts can be used side by side.
    #[serde(default)]
    pub own_login: bool,
    /// Read-only: the account a Claude Code connection is logged in with (its email).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

/// Model API of a DevCat Agent connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    pub protocol: devcat_harness::Protocol,
    pub base_url: String,
    pub model: String,
    /// Steps per turn before the agent pauses and asks to continue; 0 = no limit, None = the
    /// default ([`devcat_harness::MAX_STEPS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    /// Coding agents (Claude Code, Codex… over `devcat mcp`, and DevCat Agent chats) may hand it
    /// work with `agent_delegate`. Off unless the user turns it on for this connection.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delegate: bool,
}

/// The model and provider of a Hermes Agent connection: `hermes chat --model=… --provider=…`.
/// Empty fields leave Hermes's own choice (`hermes model`, `~/.hermes/config.yaml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HermesSettings {
    /// e.g. `nvidia/nemotron-3-ultra:free` (Nous Portal's free plan), `openai/gpt-5.5`.
    #[serde(default)]
    pub model: String,
    /// Hermes's provider id: `nous` (Nous Portal), `openrouter`, `anthropic`, `custom`…
    #[serde(default)]
    pub provider: String,
    /// The OpenAI-compatible address, for the `custom` provider only (a local server).
    #[serde(default)]
    pub base_url: String,
}

impl HermesSettings {
    /// What `hermes` is started with: `chat` (its classic terminal chat, where `--model` and
    /// `--provider` apply; at the top level they're for `-z` and `--tui` only), then the
    /// connection's choices, as `--flag=value` so a value can never read as another flag.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["chat".to_string()];
        if !self.model.is_empty() {
            a.push(format!("--model={}", self.model));
        }
        if !self.provider.is_empty() {
            a.push(format!("--provider={}", self.provider));
        }
        if self.provider == "custom" && !self.base_url.is_empty() {
            a.push(format!("--base-url={}", self.base_url));
        }
        a
    }

    /// Trimmed and checked for saving.
    pub fn cleaned(mut self) -> AppResult<Self> {
        self.model = self.model.trim().to_string();
        self.provider = self.provider.trim().to_ascii_lowercase();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if self.model.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(AppError::msg("The Hermes model name can't contain spaces"));
        }
        if !self.provider.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(AppError::msg("The Hermes provider is a name like nous, openrouter or custom"));
        }
        if self.provider != "custom" {
            self.base_url.clear();
        } else if !self.base_url.is_empty() && !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://")) {
            return Err(AppError::msg("The API address must start with https:// or http://"));
        }
        Ok(self)
    }
}

impl AiProvider {
    /// Env var name the API key goes into.
    pub fn key_env(&self) -> Option<String> {
        self.api_key_env.clone().or_else(|| self.kind.default_key_env().map(str::to_string))
    }

    /// Program name / path to run (before PATH resolution).
    fn program(&self) -> AppResult<String> {
        self.command
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| self.kind.program().map(str::to_string))
            .ok_or_else(|| AppError::msg(format!("{} has no command configured", self.name)))
    }
}

/// What the terminal runs for a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Program / args / env for a profile (program not resolved on PATH yet).
pub fn plan(p: &AiProvider, api_key: Option<String>, extra_args: Vec<String>) -> AppResult<Launch> {
    let program = p.program()?;
    // Hermes: `chat` and the connection's model / provider first, then the user's own arguments
    let lead = match (p.kind, &p.hermes) {
        (AiKind::Hermes, Some(h)) => h.args(),
        (AiKind::Hermes, None) => vec!["chat".to_string()],
        _ => vec![],
    };
    let args = lead.into_iter().chain(p.args.iter().cloned()).chain(extra_args).collect();
    let mut env: Vec<(String, String)> = p.env.iter().map(|e| (e.key.clone(), e.value.clone())).collect();
    if let (Some(key), Some(var)) = (api_key.filter(|k| !k.is_empty()), p.key_env()) {
        env.retain(|(k, _)| *k != var);
        env.push((var, key));
    }
    Ok(Launch { program, args, env })
}

/// Absolute path of the profile's program (claude via its dedicated lookup).
async fn resolve_program(p: &AiProvider, program: &str) -> Option<PathBuf> {
    if p.kind == AiKind::Claude && p.command.is_none() {
        return locate::claude_path();
    }
    // Cursor's installer puts the CLI on PATH as `cursor-agent` and, in newer versions, also as `agent`.
    if p.kind == AiKind::Cursor && p.command.is_none() {
        return match locate::find_program(program).await {
            Some(path) => Some(path),
            None => locate::find_program("agent").await,
        };
    }
    locate::find_program(program).await
}

fn not_found(p: &AiProvider, program: &str) -> AppError {
    let hint = match (p.command.is_none(), p.kind.install_hint()) {
        (true, Some(h)) => format!(" Install it with: {h}"),
        _ => String::new(),
    };
    AppError::msg(format!("{program} not found on PATH ({}).{hint}", p.name))
}

/// Model API settings (with the keychain key) for a DevCat Agent connection.
pub fn agent_config(provider_id: &str) -> AppResult<(AiProvider, devcat_harness::LlmConfig)> {
    let st = store()?;
    let p = st.get(provider_id)?;
    let a = p.agent.clone().filter(|_| p.kind == AiKind::Agent).ok_or_else(|| AppError::msg(format!("{} is not a DevCat Agent connection", p.name)))?;
    let cfg = devcat_harness::LlmConfig { protocol: a.protocol, base_url: a.base_url, api_key: st.api_key(&p.id), model: a.model, max_tokens: 16_000, effort: None };
    Ok((p, cfg))
}

/// DevCat Agent connections the user lets coding agents hand work to (`AgentSettings::delegate`).
pub fn delegates() -> Vec<AiProvider> {
    store().and_then(|st| st.list()).unwrap_or_default().into_iter().filter(|p| p.kind == AiKind::Agent && p.agent.as_ref().is_some_and(|a| a.delegate)).collect()
}

/// Steps per turn for a DevCat Agent connection (its setting, read fresh so a change applies to
/// open chats on their next turn).
pub fn agent_max_steps(provider_id: &str) -> u32 {
    store().and_then(|st| st.get(provider_id)).ok().and_then(|p| p.agent).and_then(|a| a.max_steps).unwrap_or(devcat_harness::MAX_STEPS)
}

/// Models a DevCat Agent API offers, for the model picker. Uses `api_key` (typed in the form, not
/// saved yet) or else the key stored for `provider_id`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_agent_models(settings: AgentSettings, provider_id: Option<String>, api_key: Option<String>) -> AppResult<Vec<devcat_harness::llm::ModelInfo>> {
    let key = api_key.filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string()).or_else(|| provider_id.and_then(|id| store().ok()?.api_key(&id)));
    let cfg = devcat_harness::LlmConfig { protocol: settings.protocol, base_url: settings.base_url.trim().to_string(), api_key: key, model: String::new(), max_tokens: 0, effort: None };
    let mut models = devcat_harness::llm::list_models(&cfg).await.map_err(|e| AppError::msg(e.to_string()))?;
    // Many providers (Z.ai, OpenAI, DeepSeek) don't say how much context a model takes or what it
    // costs: look the name up in OpenRouter's public catalog (its list price: an estimate).
    if models.iter().any(|m| m.context.is_none() || m.price.is_none()) {
        let catalog = catalog().await;
        for m in models.iter_mut() {
            if let Some((context, price)) = catalog.get(&model_key(&m.id)) {
                m.context = m.context.or(*context);
                m.price = m.price.or(*price);
            }
        }
    }
    Ok(models)
}

/// The context window of a DevCat Agent connection's model: what its own API says (local servers
/// such as llama.cpp report the window they loaded), else OpenRouter's catalog.
pub async fn provider_context_window(provider_id: &str) -> Option<u64> {
    let (_, cfg) = agent_config(provider_id).ok()?;
    let listed = tokio::time::timeout(std::time::Duration::from_secs(8), devcat_harness::llm::list_models(&cfg)).await.ok().and_then(Result::ok);
    if let Some(c) = listed.into_iter().flatten().find(|m| m.id == cfg.model).and_then(|m| m.context) {
        return Some(c);
    }
    context_window(&cfg.model).await
}

/// A model's context window from OpenRouter's catalog, when it lists the model (scheduled jobs
/// run without the window, which otherwise passes it from the model picker).
pub async fn context_window(model: &str) -> Option<u64> {
    catalog().await.get(&model_key(model)).and_then(|(context, _)| *context)
}

/// `vendor/Model-Name:variant` -> `model-name`, to match a provider's model id to the catalog's.
fn model_key(id: &str) -> String {
    let id = id.rsplit('/').next().unwrap_or(id);
    id.split(':').next().unwrap_or(id).to_lowercase()
}

type Catalog = std::collections::HashMap<String, (Option<u64>, Option<devcat_harness::llm::Price>)>;

/// Context windows and list prices by model name, from OpenRouter's public model list (no key,
/// nothing sent). Asked once per run; empty when offline.
async fn catalog() -> &'static Catalog {
    static CATALOG: tokio::sync::OnceCell<Catalog> = tokio::sync::OnceCell::const_new();
    CATALOG
        .get_or_init(|| async {
            let mut map = Catalog::new();
            let Ok(http) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).build() else { return map };
            let Ok(resp) = http.get("https://openrouter.ai/api/v1/models").send().await else { return map };
            let Ok(v) = resp.json::<serde_json::Value>().await else { return map };
            for m in v["data"].as_array().into_iter().flatten() {
                let Some(id) = m["id"].as_str() else { continue };
                if id.starts_with('~') {
                    continue; // aliases like ~z-ai/glm-latest
                }
                // The first listing of a name wins (the vendor's own, before resellers' variants).
                map.entry(model_key(id)).or_insert((m["context_length"].as_u64(), devcat_harness::llm::price_of(m)));
            }
            map
        })
        .await
}

static STORE: once_cell::sync::OnceCell<AiStore> = once_cell::sync::OnceCell::new();

fn store() -> AppResult<&'static AiStore> {
    STORE.get().ok_or_else(|| AppError::msg("AI connection store not initialised"))
}

/// Called once from `lib.rs` setup (and the dev bridge).
pub fn init(data_dir: PathBuf) {
    let _ = STORE.set(AiStore::new(data_dir, Box::new(crate::network::store::Keyring)));
}

/// Resolved launch for a saved profile; `extra_args` go after the profile's own args.
pub async fn launch(provider_id: &str, extra_args: Vec<String>) -> AppResult<Launch> {
    let st = store()?;
    let p = st.get(provider_id)?;
    let mut l = plan(&p, st.api_key(&p.id), extra_args)?;
    if let Some(dir) = st.account_dir(&p) {
        let main = claude_account::main_dir();
        { let dir = dir.clone(); tokio::task::spawn_blocking(move || claude_account::prepare(&dir, &main)).await.map_err(|e| AppError::msg(e.to_string()))??; }
        // first, so a CLAUDE_CONFIG_DIR row the user added still wins
        l.env.insert(0, ("CLAUDE_CONFIG_DIR".into(), dir.to_string_lossy().into_owned()));
    }
    let path = resolve_program(&p, &l.program).await.ok_or_else(|| not_found(&p, &l.program))?;
    l.program = path.to_string_lossy().into_owned();
    Ok(l)
}

/// The config folders of the Claude Code connections with a login of their own that have been
/// started (Sync MCP writes DevCat's server into each one's `.claude.json` too).
pub fn claude_account_dirs() -> Vec<PathBuf> {
    let Ok(st) = store() else { return vec![] };
    st.list().unwrap_or_default().iter().filter_map(|p| st.account_dir(p)).filter(|d| d.is_dir()).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDetect {
    pub kind: AiKind,
    /// Profile tested (None when detecting the built-in kinds).
    pub provider_id: Option<String>,
    /// Program looked up.
    pub program: String,
    /// Absolute path, None = not installed.
    pub path: Option<String>,
    pub version: Option<String>,
    pub install_hint: Option<String>,
    pub error: Option<String>,
}

/// First line of `<bin> --version` (stdout, else stderr), bounded by a timeout.
async fn version_of(bin: &std::path::Path) -> Result<String, String> {
    let mut cmd = locate::tool_command(&bin.to_string_lossy()).await;
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = match tokio::time::timeout(VERSION_TIMEOUT, cmd.output()).await {
        Err(_) => return Err("--version timed out".into()),
        Ok(Err(e)) => return Err(e.to_string()),
        Ok(Ok(o)) => o,
    };
    let first = |b: &[u8]| String::from_utf8_lossy(b).lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(80).collect::<String>());
    match first(&out.stdout).or_else(|| first(&out.stderr)) {
        Some(v) if out.status.success() => Ok(v),
        Some(v) => Err(v),
        None => Err(format!("--version exited with {}", out.status)),
    }
}

async fn detect_one(p: &AiProvider, provider_id: Option<String>) -> AiDetect {
    let program = p.program().unwrap_or_default();
    let path = if program.is_empty() { None } else { resolve_program(p, &program).await };
    let (version, error) = match &path {
        Some(bin) => match version_of(bin).await {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        },
        None if program.is_empty() => (None, Some("No command configured".into())),
        None => (None, Some(format!("{program} not found"))),
    };
    AiDetect {
        kind: p.kind,
        provider_id,
        program,
        path: path.map(|p| p.to_string_lossy().into_owned()),
        version,
        install_hint: if p.command.is_none() { p.kind.install_hint().map(str::to_string) } else { None },
        error,
    }
}

// ---------------------------------------------------------------- commands

/// The profiles without their logins (for DevCat itself: picking one by id, name or type).
pub async fn providers() -> AppResult<Vec<AiProvider>> {
    store()?.list()
}

/// The profiles for the window and the phone, with the account each Claude Code one is logged in with.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_providers_list() -> AppResult<Vec<AiProvider>> {
    tokio::task::spawn_blocking(|| {
        let st = store()?;
        let mut list = st.list()?;
        for p in &mut list {
            p.login = st.login_of(p);
        }
        Ok(list)
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_provider_save(provider: AiProvider) -> AppResult<AiProvider> {
    store()?.save(provider)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_provider_delete(id: String) -> AppResult<()> {
    store()?.delete(&id)
}

/// Without `provider`: whether each built-in CLI is installed (+ version).
/// With one (possibly unsaved edits): test that profile's program.
const CREATE_CHAT_TIMEOUT: Duration = Duration::from_secs(20);

/// A new Cursor chat for a Cursor connection (`cursor-agent create-chat`), so the workspace tab
/// knows its chat id before the first message and resumes it (`--resume <id>`) on every restart,
/// as Claude Code tabs do with `--session-id`. Runs with the connection's env and API key; the
/// profile's own args are interactive-session flags, so they're left out.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_cursor_create_chat(provider_id: String, cwd: String) -> AppResult<String> {
    let st = store()?;
    let p = st.get(&provider_id)?;
    if p.kind != AiKind::Cursor {
        return Err(AppError::msg(format!("{} is not a Cursor connection", p.name)));
    }
    let l = plan(&p, st.api_key(&p.id), Vec::new())?;
    let bin = resolve_program(&p, &l.program).await.ok_or_else(|| not_found(&p, &l.program))?;
    let mut cmd = locate::tool_command(&bin.to_string_lossy()).await;
    cmd.arg("create-chat").envs(l.env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if std::path::Path::new(&cwd).is_dir() {
        cmd.current_dir(&cwd);
    }
    let out = match tokio::time::timeout(CREATE_CHAT_TIMEOUT, cmd.output()).await {
        Err(_) => return Err(AppError::msg("cursor-agent create-chat timed out")),
        Ok(r) => r?,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    match chat_id_of(&stdout) {
        Some(id) if out.status.success() => Ok(id),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            let why = err.lines().chain(stdout.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("no chat id printed");
            Err(AppError::msg(format!("cursor-agent create-chat failed: {}", why.chars().take(200).collect::<String>())))
        }
    }
}

/// The chat id `create-chat` prints: the last non-empty line, if it looks like an id (it's passed
/// back as an argument, so nothing but letters, digits, `-` and `_`).
fn chat_id_of(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).last()?;
    let ok = (8..=128).contains(&line.len()) && line.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| line.to_string())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_provider_detect(provider: Option<AiProvider>) -> AppResult<Vec<AiDetect>> {
    Ok(match provider {
        Some(p) => vec![detect_one(&p, Some(p.id.clone()).filter(|i| !i.is_empty())).await],
        None => {
            let defaults: Vec<AiProvider> = AiKind::BUILT_IN
                .iter()
                .map(|&kind| AiProvider { kind, name: kind.label().into(), ..store::builtin_claude() })
                .collect();
            futures::future::join_all(defaults.iter().map(|p| detect_one(p, None))).await
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_chat_id() {
        assert_eq!(chat_id_of("c6b62c6f-7ead-4fd6-9922-e952131177ff\n").as_deref(), Some("c6b62c6f-7ead-4fd6-9922-e952131177ff"));
        assert_eq!(chat_id_of("Created chat\n\n  abc123def456  \n").as_deref(), Some("abc123def456"));
        assert_eq!(chat_id_of(""), None);
        assert_eq!(chat_id_of("Error: not logged in"), None);
        assert_eq!(chat_id_of("--model=x; rm -rf /"), None);
        assert_eq!(chat_id_of("short"), None);
    }

    #[test]
    fn cursor_kind() {
        assert!(AiKind::BUILT_IN.contains(&AiKind::Cursor));
        assert_eq!(AiKind::Cursor.program(), Some("cursor-agent"));
        assert_eq!(AiKind::Cursor.default_key_env(), Some("CURSOR_API_KEY"));
        assert_eq!(serde_json::to_value(AiKind::Cursor).unwrap(), "cursor");
    }

    fn provider(kind: AiKind) -> AiProvider {
        AiProvider { kind, name: kind.label().into(), ..store::builtin_claude() }
    }

    #[test]
    fn plan_defaults_to_own_login() {
        let l = plan(&provider(AiKind::Claude), None, vec!["--resume".into(), "abc".into()]).unwrap();
        assert_eq!(l, Launch { program: "claude".into(), args: vec!["--resume".into(), "abc".into()], env: vec![] });
    }

    #[test]
    fn plan_injects_key_and_env() {
        let mut p = provider(AiKind::Codex);
        p.args = vec!["--full-auto".into()];
        p.env = vec![AiEnvVar { key: "OPENAI_API_KEY".into(), value: "from-env".into() }, AiEnvVar { key: "X".into(), value: "1".into() }];
        let l = plan(&p, Some("sk-1".into()), vec!["more".into()]).unwrap();
        assert_eq!(l.program, "codex");
        assert_eq!(l.args, vec!["--full-auto", "more"]);
        // The keychain key wins over a plain env row with the same name.
        assert_eq!(l.env, vec![("X".into(), "1".into()), ("OPENAI_API_KEY".into(), "sk-1".into())]);

        for (kind, var) in [(AiKind::Claude, "ANTHROPIC_API_KEY"), (AiKind::Gemini, "GEMINI_API_KEY"), (AiKind::Aider, "OPENAI_API_KEY")] {
            let l = plan(&provider(kind), Some("k".into()), vec![]).unwrap();
            assert_eq!(l.env, vec![(var.to_string(), "k".to_string())]);
        }
        let mut aider = provider(AiKind::Aider);
        aider.api_key_env = Some("ANTHROPIC_API_KEY".into());
        assert_eq!(plan(&aider, Some("k".into()), vec![]).unwrap().env, vec![("ANTHROPIC_API_KEY".into(), "k".into())]);
        assert!(plan(&provider(AiKind::Gemini), Some(String::new()), vec![]).unwrap().env.is_empty());
    }

    #[test]
    fn plan_custom_and_override() {
        assert!(plan(&provider(AiKind::Custom), None, vec![]).is_err());
        let mut c = provider(AiKind::Custom);
        c.command = Some("/opt/llm/bin/agent".into());
        c.api_key_env = Some("AGENT_KEY".into());
        let l = plan(&c, Some("s".into()), vec![]).unwrap();
        assert_eq!(l.program, "/opt/llm/bin/agent");
        assert_eq!(l.env, vec![("AGENT_KEY".into(), "s".into())]);
        // Custom without a var name: the key is not exported anywhere.
        c.api_key_env = None;
        assert!(plan(&c, Some("s".into()), vec![]).unwrap().env.is_empty());
        let mut g = provider(AiKind::Gemini);
        g.command = Some("~/bin/gemini-dev".into());
        assert_eq!(plan(&g, None, vec![]).unwrap().program, "~/bin/gemini-dev");
    }

    #[test]
    fn hermes_runs_its_model_and_provider() {
        assert!(AiKind::BUILT_IN.contains(&AiKind::Hermes));
        assert_eq!(AiKind::Hermes.program(), Some("hermes"));
        assert_eq!(serde_json::to_value(AiKind::Hermes).unwrap(), "hermes");
        let mut p = provider(AiKind::Hermes);
        // nothing chosen: Hermes's own setup
        assert_eq!(plan(&p, None, vec![]).unwrap().args, vec!["chat"]);
        p.hermes = Some(HermesSettings { model: "nvidia/nemotron-3-ultra:free".into(), provider: "nous".into(), base_url: "http://ignored".into() });
        p.args = vec!["--yolo".into()];
        let l = plan(&p, Some("sk-or".into()), vec![]).unwrap();
        assert_eq!(l.program, "hermes");
        // the address is only for the custom provider
        assert_eq!(l.args, vec!["chat", "--model=nvidia/nemotron-3-ultra:free", "--provider=nous", "--yolo"]);
        assert_eq!(l.env, vec![("OPENROUTER_API_KEY".into(), "sk-or".into())]);
        p.hermes = Some(HermesSettings { model: "qwen3.5:27b".into(), provider: "custom".into(), base_url: "http://localhost:11434/v1".into() });
        assert_eq!(plan(&p, None, vec![]).unwrap().args[1..4], ["--model=qwen3.5:27b", "--provider=custom", "--base-url=http://localhost:11434/v1"]);
        // other kinds ignore Hermes settings
        let mut c = provider(AiKind::Codex);
        c.hermes = p.hermes.clone();
        assert!(plan(&c, None, vec![]).unwrap().args.is_empty());
    }

    #[test]
    fn hermes_settings_are_checked() {
        let ok = HermesSettings { model: " openai/gpt-5.5 ".into(), provider: " OpenRouter ".into(), base_url: "http://x/v1/".into() }.cleaned().unwrap();
        assert_eq!(ok, HermesSettings { model: "openai/gpt-5.5".into(), provider: "openrouter".into(), base_url: String::new() });
        let custom = HermesSettings { model: String::new(), provider: "custom".into(), base_url: "http://127.0.0.1:8000/v1/".into() }.cleaned().unwrap();
        assert_eq!(custom.base_url, "http://127.0.0.1:8000/v1");
        assert!(HermesSettings { model: "a b".into(), ..Default::default() }.cleaned().is_err());
        assert!(HermesSettings { provider: "nous --yolo".into(), ..Default::default() }.cleaned().is_err());
        assert!(HermesSettings { provider: "custom".into(), base_url: "file:///etc".into(), ..Default::default() }.cleaned().is_err());
    }

    #[test]
    fn wire_format() {
        let mut p = provider(AiKind::Opencode);
        p.api_key = Some("secret".into());
        p.has_api_key = true;
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["kind"], "opencode");
        assert_eq!(v["hasApiKey"], true);
        assert!(v.get("apiKey").is_none());
        let back: AiProvider = serde_json::from_value(serde_json::json!({ "name": "x", "kind": "custom", "command": "llm", "apiKey": "k" })).unwrap();
        assert_eq!(back.api_key.as_deref(), Some("k"));
        assert!(back.id.is_empty() && back.args.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_custom_program() {
        let mut c = provider(AiKind::Custom);
        c.command = Some("git".into());
        let d = detect_one(&c, None).await;
        assert!(d.path.is_some(), "{d:?}");
        assert!(d.version.as_deref().is_some_and(|v| v.contains("git")), "{d:?}");
        c.command = Some("definitely-not-a-devcat-cli".into());
        let d = detect_one(&c, None).await;
        assert!(d.path.is_none() && d.error.is_some());
    }
}
