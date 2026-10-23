//! AI connections for the AI workspace: saved profiles for the coding-agent CLI that runs in
//! the workspace terminal (Claude Code, Codex, Gemini, Copilot, Aider, opencode or a custom
//! command). `pty_open(provider_id)` resolves program / args / env here, so API keys go
//! from the keychain straight into the child's environment and never reach the frontend.
pub mod store;

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
    Codex,
    Gemini,
    Copilot,
    Aider,
    Opencode,
    Custom,
    /// DevCat's own agent (crates/harness): talks to a model API directly, no CLI.
    Agent,
}

impl AiKind {
    pub const BUILT_IN: [AiKind; 6] = [AiKind::Claude, AiKind::Codex, AiKind::Gemini, AiKind::Copilot, AiKind::Aider, AiKind::Opencode];

    pub fn label(self) -> &'static str {
        match self {
            AiKind::Claude => "Claude Code",
            AiKind::Codex => "OpenAI Codex",
            AiKind::Gemini => "Gemini CLI",
            AiKind::Copilot => "GitHub Copilot",
            AiKind::Aider => "Aider",
            AiKind::Opencode => "opencode",
            AiKind::Custom => "Custom",
            AiKind::Agent => "DevCat Agent",
        }
    }

    /// Default binary name (None for custom).
    pub fn program(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("claude"),
            AiKind::Codex => Some("codex"),
            AiKind::Gemini => Some("gemini"),
            AiKind::Copilot => Some("copilot"),
            AiKind::Aider => Some("aider"),
            AiKind::Opencode => Some("opencode"),
            AiKind::Custom | AiKind::Agent => None,
        }
    }

    /// Conventional env var for an API key (None = the user must choose).
    pub fn default_key_env(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("ANTHROPIC_API_KEY"),
            AiKind::Codex | AiKind::Aider | AiKind::Opencode => Some("OPENAI_API_KEY"),
            AiKind::Gemini => Some("GEMINI_API_KEY"),
            AiKind::Copilot => Some("GH_TOKEN"),
            // not an env var: the agent sends the key itself
            AiKind::Agent => Some("DEVCAT_AGENT_API_KEY"),
            AiKind::Custom => None,
        }
    }

    pub fn install_hint(self) -> Option<&'static str> {
        match self {
            AiKind::Claude => Some("curl -fsSL https://claude.ai/install.sh | bash"),
            AiKind::Codex => Some("npm i -g @openai/codex"),
            AiKind::Gemini => Some("npm i -g @google/gemini-cli"),
            AiKind::Copilot => Some("npm i -g @github/copilot"),
            AiKind::Aider => Some("pip install aider-chat"),
            AiKind::Opencode => Some("npm i -g opencode-ai"),
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
}

/// Model API of a DevCat Agent connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    pub protocol: devcat_harness::Protocol,
    pub base_url: String,
    pub model: String,
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
    let args = p.args.iter().cloned().chain(extra_args).collect();
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
    let cfg = devcat_harness::LlmConfig { protocol: a.protocol, base_url: a.base_url, api_key: st.api_key(&p.id), model: a.model, max_tokens: 16_000 };
    Ok((p, cfg))
}

/// Models a DevCat Agent API offers, for the model picker. Uses `api_key` (typed in the form, not
/// saved yet) or else the key stored for `provider_id`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_agent_models(settings: AgentSettings, provider_id: Option<String>, api_key: Option<String>) -> AppResult<Vec<devcat_harness::llm::ModelInfo>> {
    let key = api_key.filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string()).or_else(|| provider_id.and_then(|id| store().ok()?.api_key(&id)));
    let cfg = devcat_harness::LlmConfig { protocol: settings.protocol, base_url: settings.base_url.trim().to_string(), api_key: key, model: String::new(), max_tokens: 0 };
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
    let path = resolve_program(&p, &l.program).await.ok_or_else(|| not_found(&p, &l.program))?;
    l.program = path.to_string_lossy().into_owned();
    Ok(l)
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

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn ai_providers_list() -> AppResult<Vec<AiProvider>> {
    store()?.list()
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
