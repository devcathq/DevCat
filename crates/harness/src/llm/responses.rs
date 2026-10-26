//! OpenAI Responses API (`POST {base}/responses`, streamed): OpenAI's newer models, including the
//! ones only offered there (codex). Stateless (`store: false`): the whole conversation goes with
//! every request, like the other adapters.
use super::sse::Sse;
use super::{Delta, LlmConfig, Reply, Request, Retry, http_error, transport, url};
use crate::message::{Block, Message, Role, Usage};
use crate::{Error, Result};
use serde_json::{Value, json};

pub(super) async fn send(http: &reqwest::Client, cfg: &LlmConfig, req: &Request<'_>) -> std::result::Result<reqwest::Response, Retry> {
    let mut b = http.post(url(&cfg.base_url, "responses")).json(&body(cfg, req));
    if let Some(k) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        b = b.bearer_auth(k);
    }
    let resp = b.send().await.map_err(transport)?;
    if resp.status().is_success() { Ok(resp) } else { Err(http_error(resp).await) }
}

/// Reasoning models take a `reasoning` setting (we ask for summaries, shown as thinking); the
/// others reject it, and the API doesn't say which is which.
fn reasons(model: &str) -> bool {
    let m = model.to_lowercase();
    let m = m.rsplit('/').next().unwrap_or(&m);
    m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("codex") || (m.starts_with("gpt-5") && !m.contains("chat"))
}

pub(super) fn body(cfg: &LlmConfig, req: &Request<'_>) -> Value {
    let input: Vec<Value> = req.messages.iter().flat_map(wire_items).collect();
    let mut b = json!({
        "model": cfg.model,
        "instructions": req.system,
        "input": input,
        "stream": true,
        "store": false,
        "max_output_tokens": cfg.max_tokens,
    });
    if reasons(&cfg.model) {
        b["reasoning"] = json!({ "summary": "auto" });
    }
    if let Some(key) = req.cache_key.filter(|_| cfg.base_url.contains("api.openai.com")) {
        b["prompt_cache_key"] = Value::String(key.to_string());
    }
    if !req.tools.is_empty() {
        // functions are strict by default here (unlike Chat Completions): every property required
        // and no others allowed, which our schemas with optional properties, and MCP's, aren't
        b["tools"] = req.tools.iter().map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.input_schema, "strict": false })).collect();
    }
    b
}

/// One of our messages as Responses input items: messages, `function_call` and
/// `function_call_output`. Reasoning isn't sent back (it would need the encrypted copy).
fn wire_items(m: &Message) -> Vec<Value> {
    match m.role {
        Role::User => {
            let mut out: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolResult { tool_use_id, content, is_error } => {
                        let output = if *is_error { format!("Error: {content}") } else { content.clone() };
                        Some(json!({ "type": "function_call_output", "call_id": tool_use_id, "output": output }))
                    }
                    _ => None,
                })
                .collect();
            let text = m.text();
            let images: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::Image { media_type, data } => Some(json!({ "type": "input_image", "image_url": format!("data:{media_type};base64,{data}") })),
                    _ => None,
                })
                .collect();
            if !images.is_empty() {
                let mut parts: Vec<Value> = if text.is_empty() { Vec::new() } else { vec![json!({ "type": "input_text", "text": text })] };
                parts.extend(images);
                out.push(json!({ "role": "user", "content": parts }));
            } else if !text.is_empty() {
                out.push(json!({ "role": "user", "content": text }));
            }
            out
        }
        Role::Assistant => {
            let mut out = Vec::new();
            let text = m.text();
            if !text.is_empty() {
                out.push(json!({ "role": "assistant", "content": text }));
            }
            out.extend(m.tool_uses().map(|(id, name, input)| json!({ "type": "function_call", "call_id": id, "name": name, "arguments": input.to_string() })));
            out
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
struct Call {
    item_id: String,
    call_id: String,
    name: String,
    args: String,
}

#[derive(Default)]
pub(super) struct Parser {
    text: String,
    thinking: String,
    calls: Vec<Call>,
    usage: Usage,
    truncated: bool,
}

impl Parser {
    pub(super) fn event(&mut self, v: &Value, on: &mut (dyn FnMut(Delta) + Send)) -> Result<()> {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or_default();
        match s("/type") {
            "response.output_text.delta" => {
                let d = s("/delta");
                if !d.is_empty() {
                    self.text.push_str(d);
                    on(Delta::Text(d.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let d = s("/delta");
                if !d.is_empty() {
                    self.thinking.push_str(d);
                    on(Delta::Thinking(d.to_string()));
                }
            }
            // a new summary part reads as a new paragraph
            "response.reasoning_summary_part.added" if !self.thinking.is_empty() => {
                self.thinking.push_str("\n\n");
                on(Delta::Thinking("\n\n".into()));
            }
            "response.output_item.added" if s("/item/type") == "function_call" => {
                let call = Call { item_id: s("/item/id").into(), call_id: s("/item/call_id").into(), name: s("/item/name").into(), args: s("/item/arguments").into() };
                on(Delta::ToolStart { id: call.call_id.clone(), name: call.name.clone() });
                self.calls.push(call);
            }
            "response.function_call_arguments.delta" => {
                let id = s("/item_id");
                if let Some(c) = self.calls.iter_mut().rev().find(|c| c.item_id == id || id.is_empty()) {
                    c.args.push_str(s("/delta"));
                }
            }
            // the finished item is the authority on the arguments (and the ids), when it has them
            "response.output_item.done" if s("/item/type") == "function_call" => {
                let id = s("/item/id");
                if let Some(c) = self.calls.iter_mut().find(|c| c.item_id == id) {
                    if !s("/item/arguments").is_empty() {
                        c.args = s("/item/arguments").to_string();
                    }
                    if !s("/item/call_id").is_empty() {
                        c.call_id = s("/item/call_id").to_string();
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(u) = v.pointer("/response/usage") {
                    let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                    self.usage = Usage { input_tokens: n("/input_tokens"), output_tokens: n("/output_tokens"), cache_read_tokens: n("/input_tokens_details/cached_tokens") };
                }
                match s("/response/incomplete_details/reason") {
                    "" => {}
                    "max_output_tokens" => self.truncated = true,
                    // content_filter and the like: say so, rather than end as if it had answered
                    reason => return Err(Error::Provider(format!("The model stopped before finishing its answer ({reason})."))),
                }
            }
            "response.failed" => {
                let msg = v.pointer("/response/error/message").and_then(Value::as_str).unwrap_or("the response failed");
                return Err(Error::Provider(msg.to_string()));
            }
            "error" => {
                let msg = v.get("message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| v.to_string());
                return Err(Error::Provider(msg));
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Reply {
        let mut content = Vec::new();
        if !self.thinking.trim().is_empty() {
            content.push(Block::Thinking { text: self.thinking, signature: None });
        }
        if !self.text.trim().is_empty() {
            content.push(Block::Text { text: self.text });
        }
        for (n, c) in self.calls.into_iter().enumerate().filter(|(_, c)| !c.name.is_empty()) {
            let input = if c.args.trim().is_empty() { Value::Object(Default::default()) } else { serde_json::from_str(&c.args).unwrap_or_else(|_| json!({ "_unparsed_arguments": c.args })) };
            let id = if c.call_id.is_empty() { format!("call_{n}_{}", uuid::Uuid::new_v4().simple()) } else { c.call_id };
            content.push(Block::ToolUse { id, name: c.name, input });
        }
        Reply { content, usage: self.usage, truncated: self.truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolSpec;

    fn run(events: &[Value]) -> Result<(Reply, Vec<Delta>)> {
        let mut p = Parser::default();
        let mut deltas = Vec::new();
        for e in events {
            p.event(e, &mut |d| deltas.push(d))?;
        }
        Ok((p.finish(), deltas))
    }

    fn cfg(model: &str, base: &str) -> LlmConfig {
        LlmConfig { protocol: super::super::Protocol::OpenaiResponses, base_url: base.into(), api_key: None, model: model.into(), max_tokens: 1000 }
    }

    #[test]
    fn streamed_reasoning_text_and_a_tool_call() {
        let (r, d) = run(&[
            json!({ "type": "response.created", "response": { "id": "resp_1" } }),
            json!({ "type": "response.reasoning_summary_part.added", "summary_index": 0 }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Find the file." }),
            json!({ "type": "response.reasoning_summary_part.added", "summary_index": 1 }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Then read it." }),
            json!({ "type": "response.output_text.delta", "delta": "Checking " }),
            json!({ "type": "response.output_text.delta", "delta": "now." }),
            json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "glob", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"pat" }),
            json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "tern\":\"*.rs\"}" }),
            json!({ "type": "response.output_item.done", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "glob", "arguments": "{\"pattern\":\"*.rs\"}" } }),
            json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 120, "output_tokens": 30, "input_tokens_details": { "cached_tokens": 100 } } } }),
        ])
        .unwrap();
        assert_eq!(r.content, vec![
            Block::Thinking { text: "Find the file.\n\nThen read it.".into(), signature: None },
            Block::Text { text: "Checking now.".into() },
            Block::ToolUse { id: "call_A".into(), name: "glob".into(), input: json!({ "pattern": "*.rs" }) },
        ]);
        assert_eq!(r.usage, Usage { input_tokens: 120, output_tokens: 30, cache_read_tokens: 100 });
        assert!(!r.truncated);
        assert!(d.contains(&Delta::ToolStart { id: "call_A".into(), name: "glob".into() }));
    }

    #[test]
    fn cut_off_and_failed_responses() {
        let (r, _) = run(&[json!({ "type": "response.incomplete", "response": { "incomplete_details": { "reason": "max_output_tokens" }, "usage": { "input_tokens": 5, "output_tokens": 9 } } })]).unwrap();
        assert!(r.truncated);
        let failed = run(&[json!({ "type": "response.failed", "response": { "error": { "code": "server_error", "message": "boom" } } })]).err().unwrap();
        assert!(failed.to_string().contains("boom"));
        let error = run(&[json!({ "type": "error", "code": "rate_limit_exceeded", "message": "slow down" })]).err().unwrap();
        assert!(error.to_string().contains("slow down"));
        let filtered = run(&[json!({ "type": "response.incomplete", "response": { "incomplete_details": { "reason": "content_filter" } } })]).err().unwrap();
        assert!(filtered.to_string().contains("content_filter"), "{filtered}");
        let (done, _) = run(&[json!({ "type": "response.completed", "response": { "incomplete_details": null, "usage": { "input_tokens": 1, "output_tokens": 1 } } })]).unwrap();
        assert!(!done.truncated);
    }

    #[test]
    fn a_finished_call_without_arguments_keeps_the_streamed_ones() {
        let (r, _) = run(&[
            json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "glob", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"pattern\":\"*.rs\"}" }),
            json!({ "type": "response.output_item.done", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "glob" } }),
        ])
        .unwrap();
        assert_eq!(r.content, vec![Block::ToolUse { id: "call_A".into(), name: "glob".into(), input: json!({ "pattern": "*.rs" }) }]);
    }

    #[test]
    fn conversation_becomes_input_items() {
        let msgs = [
            Message::user_text("what's in a?"),
            Message { role: Role::Assistant, content: vec![Block::Thinking { text: "hm".into(), signature: None }, Block::Text { text: "Reading.".into() }, Block::ToolUse { id: "c1".into(), name: "read".into(), input: json!({ "path": "a" }) }] },
            Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c1".into(), content: "no such file".into(), is_error: true }] },
            Message { role: Role::User, content: vec![Block::Text { text: "and this?".into() }, Block::Image { media_type: "image/png".into(), data: "AAAA".into() }] },
        ];
        let tools = [ToolSpec { name: "read".into(), description: "Read".into(), input_schema: json!({ "type": "object" }) }];
        let b = body(&cfg("gpt-5-codex", "https://api.openai.com/v1"), &Request { system: "sys", messages: &msgs, tools: &tools, cache_key: Some("s1") });
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["store"], false);
        assert_eq!(b["input"], json!([
            { "role": "user", "content": "what's in a?" },
            { "role": "assistant", "content": "Reading." },
            { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "{\"path\":\"a\"}" },
            { "type": "function_call_output", "call_id": "c1", "output": "Error: no such file" },
            { "role": "user", "content": [{ "type": "input_text", "text": "and this?" }, { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }] },
        ]));
        assert_eq!(b["tools"], json!([{ "type": "function", "name": "read", "description": "Read", "parameters": { "type": "object" }, "strict": false }]));
        // only an image: no empty text part
        let image = [Message { role: Role::User, content: vec![Block::Image { media_type: "image/png".into(), data: "AAAA".into() }] }];
        let only = body(&cfg("gpt-5", "https://api.openai.com/v1"), &Request { system: "sys", messages: &image, tools: &[], cache_key: None });
        assert_eq!(only["input"], json!([{ "role": "user", "content": [{ "type": "input_image", "image_url": "data:image/png;base64,AAAA" }] }]));
        assert_eq!(b["reasoning"], json!({ "summary": "auto" }));
        assert_eq!(b["prompt_cache_key"], "s1");
        let plain = body(&cfg("gpt-4.1", "https://gateway.example/v1"), &Request { system: "sys", messages: &msgs[..1], tools: &[], cache_key: Some("s1") });
        assert!(plain.get("reasoning").is_none() && plain.get("prompt_cache_key").is_none() && plain.get("tools").is_none());
    }

    #[test]
    fn which_models_reason() {
        for m in ["o3", "o4-mini", "gpt-5", "gpt-5-mini", "gpt-5-codex", "openai/o1", "codex-mini-latest"] {
            assert!(reasons(m), "{m}");
        }
        for m in ["gpt-4.1", "gpt-4o", "gpt-5-chat-latest"] {
            assert!(!reasons(m), "{m}");
        }
    }
}
