//! The session: an append-only JSONL event log, one file per session. Every other view (the
//! messages sent to the model, the chat UI, resume, token totals) is a projection of it, so a
//! crash never loses more than the event being written.
use crate::message::{Block, Message, Role, Usage};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Format version written in the first line. Bump it (with a migration in [`Session::open`]) when
/// an event changes shape.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Always the first line.
    Meta { format: u32, id: String, cwd: String, created_ms: u64 },
    /// A message from the user, with any images they attached.
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Block>,
    },
    /// One model step: what the model said (text, thinking, tool calls) and what it cost.
    Assistant { content: Vec<Block>, model: String, usage: Usage },
    /// Results of the tool calls of the previous step, in call order.
    ToolResults { results: Vec<Block> },
    /// A decision on a tool call that needed approval (kept for the audit trail).
    Approval { tool_use_id: String, tool: String, allowed: bool, by: String },
    /// The turn ended without a final answer (provider error, stopped, step limit).
    Interrupted { reason: String },
    /// Free-form title, e.g. set by the user. The last one wins.
    Title { title: String },
    /// The conversation so far was summarized to free the context window: from here on the model
    /// gets `summary` instead of the events before it (they stay in the log).
    Compacted { summary: String, model: String, usage: Usage, tokens_before: u64 },
}

/// The first user message after a compaction: the summary of everything before it.
pub fn summary_message(summary: &str) -> String {
    format!("This session continues an earlier conversation that was summarized to free the context window. The summary:\n\n{summary}")
}

pub struct Session {
    pub id: String,
    pub cwd: String,
    path: PathBuf,
    events: Vec<SessionEvent>,
}

impl Session {
    /// Start a new session file `<dir>/<id>.jsonl`.
    pub async fn create(dir: &Path, cwd: &str) -> Result<Session> {
        tokio::fs::create_dir_all(dir).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut s = Session { id: id.clone(), cwd: cwd.to_string(), path: dir.join(format!("{id}.jsonl")), events: vec![] };
        let created_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        s.append(SessionEvent::Meta { format: FORMAT, id, cwd: cwd.to_string(), created_ms }).await?;
        Ok(s)
    }

    /// Load a session to resume it. A torn last line (crash while writing) is cut off the file, so
    /// the next event starts on a clean line.
    pub async fn open(path: &Path) -> Result<Session> {
        let text = tokio::fs::read_to_string(path).await?;
        let mut events = Vec::new();
        let mut good = 0; // bytes of complete, valid lines
        let mut rest = text.as_str();
        let mut n = 0;
        while !rest.is_empty() {
            n += 1;
            let (line, next, complete) = match rest.find('\n') {
                Some(i) => (&rest[..i], &rest[i + 1..], true),
                None => (rest, "", false),
            };
            if !line.trim().is_empty() {
                match serde_json::from_str::<SessionEvent>(line) {
                    Ok(e) if complete => events.push(e),
                    Ok(_) | Err(_) if next.trim().is_empty() => break,
                    Err(e) => return Err(Error::Session(format!("{}: line {n}: {e}", path.display()))),
                    Ok(_) => unreachable!("only the last line can be incomplete"),
                }
            }
            good = text.len() - next.len();
            rest = next;
        }
        if good < text.len() {
            let f = tokio::fs::OpenOptions::new().write(true).open(path).await?;
            f.set_len(good as u64).await?;
        }
        let Some(SessionEvent::Meta { format, id, cwd, .. }) = events.first().cloned() else {
            return Err(Error::Session(format!("{}: missing header", path.display())));
        };
        if format > FORMAT {
            return Err(Error::Session(format!("{} was written by a newer DevCat (format {format})", path.display())));
        }
        Ok(Session { id, cwd, path: path.to_path_buf(), events })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// Write one event (a line, flushed) and keep it in memory.
    pub async fn append(&mut self, e: SessionEvent) -> Result<()> {
        let mut line = serde_json::to_string(&e).map_err(|e| Error::Session(e.to_string()))?;
        line.push('\n');
        let mut f = tokio::fs::OpenOptions::new().create(true).append(true).open(&self.path).await?;
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        self.events.push(e);
        Ok(())
    }

    /// The conversation to send to the model. Tool calls without results (the turn was cut off)
    /// get an error result, because providers reject a tool call that has no answer.
    pub fn messages(&self) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::new();
        for e in &self.events {
            match e {
                SessionEvent::Compacted { summary, .. } => {
                    out.clear();
                    push(&mut out, Role::User, vec![Block::Text { text: summary_message(summary) }]);
                }
                SessionEvent::User { text, images } => {
                    close_dangling(&mut out);
                    let mut content = vec![Block::Text { text: text.clone() }];
                    content.extend(images.iter().cloned());
                    push(&mut out, Role::User, content);
                }
                SessionEvent::Assistant { content, .. } => push(&mut out, Role::Assistant, content.clone()),
                SessionEvent::ToolResults { results } => push(&mut out, Role::User, results.clone()),
                _ => {}
            }
        }
        close_dangling(&mut out);
        out
    }

    /// Tokens used by the whole session.
    pub fn usage(&self) -> Usage {
        let mut u = Usage::default();
        for e in &self.events {
            if let SessionEvent::Assistant { usage, .. } | SessionEvent::Compacted { usage, .. } = e {
                u += *usage;
            }
        }
        u
    }

    /// About how many tokens the next request takes: what the provider counted for the last step
    /// (input + output), plus an estimate (4 characters a token) for what came after it. Before
    /// any step, or right after a compaction, it's all estimated.
    pub fn context_tokens(&self) -> u64 {
        let mut tokens = 0u64;
        let mut chars = 0usize;
        for e in &self.events {
            match e {
                SessionEvent::Assistant { usage, .. } => {
                    tokens = usage.input_tokens + usage.output_tokens;
                    chars = 0;
                }
                SessionEvent::Compacted { summary, .. } => {
                    tokens = 0;
                    chars = summary.len();
                }
                SessionEvent::User { text, images } => chars += text.len() + images.len() * 6_000,
                SessionEvent::ToolResults { results } => chars += results.iter().map(block_chars).sum::<usize>(),
                _ => {}
            }
        }
        tokens + (chars / 4) as u64
    }

    /// The title: the last `Title` event, else the start of the first user message.
    pub fn title(&self) -> String {
        if let Some(t) = self.events.iter().rev().find_map(|e| match e {
            SessionEvent::Title { title } => Some(title.clone()),
            _ => None,
        }) {
            return t;
        }
        let first = self.events.iter().find_map(|e| match e {
            SessionEvent::User { text, .. } => Some(text.trim()),
            _ => None,
        });
        let line = first.unwrap_or("New session").lines().next().unwrap_or_default();
        if line.chars().count() > 60 { format!("{}…", line.chars().take(60).collect::<String>()) } else { line.to_string() }
    }
}

/// Merge into the previous message when the role repeats (providers want alternating roles).
fn block_chars(b: &Block) -> usize {
    match b {
        Block::Text { text } | Block::Thinking { text, .. } => text.len(),
        Block::ToolResult { content, .. } => content.len(),
        Block::ToolUse { input, .. } => input.to_string().len(),
        Block::Image { .. } => 6_000,
    }
}

fn push(out: &mut Vec<Message>, role: Role, content: Vec<Block>) {
    match out.last_mut() {
        Some(m) if m.role == role => m.content.extend(content),
        _ => out.push(Message { role, content }),
    }
}

fn close_dangling(out: &mut Vec<Message>) {
    let Some(last) = out.last() else { return };
    if last.role != Role::Assistant {
        return;
    }
    let ids: Vec<String> = last.tool_uses().map(|(id, _, _)| id.to_string()).collect();
    if ids.is_empty() {
        return;
    }
    let results = ids
        .into_iter()
        .map(|id| Block::ToolResult { tool_use_id: id, content: "Not run: the turn was interrupted.".into(), is_error: true })
        .collect();
    push(out, Role::User, results);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("devcat-harness-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn round_trip_and_projection() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        s.append(SessionEvent::User { text: "list files".into(), images: vec![] }).await.unwrap();
        let call = Block::ToolUse { id: "t1".into(), name: "glob".into(), input: json!({ "pattern": "*" }) };
        s.append(SessionEvent::Assistant { content: vec![call], model: "m".into(), usage: Usage { input_tokens: 10, output_tokens: 2, cache_read_tokens: 0 } }).await.unwrap();
        s.append(SessionEvent::ToolResults { results: vec![Block::ToolResult { tool_use_id: "t1".into(), content: "a.rs".into(), is_error: false }] }).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "One file.".into() }], model: "m".into(), usage: Usage { input_tokens: 20, output_tokens: 3, cache_read_tokens: 0 } }).await.unwrap();

        let r = Session::open(s.path()).await.unwrap();
        assert_eq!(r.id, s.id);
        assert_eq!(r.events(), s.events());
        let m = r.messages();
        assert_eq!(m.iter().map(|m| m.role).collect::<Vec<_>>(), vec![Role::User, Role::Assistant, Role::User, Role::Assistant]);
        assert_eq!(r.usage().input_tokens, 30);
        assert_eq!(r.title(), "list files");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn interrupted_tool_call_gets_an_error_result_and_torn_line_is_ignored() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        s.append(SessionEvent::User { text: "go".into(), images: vec![] }).await.unwrap();
        let call = Block::ToolUse { id: "t1".into(), name: "bash".into(), input: json!({ "command": "sleep 100" }) };
        s.append(SessionEvent::Assistant { content: vec![call], model: "m".into(), usage: Usage::default() }).await.unwrap();
        // crash in the middle of the next write
        let mut f = std::fs::OpenOptions::new().append(true).open(s.path()).unwrap();
        std::io::Write::write_all(&mut f, b"{\"event\":\"tool_res").unwrap();
        drop(f);

        let mut r = Session::open(s.path()).await.unwrap();
        r.append(SessionEvent::User { text: "again".into(), images: vec![] }).await.unwrap();
        // the file is clean again: a second resume reads every event
        let r = Session::open(s.path()).await.unwrap();
        assert_eq!(r.events().len(), 4);
        let m = r.messages();
        assert_eq!(m.len(), 3);
        assert!(matches!(&m[2].content[0], Block::ToolResult { is_error: true, .. }));
        assert!(matches!(&m[2].content[1], Block::Text { text } if text == "again"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
