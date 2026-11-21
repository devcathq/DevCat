//! Model providers. [`Llm::step`] sends the conversation and streams one assistant message back;
//! each wire protocol has its own small adapter.
mod anthropic;
mod openai;
mod responses;
pub(crate) mod sse;

use crate::message::{Block, Message, ToolSpec, Usage};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
    /// OpenAI-compatible Chat Completions (`/chat/completions`): OpenAI, DeepSeek, Kimi, GLM,
    /// OpenRouter, Ollama, LM Studio, vLLM and most gateways.
    OpenaiChat,
    /// OpenAI Responses API (`/responses`): OpenAI's newer models, some of them only there.
    OpenaiResponses,
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
    /// How hard the model thinks, when the user chose (None: the provider's default, and nothing
    /// about it is sent). Each adapter says it the way its API does (see [`Effort`]).
    #[serde(default)]
    pub effort: Option<Effort>,
}

/// How much the model reasons before it answers. Anthropic: adaptive thinking with
/// `output_config.effort` on Claude 4.6 and later, a thinking budget on other models; OpenAI:
/// `reasoning_effort` (Chat Completions) or `reasoning.effort` (Responses), where the top
/// levels are `high`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
    /// The level from its name (`low` … `max`); None for anything else (`auto`).
    pub fn parse(s: &str) -> Option<Effort> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_lowercase())).ok()
    }
    /// OpenAI's levels stop at `high`.
    pub(crate) fn openai(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            _ => "high",
        }
    }
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
    /// The step is tried again (`attempt` of `of`): the provider was busy, or the connection broke
    /// (`reason`). Whatever was streamed for this step so far is void and comes again.
    Retry { attempt: u32, of: u32, reason: String },
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
    /// The wait before the first retry; it doubles each time, up to [`MAX_WAIT`].
    backoff: Duration,
    /// This server ends its streams without saying so (no finish reason, no `[DONE]`): a stream
    /// that just ends is its answer. Learned from the server (see [`Llm::step`]).
    quiet_end: AtomicBool,
    /// This server has said "done" at the end of a stream: one that just ends is a broken one.
    says_done: AtomicBool,
}

/// Retries of one step: for an overloaded, rate-limited or unreachable provider, and for a stream
/// that breaks before the answer is complete.
pub const RETRIES: u32 = 4;

/// The longest wait between two tries.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// A step that has taken this long isn't tried again: a server that answers each try with five
/// minutes of silence would otherwise keep the turn waiting for half an hour.
const RETRY_FOR: Duration = Duration::from_secs(8 * 60);

/// Why reading a streamed answer stopped short.
pub(crate) enum Broken {
    /// The connection broke, or the provider said it's overloaded: trying again can help.
    Retry(Error),
    /// The provider refused or failed for good.
    Never(Error),
    /// The stream ended without the provider saying the answer was complete. What came is here.
    Incomplete(Reply),
}

/// Whether a reply that came without its end can still be a whole answer: it says something, and
/// no tool call was cut in the middle of its arguments.
fn looks_whole(reply: &Reply) -> bool {
    let says = reply.content.iter().any(|b| matches!(b, Block::Text { .. } | Block::ToolUse { .. }));
    let cut = reply.content.iter().any(|b| matches!(b, Block::ToolUse { input, .. } if input.get(UNPARSED).is_some()));
    says && !cut
}

/// The key under which a tool call's arguments are kept when they aren't valid JSON.
pub const UNPARSED: &str = "_unparsed_arguments";

impl Llm {
    pub fn new(config: LlmConfig) -> Llm {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            // a stalled stream: no bytes for this long
            .read_timeout(Duration::from_secs(300))
            // a long tool call sits between two steps: don't come back to a connection the
            // server gave up on meanwhile
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Llm { config, http, backoff: Duration::from_secs(2), quiet_end: AtomicBool::new(false), says_done: AtomicBool::new(false) }
    }

    /// The same, waiting `first` before the first retry (tests don't wait seconds).
    pub fn with_backoff(mut self, first: Duration) -> Llm {
        self.backoff = first;
        self
    }

    /// Run one model step, calling `on` for every streamed piece. A step that fails in a way
    /// another try can fix (the provider is busy, the connection broke, the stream stopped in the
    /// middle of the answer) is tried again up to [`RETRIES`] times; `on` hears of each retry
    /// ([`Delta::Retry`]), since what it streamed before is void.
    pub async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply> {
        let mut attempt = 0;
        let started = std::time::Instant::now();
        // a stream already ended without its end once in this step
        let mut ended_quietly = false;
        loop {
            let sent = match self.config.protocol {
                Protocol::Anthropic => anthropic::send(&self.http, &self.config, req).await,
                Protocol::OpenaiChat => openai::send(&self.http, &self.config, req).await,
                Protocol::OpenaiResponses => responses::send(&self.http, &self.config, req).await,
            };
            let (error, wait) = match sent {
                Ok(resp) => {
                    let read = match self.config.protocol {
                        Protocol::Anthropic => anthropic::read(resp, on).await,
                        Protocol::OpenaiChat => openai::read(resp, on).await,
                        Protocol::OpenaiResponses => responses::read(resp, on).await,
                    };
                    match read {
                        Ok(reply) => {
                            self.says_done.store(true, Ordering::Relaxed);
                            self.quiet_end.store(false, Ordering::Relaxed);
                            return Ok(reply);
                        }
                        Err(Broken::Never(e)) => return Err(e),
                        Err(Broken::Retry(e)) => (e, None),
                        Err(Broken::Incomplete(reply)) => {
                            // Some small OpenAI-compatible servers never say that they're done.
                            // One that ends a whole answer this way twice in a row, and has
                            // never said "done", is one of them and is believed from then on.
                            // Anthropic's and the Responses API always say it, and an answer
                            // cut in the middle is never whole.
                            let may_be_quiet = self.config.protocol == Protocol::OpenaiChat && !self.says_done.load(Ordering::Relaxed) && looks_whole(&reply);
                            if may_be_quiet && (ended_quietly || self.quiet_end.load(Ordering::Relaxed)) {
                                self.quiet_end.store(true, Ordering::Relaxed);
                                return Ok(reply);
                            }
                            // (out of tries or time on its first answer: taken as it is, once)
                            if may_be_quiet && (attempt >= RETRIES || started.elapsed() >= RETRY_FOR) {
                                return Ok(reply);
                            }
                            ended_quietly |= may_be_quiet;
                            (Error::Io("the connection closed before the model finished its answer".into()), None)
                        }
                    }
                }
                Err(Retry::Later(e, wait)) => (e, wait),
                Err(Retry::Never(e)) => return Err(e),
            };
            if attempt >= RETRIES || started.elapsed() >= RETRY_FOR {
                return Err(error);
            }
            attempt += 1;
            on(Delta::Retry { attempt, of: RETRIES, reason: error.to_string() });
            // the provider's own "retry after", or ours: doubling, up to MAX_WAIT
            let wait = wait.unwrap_or_else(|| (self.backoff * 2u32.pow(attempt - 1)).min(MAX_WAIT));
            tokio::time::sleep(wait).await;
        }
    }
}

/// Whether `e` is the provider saying the conversation doesn't fit the model's context window
/// (each provider words it its own way). Summarizing the conversation can fix that.
pub fn is_overflow(e: &Error) -> bool {
    let Error::Provider(m) = e else { return false };
    let m = m.to_lowercase();
    // 413 is the request's size in bytes; the same cure
    m.starts_with("413 ")
        || [
            "context length",
            "context_length",
            "context window",
            "context size",
            "context limit",
            "maximum context",
            "prompt is too long",
            "prompt too long",
            "input is too long",
            "too many tokens",
            "token limit",
            "maximum number of tokens",
            "exceeds the maximum input",
            "reduce the length",
            "request too large",
        ]
        .iter()
        .any(|p| m.contains(p))
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
        Protocol::OpenaiChat | Protocol::OpenaiResponses => {
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
        // llama.cpp's server (and servers built on it) put the loaded context under `meta`
        .or_else(|| m.pointer("/meta/n_ctx").and_then(|c| c.as_u64()))
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

/// A request that didn't get an answer. Nearly always the network (the server unreachable, a kept
/// connection it had closed meanwhile, a reset): trying again helps, and at worst asks the model
/// twice. Only a request that could never be sent (a bad address) isn't tried again.
pub(crate) fn transport(e: reqwest::Error) -> Retry {
    if e.is_builder() || e.is_redirect() { Retry::Never(e.into()) } else { Retry::Later(e.into(), None) }
}

/// Whether an error a provider reports in the middle of a stream is worth another try, from its
/// `kind` (a type or code such as "overloaded_error", "rate_limit_exceeded", 503).
pub(crate) fn transient(kind: &str) -> bool {
    let k = kind.to_lowercase();
    if let Ok(code) = k.parse::<u16>() {
        return matches!(code, 408 | 409 | 425 | 429) || code >= 500;
    }
    ["overloaded", "rate_limit", "ratelimit", "server_error", "api_error", "timeout", "unavailable", "internal"].iter().any(|p| k.contains(p))
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
    fn a_conversation_that_doesnt_fit_is_recognised_in_each_providers_words() {
        for m in [
            "400 This model's maximum context length is 128000 tokens. However, your messages resulted in 130812 tokens. Please reduce the length of the messages.",
            "400 prompt is too long: 215234 tokens > 200000 maximum",
            "400 input length and `max_tokens` exceed context limit: 190000 + 16000 > 200000, decrease input length or `max_tokens` and try again",
            "400 Your input exceeds the context window of this model. Please adjust your input and try again.",
            "400 the request exceeds the available context size, try increasing it",
            "400 This endpoint's maximum context length is 65536 tokens. However, you requested about 70211 tokens",
            "400 The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
            "413 Request too large",
            "400 Invalid request: Your request exceeded model token limit: 262144",
        ] {
            assert!(is_overflow(&Error::Provider(m.into())), "{m}");
        }
        for m in ["401 invalid x-api-key", "429 rate limited", "400 tools.0.input_schema: invalid", "500 internal error", "404 model not found"] {
            assert!(!is_overflow(&Error::Provider(m.into())), "{m}");
        }
        // a broken connection is no verdict on the conversation
        assert!(!is_overflow(&Error::Io("maximum context length".into())));
    }

    #[test]
    fn which_errors_in_a_stream_are_worth_another_try() {
        for kind in ["overloaded_error", "rate_limit_error", "api_error", "server_error", "rate_limit_exceeded", "timeout_error", "service_unavailable", "429", "502", "529"] {
            assert!(transient(kind), "{kind}");
        }
        for kind in ["invalid_request_error", "authentication_error", "content_filter", "400", "401", "404", ""] {
            assert!(!transient(kind), "{kind}");
        }
    }

    #[test]
    fn model_list_reads_context_windows() {
        let v = serde_json::json!({ "data": [
            { "id": "openrouter", "context_length": 1048576 },
            { "id": "anthropic", "max_input_tokens": 200000 },
            { "id": "lmstudio", "max_context_length": 32768 },
            { "id": "nested", "top_provider": { "context_length": 64000 } },
            { "id": "llamacpp", "object": "model", "meta": { "n_ctx": 196608 } },
            { "id": "unknown" },
        ] });
        let got: Vec<_> = models(&v).into_iter().map(|m| (m.id, m.context)).collect();
        assert!(got.contains(&("openrouter".into(), Some(1048576))));
        assert!(got.contains(&("anthropic".into(), Some(200000))));
        assert!(got.contains(&("lmstudio".into(), Some(32768))));
        assert!(got.contains(&("nested".into(), Some(64000))));
        assert!(got.contains(&("llamacpp".into(), Some(196608))));
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
