//! OpenAI-compatible Chat Completions (`POST {base}/chat/completions`, streamed).
use super::sse::Sse;
use super::{Delta, LlmConfig, Reply, Request, Retry, http_error, transport, url};
use crate::message::{Block, Message, Role, Usage};
use crate::{Error, Result};
use serde_json::{Value, json};

pub(super) async fn send(http: &reqwest::Client, cfg: &LlmConfig, req: &Request<'_>) -> std::result::Result<reqwest::Response, Retry> {
    let mut b = http.post(url(&cfg.base_url, "chat/completions")).json(&body(cfg, req));
    if let Some(k) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        b = b.bearer_auth(k);
    }
    let resp = b.send().await.map_err(transport)?;
    if resp.status().is_success() { Ok(resp) } else { Err(http_error(resp).await) }
}

pub(super) fn body(cfg: &LlmConfig, req: &Request<'_>) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": req.system })];
    for m in req.messages {
        messages.extend(wire_message(m));
    }
    let mut b = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": cfg.max_tokens,
    });
    // OpenAI routes requests with the same key to the same cache (others ignore unknown fields
    // inconsistently, so only send it there).
    if let Some(key) = req.cache_key.filter(|_| cfg.base_url.contains("api.openai.com")) {
        b["prompt_cache_key"] = Value::String(key.to_string());
    }
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input_schema } }))
            .collect();
    }
    b
}

/// One of our messages as Chat Completions messages (tool results are separate `tool` messages).
fn wire_message(m: &Message) -> Vec<Value> {
    match m.role {
        Role::User => {
            let mut out: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolResult { tool_use_id, content, is_error } => {
                        let content = if *is_error { format!("Error: {content}") } else { content.clone() };
                        Some(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content }))
                    }
                    _ => None,
                })
                .collect();
            let text = m.text();
            let images: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::Image { media_type, data } => Some(json!({ "type": "image_url", "image_url": { "url": format!("data:{media_type};base64,{data}") } })),
                    _ => None,
                })
                .collect();
            if !images.is_empty() {
                // Tool messages carry only text: images from tools follow them in a user message.
                let text = if text.is_empty() { "The images from the tool results above.".to_string() } else { text };
                let mut parts = vec![json!({ "type": "text", "text": text })];
                parts.extend(images);
                out.push(json!({ "role": "user", "content": parts }));
            } else if !text.is_empty() {
                out.push(json!({ "role": "user", "content": text }));
            }
            out
        }
        Role::Assistant => {
            let calls: Vec<Value> = m
                .tool_uses()
                .map(|(id, name, input)| json!({ "id": id, "type": "function", "function": { "name": name, "arguments": input.to_string() } }))
                .collect();
            let text = m.text();
            // Reasoning isn't sent back: DeepSeek rejects it in the input, others ignore it.
            let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) } });
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            vec![msg]
        }
    }
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

/// Splits `<think>…</think>` (models that put reasoning in the content) from the answer.
#[derive(Default)]
struct ThinkTags {
    inside: bool,
    started: bool,
    pending: String,
}

impl ThinkTags {
    /// Returns (thinking, text) pieces of `chunk`.
    fn feed(&mut self, chunk: &str) -> (String, String) {
        self.pending.push_str(chunk);
        let (mut think, mut text) = (String::new(), String::new());
        loop {
            if !self.started {
                let t = self.pending.trim_start();
                if t.is_empty() || "<think>".starts_with(t) {
                    return (think, text); // wait for more
                }
                self.started = true;
                if let Some(rest) = t.strip_prefix("<think>") {
                    self.inside = true;
                    self.pending = rest.to_string();
                }
            }
            if !self.inside {
                text.push_str(&std::mem::take(&mut self.pending));
                return (think, text);
            }
            match self.pending.find("</think>") {
                Some(i) => {
                    think.push_str(&self.pending[..i]);
                    self.pending = self.pending[i + 8..].trim_start().to_string();
                    self.inside = false;
                }
                None => {
                    // keep a possible partial "</think>" for the next chunk
                    let keep = (1..8).rev().find(|n| self.pending.ends_with(&"</think>"[..*n])).unwrap_or(0);
                    let cut = self.pending.len() - keep;
                    think.push_str(&self.pending[..cut]);
                    self.pending = self.pending[cut..].to_string();
                    return (think, text);
                }
            }
        }
    }
}

pub(super) async fn read(resp: reqwest::Response, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply> {
    let mut sse = Sse::new(resp);
    let mut parser = Parser::default();
    while let Some(data) = sse.next().await? {
        if data.trim() == "[DONE]" {
            break;
        }
        let v: Value = serde_json::from_str(&data).map_err(|e| Error::Provider(format!("bad stream event: {e}")))?;
        parser.event(&v, on)?;
    }
    Ok(parser.finish())
}

#[derive(Default)]
pub(super) struct Parser {
    text: String,
    thinking: String,
    calls: Vec<Call>,
    tags: ThinkTags,
    usage: Usage,
    truncated: bool,
}

impl Parser {
    pub(super) fn event(&mut self, v: &Value, on: &mut (dyn FnMut(Delta) + Send)) -> Result<()> {
        if let Some(err) = v.get("error") {
            let msg = err.get("message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| err.to_string());
            return Err(Error::Provider(msg));
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            // Cached input: OpenAI / GLM `prompt_tokens_details.cached_tokens`, DeepSeek
            // `prompt_cache_hit_tokens`, Kimi `cached_tokens`.
            let cached = n("/prompt_tokens_details/cached_tokens").max(n("/prompt_cache_hit_tokens")).max(n("/cached_tokens"));
            self.usage = Usage { input_tokens: n("/prompt_tokens"), output_tokens: n("/completion_tokens"), cache_read_tokens: cached };
        }
        let Some(choice) = v.pointer("/choices/0") else { return Ok(()) };
        if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
            self.truncated = true;
        }
        let Some(d) = choice.get("delta") else { return Ok(()) };
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = d.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                self.thinking.push_str(r);
                on(Delta::Thinking(r.to_string()));
            }
        }
        if let Some(c) = d.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let (think, text) = self.tags.feed(c);
            if !think.is_empty() {
                self.thinking.push_str(&think);
                on(Delta::Thinking(think));
            }
            if !text.is_empty() {
                self.text.push_str(&text);
                on(Delta::Text(text));
            }
        }
        for tc in d.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let i = tc.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(self.calls.len().saturating_sub(1));
            while self.calls.len() <= i {
                self.calls.push(Call::default());
            }
            let call = &mut self.calls[i];
            if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                call.id = id.to_string();
            }
            if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                let first = call.name.is_empty();
                call.name.push_str(name);
                if first {
                    on(Delta::ToolStart { id: call.id.clone(), name: call.name.clone() });
                }
            }
            if let Some(args) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                call.args.push_str(args);
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Reply {
        // text that was held back waiting for a "<think>" that never came
        if !self.tags.inside && !self.tags.pending.is_empty() {
            self.text.push_str(&self.tags.pending);
        }
        let mut content = Vec::new();
        if !self.thinking.trim().is_empty() {
            content.push(Block::Thinking { text: self.thinking, signature: None });
        }
        if !self.text.trim().is_empty() {
            content.push(Block::Text { text: self.text });
        }
        for (n, c) in self.calls.into_iter().enumerate().filter(|(_, c)| !c.name.is_empty()) {
            let input = if c.args.trim().is_empty() { Value::Object(Default::default()) } else { serde_json::from_str(&c.args).unwrap_or_else(|_| json!({ "_unparsed_arguments": c.args })) };
            let id = if c.id.is_empty() { format!("call_{n}_{}", uuid::Uuid::new_v4().simple()) } else { c.id };
            content.push(Block::ToolUse { id, name: c.name, input });
        }
        Reply { content, usage: self.usage, truncated: self.truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[Value]) -> (Reply, Vec<Delta>) {
        let mut p = Parser::default();
        let mut deltas = Vec::new();
        for e in events {
            p.event(e, &mut |d| deltas.push(d)).unwrap();
        }
        (p.finish(), deltas)
    }

    #[test]
    fn streamed_text_reasoning_and_split_tool_call() {
        let (r, d) = run(&[
            json!({ "choices": [{ "delta": { "reasoning_content": "Look at " } }] }),
            json!({ "choices": [{ "delta": { "reasoning_content": "the files." } }] }),
            json!({ "choices": [{ "delta": { "content": "Checking." } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_1", "function": { "name": "glob", "arguments": "{\"pat" } }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "tern\":\"*.rs\"}" } }] } }] }),
            json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
            json!({ "choices": [], "usage": { "prompt_tokens": 120, "completion_tokens": 30, "prompt_tokens_details": { "cached_tokens": 100 } } }),
        ]);
        assert_eq!(r.content, vec![
            Block::Thinking { text: "Look at the files.".into(), signature: None },
            Block::Text { text: "Checking.".into() },
            Block::ToolUse { id: "call_1".into(), name: "glob".into(), input: json!({ "pattern": "*.rs" }) },
        ]);
        assert_eq!(r.usage, Usage { input_tokens: 120, output_tokens: 30, cache_read_tokens: 100 });
        assert!(d.contains(&Delta::ToolStart { id: "call_1".into(), name: "glob".into() }));
    }

    #[test]
    fn cache_fields_of_each_provider() {
        let usage = |u: Value| run(&[json!({ "choices": [], "usage": u })]).0.usage.cache_read_tokens;
        assert_eq!(usage(json!({ "prompt_tokens": 100, "prompt_tokens_details": { "cached_tokens": 90 } })), 90);
        assert_eq!(usage(json!({ "prompt_tokens": 100, "prompt_cache_hit_tokens": 95, "prompt_cache_miss_tokens": 5 })), 95);
        assert_eq!(usage(json!({ "prompt_tokens": 100, "cached_tokens": 80 })), 80);
    }

    #[test]
    fn think_tags_in_content_become_thinking() {
        let (r, _) = run(&[
            json!({ "choices": [{ "delta": { "content": "<thi" } }] }),
            json!({ "choices": [{ "delta": { "content": "nk>plan it</th" } }] }),
            json!({ "choices": [{ "delta": { "content": "ink>\n\nDone." } }] }),
        ]);
        assert_eq!(r.content, vec![Block::Thinking { text: "plan it".into(), signature: None }, Block::Text { text: "Done.".into() }]);
    }

    #[test]
    fn tool_results_become_tool_messages() {
        let msgs = [
            Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "c1".into(), name: "read".into(), input: json!({ "path": "a" }) }] },
            Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c1".into(), content: "no such file".into(), is_error: true }] },
        ];
        let cfg = LlmConfig { protocol: super::super::Protocol::OpenaiChat, base_url: "x".into(), api_key: None, model: "m".into(), max_tokens: 100 };
        let b = body(&cfg, &Request { system: "sys", messages: &msgs, tools: &[], cache_key: None });
        assert_eq!(b["messages"][1]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(b["messages"][2], json!({ "role": "tool", "tool_call_id": "c1", "content": "Error: no such file" }));
        assert!(b.get("tools").is_none());
    }

    #[test]
    fn a_tools_image_follows_the_tool_messages() {
        let msgs = [
            Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "c1".into(), name: "read".into(), input: json!({ "path": "a.png" }) }] },
            Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c1".into(), content: "a.png".into(), is_error: false }, Block::Image { media_type: "image/png".into(), data: "AAAA".into() }] },
        ];
        let cfg = LlmConfig { protocol: super::super::Protocol::OpenaiChat, base_url: "x".into(), api_key: None, model: "m".into(), max_tokens: 100 };
        let b = body(&cfg, &Request { system: "sys", messages: &msgs, tools: &[], cache_key: None });
        assert_eq!(b["messages"][2], json!({ "role": "tool", "tool_call_id": "c1", "content": "a.png" }));
        assert_eq!(b["messages"][3]["role"], "user");
        assert_eq!(b["messages"][3]["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert!(!b["messages"][3]["content"][0]["text"].as_str().unwrap().is_empty());
    }
}
