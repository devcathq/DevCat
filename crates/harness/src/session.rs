//! The session: an append-only JSONL event log, one file per session. Every other view (the
//! messages sent to the model, the chat UI, resume, token totals, the trace) is a projection of it,
//! so a crash never loses more than the event being written. Each line also carries `ms`, when it
//! was written (Unix time in milliseconds), for the trace; reading ignores it.
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
        /// Context hooks added to it (SessionStart, UserPromptSubmit): the model gets it, the
        /// chat doesn't show it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        context: Vec<String>,
        /// Not from the user but from a hook (the event, e.g. "Stop": the agent was told to go on).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook: Option<String>,
    },
    /// One model step: what the model said (text, thinking, tool calls) and what it cost.
    Assistant { content: Vec<Block>, model: String, usage: Usage },
    /// Results of the tool calls of the previous step, in call order, and the tokens the calls
    /// used themselves (helpers started with `subagent`; their own sessions have the details).
    ToolResults {
        results: Vec<Block>,
        #[serde(default, skip_serializing_if = "is_zero")]
        usage: Usage,
        /// How long each call took in milliseconds, in the same order (for the trace).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        took_ms: Vec<u64>,
    },
    /// A decision on a tool call that needed approval (kept for the audit trail).
    Approval { tool_use_id: String, tool: String, allowed: bool, by: String },
    /// The turn ended without a final answer (provider error, stopped, step limit).
    Interrupted { reason: String },
    /// Free-form title, e.g. set by the user. The last one wins.
    Title { title: String },
    /// The conversation so far was summarized to free the context window: from here on the model
    /// gets `summary` instead of the events before it (they stay in the log).
    Compacted {
        summary: String,
        model: String,
        usage: Usage,
        tokens_before: u64,
        /// The user's last `kept` messages, sent since the model's last step and not answered
        /// yet, stay in the conversation after the summary as they are (their images and hook
        /// context too): a summary made as a turn starts must not swallow the message that
        /// started it.
        #[serde(default, skip_serializing_if = "is_none")]
        kept: usize,
        /// The last `tail` events before this one stay in the conversation after the summary, word
        /// for word: the newest steps (the file just read, the error just seen), so the model goes
        /// on without looking again. They start with one of the model's steps, are all since the
        /// compaction before, and take in the messages `kept` would keep (it's 0 then).
        #[serde(default, skip_serializing_if = "is_none")]
        tail: usize,
    },
}

fn is_none(n: &usize) -> bool {
    *n == 0
}

fn is_zero(u: &Usage) -> bool {
    *u == Usage::default()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl SessionEvent {
    /// A message the user typed.
    pub fn user(text: impl Into<String>, images: Vec<Block>) -> SessionEvent {
        SessionEvent::User { text: text.into(), images, context: vec![], hook: None }
    }
}

/// Context a hook added to a message, as the model gets it.
pub fn hook_context(c: &str) -> String {
    format!("<hook-context>\n{c}\n</hook-context>")
}

/// The first user message after a compaction: the summary of everything before it.
pub fn summary_message(summary: &str) -> String {
    format!("This session continues an earlier conversation that was summarized to free the context window. The summary:\n\n{summary}")
}

/// What the model is told, with the user's next message, about a turn that ended without an
/// answer (`reason`: the step limit, a stop, an error). Without it the model sees its work end in
/// the middle and a message like "continue", and may well start over.
pub fn interrupted_note(reason: &str) -> String {
    let reason: String = reason.trim().trim_end_matches('.').chars().take(300).collect();
    format!("[The turn before this message ended before you answered: {reason}. Everything up to there happened and still stands: go on from it instead of starting over, unless this message asks for something else.]")
}

/// About how many tokens the next request takes, as far as the log knows (see
/// [`Session::weight`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weight {
    /// What the provider counted for the last step (its input and its output). None before any
    /// step, right after a compaction, and with providers that don't count.
    pub counted: Option<u64>,
    /// Characters added to the conversation after that step; without a count, all of them since
    /// the last compaction (the system prompt and the tools aren't the log's to know).
    pub chars: usize,
}

impl Weight {
    /// In tokens, at about 4 characters each.
    pub fn tokens(&self) -> u64 {
        self.counted.unwrap_or(0) + (self.chars / 4) as u64
    }
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
        Session::create_named(dir, &uuid::Uuid::new_v4().to_string(), cwd).await
    }

    /// [`Session::create`] with a chosen id (a helper's session is named after the call that
    /// started it). A name that's taken gets a suffix.
    pub async fn create_named(dir: &Path, id: &str, cwd: &str) -> Result<Session> {
        tokio::fs::create_dir_all(dir).await?;
        let mut id = file_safe(id);
        if dir.join(format!("{id}.jsonl")).exists() {
            id = format!("{id}-{}", uuid::Uuid::new_v4());
        }
        let mut s = Session { id: id.clone(), cwd: cwd.to_string(), path: dir.join(format!("{id}.jsonl")), events: vec![] };
        let created_ms = now_ms();
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

    /// A new session in `dir` with this one's events before `before` (an index into
    /// [`Session::events`]), as they were written (their times too), and `title` as its title
    /// (when not empty: the last title wins):
    /// going back to an earlier message to say something else, while this chat stays as it is.
    /// The events are copied from the file, so this session must be the file's only writer
    /// right now. A cut in the middle of a turn (a message sent while the agent worked) is told
    /// to the model with the next message, like a turn that was stopped.
    pub async fn fork(&self, dir: &Path, before: usize, title: &str) -> Result<Session> {
        let before = before.clamp(1, self.events.len());
        let text = tokio::fs::read_to_string(&self.path).await?;
        // one line for each event (`open` cut a torn last line off)
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() < self.events.len() {
            return Err(Error::Session(format!("{} has fewer lines than events: it's being written", self.path.display())));
        }
        let mut s = Session::create(dir, &self.cwd).await?;
        let mut out = String::new();
        for (e, line) in self.events[..before].iter().zip(&lines).skip(1) {
            // every one, titles too: a summary's `tail` counts the events before it
            if !matches!(e, SessionEvent::Meta { .. }) {
                out.push_str(line);
                out.push('\n');
            }
        }
        let mut f = tokio::fs::OpenOptions::new().append(true).open(&s.path).await?;
        f.write_all(out.as_bytes()).await?;
        f.flush().await?;
        drop(f);
        s.events.extend(self.events[1..before].iter().cloned());
        let mid_turn = match s.events.iter().rev().find(|e| matches!(e, SessionEvent::Assistant { .. } | SessionEvent::ToolResults { .. } | SessionEvent::Interrupted { .. })) {
            Some(SessionEvent::Assistant { content, .. }) => content.iter().any(|b| matches!(b, Block::ToolUse { .. })),
            Some(SessionEvent::ToolResults { .. }) => true,
            _ => false,
        };
        if mid_turn {
            s.append(SessionEvent::Interrupted { reason: "The user went back to an earlier message in the middle of your work and wrote this one instead".into() }).await?;
        }
        let title: String = title.trim().chars().take(120).collect();
        if !title.is_empty() {
            s.append(SessionEvent::Title { title }).await?;
        }
        Ok(s)
    }

    /// Index in [`Session::events`] of the `nth` (from 0) message the user wrote whose text,
    /// without the list of attached files the app adds, is `text`.
    pub fn user_message(&self, text: &str, nth: usize) -> Option<usize> {
        let own = |t: &str| t.split("\n\nAttached files:").next().unwrap_or(t).trim().to_string();
        let want = own(text);
        self.events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, SessionEvent::User { text, hook: None, .. } if own(text) == want || (want.is_empty() && own(text) == "See the attached files.")))
            .nth(nth)
            .map(|(i, _)| i)
    }

    /// Keep the whole output of a tool call that was too long to send, in `<id>.out/` next to the
    /// session file, so the model can read it in parts. Returns the file.
    pub async fn save_output(&self, tool_use_id: &str, text: &str) -> Result<PathBuf> {
        let dir = self.path.with_extension("out");
        tokio::fs::create_dir_all(&dir).await?;
        let file = dir.join(format!("{}.txt", file_safe(tool_use_id)));
        tokio::fs::write(&file, text).await?;
        Ok(file)
    }

    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// Write one event (a line, flushed) and keep it in memory.
    pub async fn append(&mut self, e: SessionEvent) -> Result<()> {
        let json = serde_json::to_string(&e).map_err(|e| Error::Session(e.to_string()))?;
        // `ms` first, then the event's own fields (an event is always a JSON object)
        let mut line = format!("{{\"ms\":{},{}", now_ms(), json.strip_prefix('{').unwrap_or(&json));
        line.push('\n');
        let mut f = tokio::fs::OpenOptions::new().create(true).append(true).open(&self.path).await?;
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        self.events.push(e);
        Ok(())
    }

    /// The conversation to send to the model. Tool calls without results (the turn was cut off)
    /// get an error result, because providers reject a tool call that has no answer; a step that
    /// said nothing is left out, because they reject an empty message too. A message that follows
    /// a turn that didn't finish says so first ([`interrupted_note`]).
    pub fn messages(&self) -> Vec<Message> {
        let mut p = Projection::default();
        for (i, e) in self.events.iter().enumerate() {
            match e {
                SessionEvent::Compacted { summary, kept, tail, .. } => {
                    p.out.clear();
                    p.cut = None;
                    push(&mut p.out, Role::User, vec![Block::Text { text: summary_message(summary) }]);
                    if *tail > 0 {
                        // the newest steps again, as they were (with the messages nothing
                        // answered yet: they're the last of them)
                        p.unanswered.clear();
                        for e in &self.events[i.saturating_sub(*tail)..i] {
                            p.add(e);
                        }
                    } else {
                        p.unanswered.drain(..p.unanswered.len().saturating_sub(*kept));
                        for content in &p.unanswered {
                            push(&mut p.out, Role::User, content.clone());
                        }
                    }
                }
                e => p.add(e),
            }
        }
        close_dangling(&mut p.out);
        p.out
    }

    /// How many of the events at the end of the log could stay word for word after a summary, in
    /// about `chars` characters: from one of the model's steps on (so every tool call has its
    /// result, and the user's message before it is in the summary), all since the last
    /// compaction, and leaving at least one step before them for the summary. 0 when not even
    /// the newest step fits.
    pub fn tail_within(&self, chars: usize) -> usize {
        let from = self.events.iter().rposition(|e| matches!(e, SessionEvent::Compacted { .. })).map_or(0, |i| i + 1);
        let first_step = self.events[from..].iter().position(|e| matches!(e, SessionEvent::Assistant { content, .. } if says_something(content))).map(|i| from + i);
        let mut size = 0;
        let mut tail = 0;
        for i in (from..self.events.len()).rev() {
            size += event_chars(&self.events[i]);
            if size > chars {
                break;
            }
            if Some(i) > first_step && matches!(&self.events[i], SessionEvent::Assistant { content, .. } if says_something(content)) {
                tail = self.events.len() - i;
            }
        }
        tail
    }

    /// The `tail` of the last compaction (see [`SessionEvent::Compacted`]); 0 without one.
    pub fn last_tail(&self) -> usize {
        self.events.iter().rev().find_map(|e| match e {
            SessionEvent::Compacted { tail, .. } => Some(*tail),
            _ => None,
        }).unwrap_or(0)
    }

    /// Tokens used by the whole session.
    pub fn usage(&self) -> Usage {
        let mut u = Usage::default();
        for e in &self.events {
            if let SessionEvent::Assistant { usage, .. } | SessionEvent::Compacted { usage, .. } | SessionEvent::ToolResults { usage, .. } = e {
                u += *usage;
            }
        }
        u
    }

    /// What the next request weighs: what the provider counted for the last step, and the
    /// characters that came after it. A step whose provider didn't count (some local servers
    /// leave the usage out) is weighed by its characters like everything else, instead of
    /// passing for an empty conversation.
    pub fn weight(&self) -> Weight {
        let mut w = Weight { counted: None, chars: 0 };
        // the size of each of the user's messages since the model's last step (see `unanswered`)
        let mut unanswered: Vec<usize> = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            match e {
                SessionEvent::Assistant { usage, content, .. } => {
                    if says_something(content) {
                        unanswered.clear();
                    }
                    if usage.input_tokens > 0 {
                        w = Weight { counted: Some(usage.input_tokens + usage.output_tokens), chars: 0 };
                    } else {
                        w.chars += content.iter().map(block_chars).sum::<usize>();
                    }
                }
                SessionEvent::Compacted { summary, tail, .. } if *tail > 0 => {
                    // (the messages nothing answered yet are in the tail, and stay unanswered)
                    w = Weight { counted: None, chars: summary.len() + self.events[i.saturating_sub(*tail)..i].iter().map(event_chars).sum::<usize>() };
                }
                SessionEvent::Compacted { summary, kept, .. } => {
                    unanswered.drain(..unanswered.len().saturating_sub(*kept));
                    w = Weight { counted: None, chars: summary.len() + unanswered.iter().sum::<usize>() };
                }
                SessionEvent::User { text, images, context, .. } => {
                    let chars = user_chars(text, images, context);
                    unanswered.push(chars);
                    w.chars += chars;
                }
                SessionEvent::ToolResults { results, .. } => w.chars += results.iter().map(block_chars).sum::<usize>(),
                _ => {}
            }
        }
        w
    }

    /// The user's messages since the model's last step, which nothing has answered yet (the
    /// message that starts a turn, ones sent while it worked): the size of each, oldest first.
    /// A compaction decides by them what to keep as it is (`Compacted::kept`).
    pub fn unanswered(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for e in &self.events {
            match e {
                SessionEvent::Assistant { content, .. } if says_something(content) => out.clear(),
                // a tail keeps them all (they're its last events)
                SessionEvent::Compacted { tail, .. } if *tail > 0 => {}
                SessionEvent::Compacted { kept, .. } => {
                    out.drain(..out.len().saturating_sub(*kept));
                }
                SessionEvent::User { text, images, context, .. } => out.push(user_chars(text, images, context)),
                _ => {}
            }
        }
        out
    }

    /// About how many tokens the next request takes: what the provider counted for the last step
    /// (input + output), plus an estimate (4 characters a token) for what came after it. Before
    /// any step, or right after a compaction, it's all estimated.
    pub fn context_tokens(&self) -> u64 {
        self.weight().tokens()
    }

    /// The events since the last compaction (all of them when there was none).
    fn since_compaction(&self) -> &[SessionEvent] {
        let from = self.events.iter().rposition(|e| matches!(e, SessionEvent::Compacted { .. })).map_or(0, |i| i + 1);
        &self.events[from..]
    }

    /// Model steps since the last compaction: what a new summary would have to work with.
    pub fn steps_since_compaction(&self) -> usize {
        self.since_compaction().iter().filter(|e| matches!(e, SessionEvent::Assistant { .. })).count()
    }

    /// The summary the conversation now starts with, if it was ever compacted.
    pub fn last_summary(&self) -> Option<&str> {
        self.events.iter().rev().find_map(|e| match e {
            SessionEvent::Compacted { summary, .. } => Some(summary.as_str()),
            _ => None,
        })
    }

    /// What the provider counted as input for the first step after the last compaction: the
    /// weight a compacted conversation starts from (the system prompt, the tools and the
    /// summary), which summarizing again can't get under. None when it was never compacted, no
    /// step followed yet, or the provider doesn't count.
    pub fn compacted_base(&self) -> Option<u64> {
        self.last_summary()?;
        self.since_compaction().iter().find_map(|e| match e {
            SessionEvent::Assistant { usage, .. } if usage.input_tokens > 0 => Some(usage.input_tokens),
            _ => None,
        })
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
            SessionEvent::User { text, hook: None, .. } => Some(text.trim()),
            _ => None,
        });
        let line = first.unwrap_or("New session").lines().next().unwrap_or_default();
        if line.chars().count() > 60 { format!("{}…", line.chars().take(60).collect::<String>()) } else { line.to_string() }
    }
}

/// `id` as part of a file name.
fn file_safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Whether a step's message has anything a provider takes back: text or a tool call. (Reasoning
/// alone isn't sent back by most adapters.)
fn says_something(content: &[Block]) -> bool {
    content.iter().any(|b| match b {
        Block::Text { text } => !text.trim().is_empty(),
        Block::ToolUse { .. } => true,
        _ => false,
    })
}

/// About how many characters a user's message adds to a request (an image as 6,000).
fn user_chars(text: &str, images: &[Block], context: &[String]) -> usize {
    text.len() + context.iter().map(|c| c.len()).sum::<usize>() + images.len() * 6_000
}

pub(crate) fn block_chars(b: &Block) -> usize {
    match b {
        Block::Text { text } | Block::Thinking { text, .. } => text.len(),
        Block::ToolResult { content, .. } => content.len(),
        Block::ToolUse { input, .. } => input.to_string().len(),
        Block::Image { .. } => 6_000,
    }
}

/// What the events add to the conversation sent to the model ([`Session::messages`]).
#[derive(Default)]
struct Projection<'a> {
    out: Vec<Message>,
    /// How the turn before the next user message ended, if it ended early.
    cut: Option<&'a str>,
    /// The user's messages since the model's last step: a compaction may keep them.
    unanswered: Vec<Vec<Block>>,
}

impl<'a> Projection<'a> {
    fn add(&mut self, e: &'a SessionEvent) {
        match e {
            SessionEvent::User { text, images, context, .. } => {
                close_dangling(&mut self.out);
                let mut content = Vec::new();
                if let Some(reason) = self.cut.take() {
                    content.push(Block::Text { text: interrupted_note(reason) });
                }
                content.push(Block::Text { text: text.clone() });
                content.extend(context.iter().map(|c| Block::Text { text: hook_context(c) }));
                content.extend(images.iter().cloned());
                self.unanswered.push(content.clone());
                push(&mut self.out, Role::User, content);
            }
            SessionEvent::Assistant { content, .. } if says_something(content) => {
                self.unanswered.clear();
                push(&mut self.out, Role::Assistant, content.clone());
            }
            SessionEvent::ToolResults { results, .. } => push(&mut self.out, Role::User, results.clone()),
            SessionEvent::Interrupted { reason } => self.cut = Some(reason),
            _ => {}
        }
    }
}

/// About how many characters an event adds to the conversation.
fn event_chars(e: &SessionEvent) -> usize {
    match e {
        SessionEvent::User { text, images, context, .. } => user_chars(text, images, context),
        SessionEvent::Assistant { content, .. } => content.iter().map(block_chars).sum(),
        SessionEvent::ToolResults { results, .. } => results.iter().map(block_chars).sum(),
        _ => 0,
    }
}

/// Merge into the previous message when the role repeats (providers want alternating roles).
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
        s.append(SessionEvent::user("list files", vec![])).await.unwrap();
        let call = Block::ToolUse { id: "t1".into(), name: "glob".into(), input: json!({ "pattern": "*" }) };
        s.append(SessionEvent::Assistant { content: vec![call], model: "m".into(), usage: Usage { input_tokens: 10, output_tokens: 2, cache_read_tokens: 0 } }).await.unwrap();
        s.append(SessionEvent::ToolResults { results: vec![Block::ToolResult { tool_use_id: "t1".into(), content: "a.rs".into(), is_error: false }], usage: Usage::default(), took_ms: vec![5] }).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "One file.".into() }], model: "m".into(), usage: Usage { input_tokens: 20, output_tokens: 3, cache_read_tokens: 0 } }).await.unwrap();

        let r = Session::open(s.path()).await.unwrap();
        assert_eq!(r.id, s.id);
        assert_eq!(r.events(), s.events());
        let m = r.messages();
        assert_eq!(m.iter().map(|m| m.role).collect::<Vec<_>>(), vec![Role::User, Role::Assistant, Role::User, Role::Assistant]);
        assert_eq!(r.usage().input_tokens, 30);
        assert_eq!(r.title(), "list files");
        // every line says when it was written
        let text = std::fs::read_to_string(s.path()).unwrap();
        assert!(text.lines().all(|l| l.starts_with("{\"ms\":1") && serde_json::from_str::<serde_json::Value>(l).unwrap()["ms"].as_u64().unwrap() > 1_700_000_000_000));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn weighs_the_conversation_with_or_without_the_providers_count() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        let step = |text: &str, input: u64| SessionEvent::Assistant { content: vec![Block::Text { text: text.into() }], model: "m".into(), usage: Usage { input_tokens: input, output_tokens: if input > 0 { 5 } else { 0 }, cache_read_tokens: 0 } };
        s.append(SessionEvent::user("x".repeat(400), vec![])).await.unwrap();
        // a provider that doesn't count: the characters stand in, the model's own too
        s.append(step(&"y".repeat(400), 0)).await.unwrap();
        assert_eq!(s.weight(), Weight { counted: None, chars: 800 });
        assert_eq!(s.context_tokens(), 200);
        // one that counts: its number, plus what came after
        s.append(SessionEvent::user("z".repeat(40), vec![])).await.unwrap();
        s.append(step("ok", 300)).await.unwrap();
        s.append(SessionEvent::user("w".repeat(80), vec![])).await.unwrap();
        assert_eq!(s.weight(), Weight { counted: Some(305), chars: 80 });
        assert_eq!((s.context_tokens(), s.steps_since_compaction(), s.compacted_base()), (325, 2, None));
        // after a summary: its characters, until the next step is counted
        // (the message sent since the last step isn't kept here: the summary stands for it)
        s.append(SessionEvent::Compacted { summary: "s".repeat(120), model: "m".into(), usage: Usage::default(), tokens_before: 325, kept: 0, tail: 0 }).await.unwrap();
        assert_eq!((s.context_tokens(), s.steps_since_compaction(), s.compacted_base()), (30, 0, None));
        assert!(s.unanswered().is_empty());
        s.append(step("go on", 90)).await.unwrap();
        s.append(step("more", 140)).await.unwrap();
        assert_eq!((s.steps_since_compaction(), s.compacted_base()), (2, Some(90)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_summary_keeps_the_messages_nothing_answered_yet() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        let image = Block::Image { media_type: "image/png".into(), data: "AAAA".into() };
        s.append(SessionEvent::user("first task", vec![])).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "Done.".into() }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::user("an older note", vec![])).await.unwrap();
        s.append(SessionEvent::User { text: "what's wrong in this screenshot?".into(), images: vec![image.clone()], context: vec!["branch: main".into()], hook: None }).await.unwrap();
        assert_eq!(s.unanswered(), vec![13, 32 + 12 + 6_000]);
        // summarized as the turn starts, keeping the newest message: it's there whole, after the summary
        s.append(SessionEvent::Compacted { summary: "S".into(), model: "m".into(), usage: Usage::default(), tokens_before: 9, kept: 1, tail: 0 }).await.unwrap();
        let m = s.messages();
        assert_eq!(m.len(), 1);
        assert!(m[0].content[0] == Block::Text { text: summary_message("S") } && m[0].content[1] == Block::Text { text: "what's wrong in this screenshot?".into() }, "{:?}", m[0].content);
        assert!(m[0].content.contains(&image) && m[0].content.contains(&Block::Text { text: hook_context("branch: main") }));
        assert!(!m[0].content.contains(&Block::Text { text: "an older note".into() }));
        assert_eq!((s.weight().chars, s.unanswered().len()), (1 + 32 + 12 + 6_000, 1));
        // the line as written and read again says the same (and an older log's has no `kept`)
        let again = Session::open(s.path()).await.unwrap();
        assert_eq!(again.messages(), m);
        assert!(std::fs::read_to_string(s.path()).unwrap().contains("\"kept\":1"));
        let old: SessionEvent = serde_json::from_str(r#"{"event":"compacted","summary":"S","model":"m","usage":{"input_tokens":1,"output_tokens":1},"tokens_before":9}"#).unwrap();
        assert!(matches!(old, SessionEvent::Compacted { kept: 0, .. }));
        // summarized once more without keeping it (it didn't fit): only the summary is left
        s.append(SessionEvent::Compacted { summary: "T".into(), model: "m".into(), usage: Usage::default(), tokens_before: 9, kept: 0, tail: 0 }).await.unwrap();
        assert_eq!(s.messages()[0].content, vec![Block::Text { text: summary_message("T") }]);
        assert!(s.unanswered().is_empty());
        // once the model answered, a later summary has nothing of it to keep
        s.append(SessionEvent::user("next", vec![])).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "OK.".into() }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::Compacted { summary: "U".into(), model: "m".into(), usage: Usage::default(), tokens_before: 9, kept: 3, tail: 0 }).await.unwrap();
        assert_eq!(s.messages()[0].content, vec![Block::Text { text: summary_message("U") }]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_summary_can_keep_the_newest_steps_word_for_word() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        let step = |id: &str| SessionEvent::Assistant { content: vec![Block::ToolUse { id: id.into(), name: "read".into(), input: json!({ "path": id }) }], model: "m".into(), usage: Usage { input_tokens: 50, output_tokens: 5, cache_read_tokens: 0 } };
        let done = |id: &str, out: &str| SessionEvent::ToolResults { results: vec![Block::ToolResult { tool_use_id: id.into(), content: out.into(), is_error: false }], usage: Usage::default(), took_ms: vec![] };
        s.append(SessionEvent::user("the task", vec![])).await.unwrap();
        // nothing but the first step: there'd be nothing left to summarize
        s.append(step("a")).await.unwrap();
        s.append(done("a", &"A".repeat(1_000))).await.unwrap();
        assert_eq!(s.tail_within(100_000), 0);
        s.append(step("b")).await.unwrap();
        s.append(done("b", "B")).await.unwrap();
        s.append(SessionEvent::user("also this", vec![])).await.unwrap();
        // the newest step, its result and the message after it; with more room, no further back
        // than after the first step
        assert_eq!(s.tail_within(10), 0);
        assert_eq!(s.tail_within(500), 3);
        assert_eq!(s.tail_within(100_000), 3);
        s.append(SessionEvent::Compacted { summary: "S".into(), model: "m".into(), usage: Usage::default(), tokens_before: 9, kept: 0, tail: 3 }).await.unwrap();
        let m = s.messages();
        assert_eq!(m.iter().map(|m| m.role).collect::<Vec<_>>(), vec![Role::User, Role::Assistant, Role::User]);
        assert_eq!(m[0].text(), summary_message("S"));
        assert!(m[1].tool_uses().any(|(id, _, _)| id == "b"));
        assert!(matches!(&m[2].content[0], Block::ToolResult { tool_use_id, .. } if tool_use_id == "b"));
        assert_eq!(m[2].content[1], Block::Text { text: "also this".into() });
        assert!(!m.iter().any(|m| m.tool_uses().any(|(id, _, _)| id == "a")));
        // the message is still unanswered, and the tail is weighed
        assert_eq!(s.unanswered(), vec![9]);
        assert_eq!(s.weight(), Weight { counted: None, chars: 1 + event_chars(&step("b")) + 1 + 9 });
        assert_eq!((s.last_tail(), s.steps_since_compaction()), (3, 0));
        // read back the same; an older log's line has no tail
        assert_eq!(Session::open(s.path()).await.unwrap().messages(), m);
        let old: SessionEvent = serde_json::from_str(r#"{"event":"compacted","summary":"S","model":"m","usage":{"input_tokens":1,"output_tokens":1},"tokens_before":9,"kept":1}"#).unwrap();
        assert!(matches!(old, SessionEvent::Compacted { tail: 0, kept: 1, .. }));
        // the next summary's tail is taken from after this one only
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "ok".into() }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        assert_eq!(s.tail_within(100_000), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_fork_has_the_conversation_up_to_a_message_and_the_original_stays() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        let said = |t: &str| SessionEvent::Assistant { content: vec![Block::Text { text: t.into() }], model: "m".into(), usage: Usage::default() };
        s.append(SessionEvent::user("first", vec![])).await.unwrap();
        s.append(said("one")).await.unwrap();
        s.append(SessionEvent::Title { title: "Mine".into() }).await.unwrap();
        s.append(SessionEvent::user("second\n\nAttached files:\n- /a.png (1 KB)", vec![])).await.unwrap();
        s.append(said("two")).await.unwrap();
        s.append(SessionEvent::user("first", vec![])).await.unwrap();
        assert_eq!(s.user_message("second", 0), Some(4));
        assert_eq!(s.user_message("first", 1), Some(6));
        assert_eq!((s.user_message("first", 2), s.user_message("nope", 0)), (None, None));
        let f = s.fork(&dir, 4, "Mine (fork)").await.unwrap();
        assert_ne!(f.id, s.id);
        let m = f.messages();
        assert_eq!(m.iter().map(|m| m.text()).collect::<Vec<_>>(), vec!["first", "one"]);
        assert_eq!(f.title(), "Mine (fork)");
        // on disk the same, with the times the events were first written
        let again = Session::open(f.path()).await.unwrap();
        assert_eq!(again.events(), f.events());
        let line = |p: &Path, n: usize| std::fs::read_to_string(p).unwrap().lines().nth(n).unwrap().to_string();
        assert_eq!(line(f.path(), 1), line(s.path(), 1));
        assert_eq!(s.events().len(), 7);
        // cut while the agent worked: the model hears that its work stopped there
        s.append(SessionEvent::Assistant { content: vec![Block::ToolUse { id: "t".into(), name: "read".into(), input: json!({}) }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::ToolResults { results: vec![Block::ToolResult { tool_use_id: "t".into(), content: "x".into(), is_error: false }], usage: Usage::default(), took_ms: vec![] }).await.unwrap();
        s.append(SessionEvent::user("sent while it worked", vec![])).await.unwrap();
        let mut f = s.fork(&dir, 9, "").await.unwrap();
        f.append(SessionEvent::user("something else", vec![])).await.unwrap();
        let last = f.messages().pop().unwrap();
        assert!(last.text().contains("went back to an earlier message") && last.text().contains("something else"), "{}", last.text());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_step_that_said_nothing_and_a_turn_cut_short() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        s.append(SessionEvent::user("go", vec![])).await.unwrap();
        // only reasoning, then nothing at all: neither is a message a provider takes
        s.append(SessionEvent::Assistant { content: vec![Block::Thinking { text: "hm".into(), signature: None }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::Interrupted { reason: "model provider: 500 boom.".into() }).await.unwrap();
        // not said twice, and not without a message to say it with
        assert_eq!(s.messages().len(), 1);
        s.append(SessionEvent::user("again", vec![])).await.unwrap();
        s.append(SessionEvent::Assistant { content: vec![Block::Text { text: "Done.".into() }], model: "m".into(), usage: Usage::default() }).await.unwrap();
        s.append(SessionEvent::user("thanks", vec![])).await.unwrap();
        let m = s.messages();
        assert_eq!(m.iter().map(|m| m.role).collect::<Vec<_>>(), vec![Role::User, Role::Assistant, Role::User]);
        let first: Vec<&str> = m[0].content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect();
        assert_eq!(first.len(), 3);
        assert!(first[0] == "go" && first[1].contains("ended before you answered: model provider: 500 boom.") && first[2] == "again", "{first:?}");
        assert_eq!(m[2].text(), "thanks");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn interrupted_tool_call_gets_an_error_result_and_torn_line_is_ignored() {
        let dir = tmp();
        let mut s = Session::create(&dir, "/proj").await.unwrap();
        s.append(SessionEvent::user("go", vec![])).await.unwrap();
        let call = Block::ToolUse { id: "t1".into(), name: "bash".into(), input: json!({ "command": "sleep 100" }) };
        s.append(SessionEvent::Assistant { content: vec![call], model: "m".into(), usage: Usage::default() }).await.unwrap();
        // crash in the middle of the next write
        let mut f = std::fs::OpenOptions::new().append(true).open(s.path()).unwrap();
        std::io::Write::write_all(&mut f, b"{\"event\":\"tool_res").unwrap();
        drop(f);

        let mut r = Session::open(s.path()).await.unwrap();
        r.append(SessionEvent::user("again", vec![])).await.unwrap();
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
