//! Model providers. [`Llm::step`] sends the conversation and streams one assistant message back;
//! each wire protocol has its own small adapter.
mod anthropic;
mod openai;
pub(crate) mod sse;

use crate::message::{Block, Message, ToolSpec, Usage};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI-compatible Chat Completions (`/chat/completions`): OpenAI, DeepSeek, Kimi, GLM,
    /// OpenRouter, Ollama, LM Studio, vLLM and most gateways.
    OpenaiChat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub protocol: Protocol,
    /// API root, e.g. `https://api.anthropic.com`, `https://api.openai.com/v1`,
    /// `http://127.0.0.1:11434/v1` (Ollama).
    pub base_url: String,
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    8192
}

/// What one model call needs.
pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    /// Stable per session: lets providers that support it keep the session on one cache.
    pub cache_key: Option<&'a str>,
}

/// Pieces of the answer as they arrive, for live display.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    /// The model started a tool call (its input is still streaming).
    ToolStart { id: String, name: String },
}

/// The finished assistant message of one step.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub content: Vec<Block>,
    pub usage: Usage,
    /// The model stopped because it hit `max_tokens`.
    pub truncated: bool,
}

pub struct Llm {
    pub config: LlmConfig,
    http: reqwest::Client,
}

/// Retries for overloaded / rate-limited / flaky providers, before anything was streamed.
const RETRIES: u32 = 3;

impl Llm {
    pub fn new(config: LlmConfig) -> Llm {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            // a stalled stream: no bytes for this long
            .read_timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        Llm { config, http }
    }

    /// Run one model step, calling `on` for every streamed piece.
    pub async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply> {
        let mut attempt = 0;
        loop {
            let res = match self.config.protocol {
                Protocol::Anthropic => anthropic::send(&self.http, &self.config, req).await,
                Protocol::OpenaiChat => openai::send(&self.http, &self.config, req).await,
            };
            match res {
                Ok(resp) => {
                    return match self.config.protocol {
                        Protocol::Anthropic => anthropic::read(resp, on).await,
                        Protocol::OpenaiChat => openai::read(resp, on).await,
                    };
                }
                Err(Retry::Later(e, wait)) if attempt < RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(wait.unwrap_or(Duration::from_secs(2u64.pow(attempt)))).await;
                    let _ = e;
                }
                Err(Retry::Later(e, _)) | Err(Retry::Never(e)) => return Err(e),
            }
        }
    }
}

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelInfo {
    pub id: String,
    /// Context window in tokens, when the provider says (OpenRouter, Anthropic, LM Studio,
    /// vLLM, Kimi, ...). Many (OpenAI, Z.ai, DeepSeek) don't.
    pub context: Option<u64>,
    /// List price, when the provider says (OpenRouter does).
    pub price: Option<Price>,
}

/// What a model costs, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Input read from the prompt cache, when it's priced apart.
    pub cache_read: Option<f64>,
}

/// The price in an OpenRouter-style model entry (`pricing`: dollars per token, as strings).
pub fn price_of(m: &serde_json::Value) -> Option<Price> {
    let per_mtok = |k: &str| {
        let v = m.get("pricing")?.get(k)?;
        let n = v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64())?;
        (n >= 0.0).then_some(n * 1_000_000.0)
    };
    Some(Price { input: per_mtok("prompt")?, output: per_mtok("completion")?, cache_read: per_mtok("input_cache_read") })
}

/// Models the provider offers (`GET {base}/models`, or `/v1/models` for Anthropic), newest first.
/// Embedding models are left out: they can't chat.
pub async fn list_models(cfg: &LlmConfig) -> Result<Vec<ModelInfo>> {
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(20)).build().unwrap_or_default();
    let key = cfg.api_key.as_deref().filter(|k| !k.is_empty());
    let req = match cfg.protocol {
        Protocol::OpenaiChat => {
            let r = http.get(url(&cfg.base_url, "models"));
            if let Some(k) = key { r.bearer_auth(k) } else { r }
        }
        Protocol::Anthropic => {
            let path = if cfg.base_url.trim_end_matches('/').ends_with("/v1") { "models?limit=1000" } else { "v1/models?limit=1000" };
            let r = http.get(url(&cfg.base_url, path)).header("anthropic-version", "2023-06-01");
            if let Some(k) = key { r.header("x-api-key", k) } else { r }
        }
    };
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(match http_error(resp).await {
            Retry::Later(e, _) | Retry::Never(e) => e,
        });
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| Error::Provider(format!("unexpected model list: {e}")))?;
    Ok(models(&v))
}

/// The context window a model entry reports, under any of the names providers use.
fn context_of(m: &serde_json::Value) -> Option<u64> {
    ["context_length", "max_context_length", "context_window", "max_input_tokens", "max_model_len", "max_seq_len"]
        .iter()
        .find_map(|k| m.get(k).and_then(|c| c.as_u64()))
        .or_else(|| m.pointer("/top_provider/context_length").and_then(|c| c.as_u64()))
        .filter(|&c| c > 0)
}

fn models(v: &serde_json::Value) -> Vec<ModelInfo> {
    let mut models: Vec<(String, ModelInfo)> = v
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            // OpenAI: unix seconds; Anthropic: RFC 3339. Both sort as text once padded.
            let created = m.get("created").and_then(|c| c.as_u64()).map(|c| format!("{c:020}")).or_else(|| m.get("created_at").and_then(|c| c.as_str()).map(String::from)).unwrap_or_default();
            Some((created, ModelInfo { id, context: context_of(m), price: price_of(m) }))
        })
        .filter(|(_, m)| !m.id.to_lowercase().contains("embed"))
        .collect();
    models.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    models.dedup_by(|a, b| a.1.id == b.1.id);
    models.into_iter().map(|(_, m)| m).collect()
}

/// Why a request failed, and whether trying again can help.
pub(crate) enum Retry {
    Later(Error, Option<Duration>),
    Never(Error),
}

/// Turn a non-2xx response into an error with the provider's message.
pub(crate) async fn http_error(resp: reqwest::Response) -> Retry {
    let status = resp.status();
    let wait = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.min(60)));
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(500).collect());
    let e = Error::Provider(format!("{} {}", status.as_u16(), msg.trim()));
    if status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error() { Retry::Later(e, wait) } else { Retry::Never(e) }
}

pub(crate) fn transport(e: reqwest::Error) -> Retry {
    if e.is_connect() || e.is_timeout() { Retry::Later(e.into(), None) } else { Retry::Never(e.into()) }
}

/// `base_url` + `path`, without doubling the slash.
pub(crate) fn url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_list_newest_first_without_embeddings() {
        let ids = |v: &serde_json::Value| models(v).into_iter().map(|m| m.id).collect::<Vec<_>>();
        let openai = serde_json::json!({ "data": [{ "id": "glm-4.5", "created": 1753632000 }, { "id": "glm-5.2", "created": 1781625600 }, { "id": "nomic-embed-text", "created": 1790000000 }] });
        assert_eq!(ids(&openai), vec!["glm-5.2", "glm-4.5"]);
        let anthropic = serde_json::json!({ "data": [{ "id": "a", "created_at": "2025-07-28T00:00:00Z" }, { "id": "b", "created_at": "2026-01-01T00:00:00Z" }] });
        assert_eq!(ids(&anthropic), vec!["b", "a"]);
    }

    #[test]
    fn model_list_reads_context_windows() {
        let v = serde_json::json!({ "data": [
            { "id": "openrouter", "context_length": 1048576 },
            { "id": "anthropic", "max_input_tokens": 200000 },
            { "id": "lmstudio", "max_context_length": 32768 },
            { "id": "nested", "top_provider": { "context_length": 64000 } },
            { "id": "unknown" },
        ] });
        let got: Vec<_> = models(&v).into_iter().map(|m| (m.id, m.context)).collect();
        assert!(got.contains(&("openrouter".into(), Some(1048576))));
        assert!(got.contains(&("anthropic".into(), Some(200000))));
        assert!(got.contains(&("lmstudio".into(), Some(32768))));
        assert!(got.contains(&("nested".into(), Some(64000))));
        assert!(got.contains(&("unknown".into(), None)));
    }

    #[test]
    fn model_list_reads_openrouter_prices() {
        let v = serde_json::json!({ "data": [
            { "id": "z-ai/glm-5.3", "pricing": { "prompt": "0.0000006", "completion": "0.0000022", "input_cache_read": "0.00000011" } },
            { "id": "free", "pricing": { "prompt": "0", "completion": "0" } },
            { "id": "half", "pricing": { "prompt": "0.000001" } },
            { "id": "none" },
        ] });
        let price = |id: &str| models(&v).into_iter().find(|m| m.id == id).unwrap().price;
        let glm = price("z-ai/glm-5.3").unwrap();
        assert!((glm.input - 0.6).abs() < 1e-9 && (glm.output - 2.2).abs() < 1e-9 && (glm.cache_read.unwrap() - 0.11).abs() < 1e-9, "{glm:?}");
        assert_eq!(price("free"), Some(Price { input: 0.0, output: 0.0, cache_read: None }));
        assert_eq!(price("half"), None);
        assert_eq!(price("none"), None);
    }
}
