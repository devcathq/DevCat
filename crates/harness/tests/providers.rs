//! Both adapters over real HTTP against a tiny local server that streams canned SSE responses:
//! request shape, auth headers, streaming, retries.
use devcat_harness::llm::{Delta, Request};
use devcat_harness::message::ToolSpec;
use devcat_harness::{Block, Llm, LlmConfig, Message, Protocol};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Seen {
    path: String,
    headers: String,
    body: Value,
}

/// Serve `responses` in order (status, body) and record each request.
async fn server(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // read headers, then the body by Content-Length
            let (head, body_start) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
                }
            };
            let len: usize = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
            while buf.len() < body_start + len {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let req_body: Value = serde_json::from_slice(&buf[body_start..body_start + len]).unwrap_or(Value::Null);
            let path = head.lines().next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
            seen2.lock().unwrap().push(Seen { path, headers: head.to_lowercase(), body: req_body });
            let ctype = if status == 200 { "text/event-stream" } else { "application/json" };
            let resp = format!("HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\nretry-after: 0\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}", body.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn sse(events: &[Value]) -> String {
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec { name: "read".into(), description: "Read a file".into(), input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }) }]
}

#[tokio::test]
async fn openai_compatible_streams_a_tool_call_after_a_retry() {
    let stream = sse(&[
        json!({ "choices": [{ "delta": { "role": "assistant", "content": "Reading " } }] }),
        json!({ "choices": [{ "delta": { "content": "the file." } }] }),
        json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_9", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"a.txt\"}" } }] } }] }),
        json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
        json!({ "choices": [], "usage": { "prompt_tokens": 42, "completion_tokens": 7 } }),
    ]) + "data: [DONE]\n\n";
    // first a 503 (retried), then the stream
    let (base, seen) = server(vec![(503, r#"{"error":{"message":"overloaded"}}"#.into()), (200, stream)]).await;
    let llm = Llm::new(LlmConfig { protocol: Protocol::OpenaiChat, base_url: format!("{base}/v1"), api_key: Some("sk-test".into()), model: "qwen3".into(), max_tokens: 1000, effort: None });
    let msgs = [Message::user_text("what's in a.txt?")];
    let mut text = String::new();
    let reply = llm
        .step(&Request { system: "be brief", messages: &msgs, tools: &tools(), cache_key: None }, &mut |d| {
            if let Delta::Text(t) = d {
                text.push_str(&t);
            }
        })
        .await
        .unwrap();
    assert_eq!(text, "Reading the file.");
    assert_eq!(reply.content.last(), Some(&Block::ToolUse { id: "call_9".into(), name: "read".into(), input: json!({ "path": "a.txt" }) }));
    assert_eq!(reply.usage.input_tokens, 42);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let s = &seen[1];
    assert_eq!(s.path, "/v1/chat/completions");
    assert!(s.headers.contains("authorization: bearer sk-test"));
    assert_eq!(s.body["messages"][0], json!({ "role": "system", "content": "be brief" }));
    assert_eq!(s.body["tools"][0]["function"]["name"], "read");
    assert_eq!(s.body["stream"], true);
}

#[tokio::test]
async fn anthropic_streams_and_reports_errors() {
    let stream = sse(&[
        json!({ "type": "message_start", "message": { "usage": { "input_tokens": 11, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Héllo ✓" } }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
        json!({ "type": "message_stop" }),
    ]);
    let (base, seen) = server(vec![(200, stream), (401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.into())]).await;
    let llm = Llm::new(LlmConfig { protocol: Protocol::Anthropic, base_url: base, api_key: Some("ant-key".into()), model: "claude-x".into(), max_tokens: 500, effort: None });
    let msgs = [Message::user_text("hi")];
    let reply = llm.step(&Request { system: "s", messages: &msgs, tools: &tools(), cache_key: None }, &mut |_| {}).await.unwrap();
    assert_eq!(reply.content, vec![Block::Text { text: "Héllo ✓".into() }]);
    assert_eq!((reply.usage.input_tokens, reply.usage.output_tokens), (11, 5));

    // 401 is not retried and keeps the provider's message
    let err = llm.step(&Request { system: "s", messages: &msgs, tools: &[], cache_key: None }, &mut |_| {}).await.unwrap_err();
    assert_eq!(err.to_string(), "model provider: 401 invalid x-api-key");

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1/messages");
    assert!(seen[0].headers.contains("x-api-key: ant-key") && seen[0].headers.contains("anthropic-version: 2023-06-01"));
    assert_eq!(seen[0].body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(seen[0].body["tools"][0]["input_schema"]["type"], "object");
}

#[tokio::test]
async fn openai_responses_streams_a_tool_call_after_a_retry() {
    let stream = sse(&[
        json!({ "type": "response.created", "response": { "id": "resp_1", "status": "in_progress" } }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "delta": "Reading " }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "delta": "the file." }),
        json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_7", "name": "read", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"path\":" }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "\"a.txt\"}" }),
        json!({ "type": "response.output_item.done", "item": { "type": "function_call", "id": "fc_1", "call_id": "call_7", "name": "read", "arguments": "{\"path\":\"a.txt\"}" } }),
        json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 42, "output_tokens": 7, "input_tokens_details": { "cached_tokens": 30 } } } }),
    ]);
    let (base, seen) = server(vec![(429, r#"{"error":{"message":"rate limited"}}"#.into()), (200, stream)]).await;
    let llm = Llm::new(LlmConfig { protocol: Protocol::OpenaiResponses, base_url: format!("{base}/v1"), api_key: Some("sk-test".into()), model: "gpt-5-codex".into(), max_tokens: 1000, effort: None });
    let msgs = [Message::user_text("what's in a.txt?")];
    let mut text = String::new();
    let reply = llm
        .step(&Request { system: "be brief", messages: &msgs, tools: &tools(), cache_key: None }, &mut |d| {
            if let Delta::Text(t) = d {
                text.push_str(&t);
            }
        })
        .await
        .unwrap();
    assert_eq!(text, "Reading the file.");
    assert_eq!(reply.content.last(), Some(&Block::ToolUse { id: "call_7".into(), name: "read".into(), input: json!({ "path": "a.txt" }) }));
    assert_eq!((reply.usage.input_tokens, reply.usage.cache_read_tokens), (42, 30));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let s = &seen[1];
    assert_eq!(s.path, "/v1/responses");
    assert!(s.headers.contains("authorization: bearer sk-test"));
    assert_eq!(s.body["instructions"], "be brief");
    assert_eq!(s.body["input"][0], json!({ "role": "user", "content": "what's in a.txt?" }));
    assert_eq!(s.body["tools"][0]["name"], "read");
    assert_eq!((s.body["stream"].clone(), s.body["store"].clone()), (json!(true), json!(false)));
}

// ------------------------------------------------------------------ streams that break

use std::time::Duration;

fn quick(protocol: Protocol, base: String) -> Llm {
    let base_url = if protocol == Protocol::Anthropic { base } else { format!("{base}/v1") };
    Llm::new(LlmConfig { protocol, base_url, api_key: None, model: "m".into(), max_tokens: 1000, effort: None }).with_backoff(Duration::from_millis(5))
}

/// A whole Chat Completions answer: some text and a `read` call.
fn chat_answer() -> Vec<Value> {
    vec![
        json!({ "choices": [{ "delta": { "role": "assistant", "content": "Reading " } }] }),
        json!({ "choices": [{ "delta": { "content": "the file." } }] }),
        json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_9", "type": "function", "function": { "name": "read", "arguments": "{\"path\":" } }] } }] }),
        json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "\"a.txt\"}" } }] } }] }),
        json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
    ]
}

async fn step(llm: &Llm, deltas: &mut Vec<Delta>) -> devcat_harness::Result<devcat_harness::llm::Reply> {
    let msgs = [Message::user_text("what's in a.txt?")];
    llm.step(&Request { system: "s", messages: &msgs, tools: &tools(), cache_key: None }, &mut |d| deltas.push(d)).await
}

#[tokio::test]
async fn an_answer_cut_off_in_the_middle_is_asked_for_again() {
    // the connection closes in the middle of the call's arguments, with nothing wrong on the wire
    let cut = sse(&chat_answer()[..3]);
    let whole = sse(&chat_answer()) + "data: [DONE]\n\n";
    let (base, seen) = server(vec![(200, cut), (200, whole)]).await;
    let llm = quick(Protocol::OpenaiChat, base);
    let mut deltas = Vec::new();
    let reply = step(&llm, &mut deltas).await.unwrap();
    // not half a call handed on as if it were the answer
    assert_eq!(reply.content, vec![Block::Text { text: "Reading the file.".into() }, Block::ToolUse { id: "call_9".into(), name: "read".into(), input: json!({ "path": "a.txt" }) }]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    // whoever shows the stream hears that it starts over, between the two tries
    let retry = deltas.iter().position(|d| matches!(d, Delta::Retry { attempt: 1, reason, .. } if reason.contains("before the model finished"))).expect("no retry was reported");
    assert!(deltas[..retry].contains(&Delta::Text("Reading ".into())) && deltas[retry..].contains(&Delta::Text("Reading ".into())));
}

#[tokio::test]
async fn an_answer_that_keeps_breaking_ends_with_an_error_not_with_half_of_it() {
    let cut = sse(&chat_answer()[..3]);
    let (base, seen) = server((0..5).map(|_| (200, cut.clone())).collect()).await;
    let llm = quick(Protocol::OpenaiChat, base);
    let mut deltas = Vec::new();
    let err = step(&llm, &mut deltas).await.unwrap_err();
    assert!(err.to_string().contains("before the model finished its answer"), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 5);
    assert_eq!(deltas.iter().filter(|d| matches!(d, Delta::Retry { .. })).count(), 4);
}

#[tokio::test]
async fn a_server_that_never_says_it_is_done_is_believed_the_second_time() {
    // whole answers, but no finish reason and no [DONE] (some small servers)
    let quiet = sse(&[json!({ "choices": [{ "delta": { "content": "Hello." } }] })]);
    let (base, seen) = server((0..3).map(|_| (200, quiet.clone())).collect()).await;
    let llm = quick(Protocol::OpenaiChat, base);
    let mut deltas = Vec::new();
    let reply = step(&llm, &mut deltas).await.unwrap();
    assert_eq!(reply.content, vec![Block::Text { text: "Hello.".into() }]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    // from then on it's taken at its word: one request a step
    let reply = step(&llm, &mut deltas).await.unwrap();
    assert_eq!(reply.content, vec![Block::Text { text: "Hello.".into() }]);
    assert_eq!(seen.lock().unwrap().len(), 3);
}

/// Serve raw HTTP responses in order, each written as it is before the connection closes.
async fn raw_server(responses: Vec<String>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(0));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for resp in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body_start) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
                }
            };
            let len: usize = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
            while buf.len() < body_start + len {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            *seen2.lock().unwrap() += 1;
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn a_connection_that_drops_after_the_answer_was_complete_keeps_the_answer() {
    // everything but the usage and [DONE] arrived, then the connection died
    let whole = sse(&chat_answer());
    let (base, seen) = raw_server(vec![ok_stream(&whole, whole.len() + 500)]).await;
    let llm = quick(Protocol::OpenaiChat, base);
    let mut deltas = Vec::new();
    let reply = step(&llm, &mut deltas).await.unwrap();
    assert_eq!(reply.content.last(), Some(&Block::ToolUse { id: "call_9".into(), name: "read".into(), input: json!({ "path": "a.txt" }) }));
    // not asked again, and nothing taken back from whoever shows it
    assert_eq!(*seen.lock().unwrap(), 1);
    assert!(!deltas.iter().any(|d| matches!(d, Delta::Retry { .. })));
}

#[tokio::test]
async fn only_a_server_that_never_said_done_is_believed_when_a_stream_just_ends() {
    let quiet = sse(&[json!({ "choices": [{ "delta": { "content": "Hello." } }] })]);
    // this one says "done": a stream of its that just ends later was cut, however whole it looks
    let mut responses = vec![(200, sse(&chat_answer()) + "data: [DONE]\n\n")];
    responses.extend((0..5).map(|_| (200, quiet.clone())));
    let (base, seen) = server(responses).await;
    let llm = quick(Protocol::OpenaiChat, base);
    let mut deltas = Vec::new();
    step(&llm, &mut deltas).await.unwrap();
    let err = step(&llm, &mut deltas).await.unwrap_err();
    assert!(err.to_string().contains("before the model finished its answer"), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 6);

    // Anthropic's API always ends its messages: one without its end is never taken as whole
    let cut = sse(&[
        json!({ "type": "message_start", "message": { "usage": { "input_tokens": 11, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello, and" } }),
    ]);
    let (base, seen) = server((0..5).map(|_| (200, cut.clone())).collect()).await;
    let llm = quick(Protocol::Anthropic, base);
    assert!(step(&llm, &mut deltas).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 5);
}

fn ok_stream(body: &str, declared: usize) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {declared}\r\n\r\n{body}")
}

#[tokio::test]
async fn a_connection_that_drops_while_the_model_answers_is_tried_again() {
    let whole = sse(&[
        json!({ "type": "message_start", "message": { "usage": { "input_tokens": 11, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello." } }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
        json!({ "type": "message_stop" }),
    ]);
    // the first try promises more bytes than it sends: the connection died on the way
    let part = &whole[..whole.find("content_block_delta").unwrap() + 80];
    let (base, seen) = raw_server(vec![ok_stream(part, whole.len() + 500), ok_stream(&whole, whole.len())]).await;
    let llm = quick(Protocol::Anthropic, base);
    let mut deltas = Vec::new();
    let reply = step(&llm, &mut deltas).await.unwrap();
    assert_eq!(reply.content, vec![Block::Text { text: "Hello.".into() }]);
    assert_eq!(*seen.lock().unwrap(), 2);
    assert!(deltas.iter().any(|d| matches!(d, Delta::Retry { attempt: 1, of: 4, .. })));
}

#[tokio::test]
async fn an_overloaded_provider_in_the_middle_of_a_stream_is_tried_again_and_a_refusal_isnt() {
    let start = json!({ "type": "message_start", "message": { "usage": { "input_tokens": 11, "output_tokens": 1 } } });
    let overloaded = sse(&[start.clone(), json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } })]);
    let whole = sse(&[
        start.clone(),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello." } }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
        json!({ "type": "message_stop" }),
    ]);
    let refused = sse(&[start, json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "no such tool" } })]);
    let (base, seen) = server(vec![(200, overloaded), (200, whole), (200, refused)]).await;
    let llm = quick(Protocol::Anthropic, base);
    let mut deltas = Vec::new();
    assert_eq!(step(&llm, &mut deltas).await.unwrap().content, vec![Block::Text { text: "Hello.".into() }]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    let err = step(&llm, &mut deltas).await.unwrap_err();
    assert_eq!(err.to_string(), "model provider: no such tool");
    assert_eq!(seen.lock().unwrap().len(), 3);
}
