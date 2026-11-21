//! Anthropic Messages API (`POST {base}/v1/messages`, streamed).
use super::sse::Sse;
use super::{Effort, Broken, Delta, LlmConfig, Reply, Request, Retry, UNPARSED, http_error, transient, transport, url};
use crate::message::{Block, Message, Role, Usage};
use crate::{Error, Result};
use serde_json::{Value, json};

const VERSION: &str = "2023-06-01";

pub(super) async fn send(http: &reqwest::Client, cfg: &LlmConfig, req: &Request<'_>) -> std::result::Result<reqwest::Response, Retry> {
    // accept both "https://api.anthropic.com" and ".../v1"
    let path = if cfg.base_url.trim_end_matches('/').ends_with("/v1") { "messages" } else { "v1/messages" };
    let mut b = http.post(url(&cfg.base_url, path)).header("anthropic-version", VERSION).json(&body(cfg, req));
    if let Some(k) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        b = b.header("x-api-key", k);
    }
    let resp = b.send().await.map_err(transport)?;
    if resp.status().is_success() { Ok(resp) } else { Err(http_error(resp).await) }
}

pub(super) fn body(cfg: &LlmConfig, req: &Request<'_>) -> Value {
    let mut b = json!({
        "model": cfg.model,
        "max_tokens": cfg.max_tokens,
        // The system prompt and tools are the same every step: cache them.
        "system": [{ "type": "text", "text": req.system, "cache_control": { "type": "ephemeral" } }],
        "messages": req.messages.iter().map(wire_message).collect::<Vec<_>>(),
        "stream": true,
    });
    // A rolling breakpoint on the newest block: the next step reads the whole conversation so far
    // from the cache, and only the new tool results are fresh input.
    if let Some(last) = b["messages"].as_array_mut().and_then(|m| m.last_mut()).and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut()) {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }
    if !req.tools.is_empty() {
        b["tools"] = req.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })).collect();
    }
    if let Some(e) = cfg.effort {
        match claude_version(&cfg.model) {
            // adaptive thinking, its depth set by effort (`xhigh` came with 4.7)
            Some(v) if v >= (4, 6) => {
                b["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
                let e = if v < (4, 7) && e == Effort::Xhigh { Effort::High } else { e };
                b["output_config"] = json!({ "effort": e.as_str() });
            }
            // older Claude models and servers that speak the protocol: a thinking budget, which
            // must be at least 1,024 tokens and leave room for the answer
            _ => {
                let want: u32 = match e {
                    Effort::Low => 2_048,
                    Effort::Medium => 4_096,
                    Effort::High => 8_192,
                    Effort::Xhigh | Effort::Max => 10_000,
                };
                let budget = want.min(cfg.max_tokens.saturating_sub(2_048));
                if budget >= 1_024 {
                    b["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
                }
            }
        }
    }
    b
}

/// The (major, minor) version of a Claude model id (`claude-opus-4-6`, `claude-sonnet-4-20250514`,
/// `anthropic/claude-opus-4.6`); Fable and Mythos models count as the newest. None for others.
fn claude_version(model: &str) -> Option<(u32, u32)> {
    let m = model.to_lowercase();
    if m.contains("claude") && (m.contains("fable") || m.contains("mythos")) {
        return Some((99, 0));
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"claude-(?:opus|sonnet|haiku)-(\d+)(?:[-.](\d{1,2})(?:\D|$))?").expect("valid regex"));
    let c = re.captures(&m)?;
    Some((c[1].parse().ok()?, c.get(2).and_then(|n| n.as_str().parse().ok()).unwrap_or(0)))
}

fn wire_message(m: &Message) -> Value {
    let content: Vec<Value> = m
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } if !text.is_empty() => Some(json!({ "type": "text", "text": text })),
            Block::Text { .. } => None,
            // Thinking goes back only with its signature (Anthropic verifies it).
            Block::Thinking { text, signature: Some(sig) } => Some(json!({ "type": "thinking", "thinking": text, "signature": sig })),
            Block::Thinking { .. } => None,
            Block::ToolUse { id, name, input } => Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input })),
            Block::ToolResult { tool_use_id, content, is_error } => {
                Some(json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error }))
            }
            Block::Image { media_type, data } => Some(json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data } })),
        })
        .collect();
    json!({ "role": match m.role { Role::User => "user", Role::Assistant => "assistant" }, "content": content })
}

enum Part {
    Text(String),
    Thinking(String, Option<String>),
    Tool { id: String, name: String, json: String },
}

pub(super) async fn read(resp: reqwest::Response, on: &mut (dyn FnMut(Delta) + Send)) -> std::result::Result<Reply, Broken> {
    let mut sse = Sse::new(resp);
    let mut parser = Parser::default();
    let mut finished = false;
    loop {
        let data = match sse.next().await {
            Ok(Some(data)) => data,
            Ok(None) => break,
            // the message had its stop reason: whatever went wrong after it doesn't take it back
            Err(_) if parser.stopped => break,
            Err(e) => return Err(Broken::Retry(e)),
        };
        // an event that isn't JSON is a stream that broke in the middle of one (or, after the
        // stop reason, a compatible API's own way of ending, such as `[DONE]`)
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) if parser.stopped => break,
            Err(e) => return Err(Broken::Retry(Error::Provider(format!("bad stream event: {e}")))),
        };
        match parser.event(&v, on) {
            Ok(true) => {
                finished = true;
                break;
            }
            Ok(false) => {}
            Err(e) => return Err(if parser.transient { Broken::Retry(e) } else { Broken::Never(e) }),
        }
    }
    // a stop reason is the end too (compatible APIs don't all send `message_stop`)
    let finished = finished || parser.stopped;
    let reply = parser.finish();
    if finished { Ok(reply) } else { Err(Broken::Incomplete(reply)) }
}

#[derive(Default)]
pub(super) struct Parser {
    parts: Vec<(u64, Part)>,
    usage: Usage,
    truncated: bool,
    /// The message got its stop reason.
    pub(super) stopped: bool,
    /// The error it stopped with is worth another try.
    pub(super) transient: bool,
}

impl Parser {
    fn part(&mut self, index: u64) -> Option<&mut Part> {
        self.parts.iter_mut().find(|(i, _)| *i == index).map(|(_, p)| p)
    }

    /// Handle one event; true at the end of the message.
    pub(super) fn event(&mut self, v: &Value, on: &mut (dyn FnMut(Delta) + Send)) -> Result<bool> {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or_default().to_string();
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        let index = n("/index");
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "message_start" => {
                let read = n("/message/usage/cache_read_input_tokens");
                self.usage.input_tokens = n("/message/usage/input_tokens") + read + n("/message/usage/cache_creation_input_tokens");
                self.usage.cache_read_tokens = read;
                self.usage.output_tokens = n("/message/usage/output_tokens");
            }
            "content_block_start" => {
                let part = match s("/content_block/type").as_str() {
                    "text" => Part::Text(s("/content_block/text")),
                    "thinking" => Part::Thinking(s("/content_block/thinking"), None),
                    "tool_use" => {
                        let (id, name) = (s("/content_block/id"), s("/content_block/name"));
                        on(Delta::ToolStart { id: id.clone(), name: name.clone() });
                        Part::Tool { id, name, json: String::new() }
                    }
                    _ => return Ok(false), // redacted thinking, server tools: not used
                };
                self.parts.push((index, part));
            }
            "content_block_delta" => {
                let kind = s("/delta/type");
                let Some(part) = self.part(index) else { return Ok(false) };
                match (kind.as_str(), part) {
                    ("text_delta", Part::Text(t)) => {
                        let d = s("/delta/text");
                        t.push_str(&d);
                        on(Delta::Text(d));
                    }
                    ("thinking_delta", Part::Thinking(t, _)) => {
                        let d = s("/delta/thinking");
                        t.push_str(&d);
                        on(Delta::Thinking(d));
                    }
                    ("signature_delta", Part::Thinking(_, sig)) => sig.get_or_insert_with(String::new).push_str(&s("/delta/signature")),
                    ("input_json_delta", Part::Tool { json, .. }) => json.push_str(&s("/delta/partial_json")),
                    _ => {}
                }
            }
            "message_delta" => {
                let reason = s("/delta/stop_reason");
                self.stopped |= !reason.is_empty();
                if reason == "max_tokens" {
                    self.truncated = true;
                }
                if v.pointer("/usage/output_tokens").is_some() {
                    self.usage.output_tokens = n("/usage/output_tokens");
                }
                // Anthropic (newer) and compatible APIs such as Z.ai send the input count here too,
                // sometimes only here (message_start then says 0).
                if n("/usage/input_tokens") > 0 {
                    let read = n("/usage/cache_read_input_tokens");
                    self.usage.input_tokens = n("/usage/input_tokens") + read + n("/usage/cache_creation_input_tokens");
                    self.usage.cache_read_tokens = read;
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                // overloaded_error, rate_limit_error, api_error: the same request can work a moment later
                self.transient = transient(&s("/error/type"));
                let msg = s("/error/message");
                return Err(Error::Provider(if msg.is_empty() { v.to_string() } else { msg }));
            }
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn finish(self) -> Reply {
        let content = self
            .parts
            .into_iter()
            .filter_map(|(_, p)| match p {
                Part::Text(t) if !t.trim().is_empty() => Some(Block::Text { text: t }),
                Part::Text(_) => None,
                Part::Thinking(t, sig) => Some(Block::Thinking { text: t, signature: sig }),
                Part::Tool { id, name, json } => {
                    let input = if json.trim().is_empty() { json!({}) } else { serde_json::from_str(&json).unwrap_or_else(|_| json!({ UNPARSED: json })) };
                    Some(Block::ToolUse { id, name, input })
                }
            })
            .collect();
        Reply { content, usage: self.usage, truncated: self.truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_with_thinking_text_and_tool() {
        let events = [
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 50, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 0, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "Need the file." } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig==" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Reading it." } }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"path\": \"src/" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "main.rs\"}" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 42 } }),
            json!({ "type": "message_stop" }),
        ];
        let mut p = Parser::default();
        let mut ended = false;
        for e in &events {
            ended = p.event(e, &mut |_| {}).unwrap();
        }
        assert!(ended);
        let r = p.finish();
        assert_eq!(r.content, vec![
            Block::Thinking { text: "Need the file.".into(), signature: Some("sig==".into()) },
            Block::Text { text: "Reading it.".into() },
            Block::ToolUse { id: "toolu_1".into(), name: "read".into(), input: json!({ "path": "src/main.rs" }) },
        ]);
        assert_eq!(r.usage, Usage { input_tokens: 950, output_tokens: 42, cache_read_tokens: 900 });
        assert!(!r.truncated);
    }

    #[test]
    fn an_overloaded_error_in_the_stream_is_worth_another_try() {
        let mut p = Parser::default();
        let e = p.event(&json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }), &mut |_| {}).unwrap_err();
        assert_eq!(e.to_string(), "model provider: Overloaded");
        assert!(p.transient);
        let mut p = Parser::default();
        assert!(p.event(&json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "no" } }), &mut |_| {}).is_err());
        assert!(!p.transient);
        // a stop reason ends the message, with or without `message_stop` after it
        let mut p = Parser::default();
        assert!(!p.stopped);
        p.event(&json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 3 } }), &mut |_| {}).unwrap();
        assert!(p.stopped);
    }

    #[test]
    fn input_tokens_reported_at_the_end() {
        let mut p = Parser::default();
        for e in [
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 0, "output_tokens": 0 } } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" }, "usage": { "input_tokens": 14, "output_tokens": 20, "cache_read_input_tokens": 6 } }),
        ] {
            p.event(&e, &mut |_| {}).unwrap();
        }
        let r = p.finish();
        assert_eq!(r.usage, Usage { input_tokens: 20, output_tokens: 20, cache_read_tokens: 6 });
        assert!(r.truncated);
    }

    #[test]
    fn request_keeps_signed_thinking_only() {
        let msgs = [Message {
            role: Role::Assistant,
            content: vec![
                Block::Thinking { text: "a".into(), signature: Some("s".into()) },
                Block::Thinking { text: "from another provider".into(), signature: None },
                Block::ToolUse { id: "t".into(), name: "read".into(), input: json!({}) },
            ],
        }];
        let cfg = LlmConfig { protocol: super::super::Protocol::Anthropic, base_url: "x".into(), api_key: None, model: "m".into(), max_tokens: 10, effort: None };
        let b = body(&cfg, &Request { system: "s", messages: &msgs, tools: &[], cache_key: None });
        let c = b["messages"][0]["content"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["type"], "thinking");
        assert_eq!(c[1]["type"], "tool_use");
        // the newest block carries the rolling cache breakpoint
        assert_eq!(c[1]["cache_control"]["type"], "ephemeral");
        assert!(c[0].get("cache_control").is_none());
    }

    #[test]
    fn effort_is_said_the_way_each_claude_model_takes_it() {
        assert_eq!(claude_version("claude-opus-4-6"), Some((4, 6)));
        assert_eq!(claude_version("claude-sonnet-4-20250514"), Some((4, 0)));
        assert_eq!(claude_version("claude-opus-4-5-20251101"), Some((4, 5)));
        assert_eq!(claude_version("anthropic/claude-opus-4.7"), Some((4, 7)));
        assert_eq!(claude_version("claude-haiku-5-5"), Some((5, 5)));
        assert_eq!(claude_version("claude-fable-5-2"), Some((99, 0)));
        assert_eq!((claude_version("claude-3-7-sonnet-latest"), claude_version("glm-5")), (None, None));
        let body = |model: &str, effort: Option<Effort>| {
            let cfg = LlmConfig { protocol: super::super::Protocol::Anthropic, base_url: "x".into(), api_key: None, model: model.into(), max_tokens: 16_000, effort };
            super::body(&cfg, &Request { system: "s", messages: &[Message::user_text("hi")], tools: &[], cache_key: None })
        };
        // not chosen: nothing is sent about it
        let b = body("claude-opus-5-5", None);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
        let b = body("claude-opus-5-5", Some(Effort::Xhigh));
        assert_eq!((b["thinking"]["type"].as_str(), b["output_config"]["effort"].as_str()), (Some("adaptive"), Some("xhigh")));
        // 4.6 has no xhigh
        assert_eq!(body("claude-sonnet-4-6", Some(Effort::Xhigh))["output_config"]["effort"], "high");
        // older models and other servers: a budget under max_tokens
        let b = body("claude-haiku-4-5", Some(Effort::Max));
        assert_eq!((b["thinking"]["type"].as_str(), b["thinking"]["budget_tokens"].as_u64()), (Some("enabled"), Some(10_000)));
        assert!(b.get("output_config").is_none());
        assert_eq!(body("kimi-k3", Some(Effort::Low))["thinking"]["budget_tokens"], 2_048);
        assert_eq!(Effort::parse("XHigh"), Some(Effort::Xhigh));
        assert_eq!(Effort::parse("auto"), None);
    }
}
