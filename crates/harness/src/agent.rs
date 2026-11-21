//! The agent loop. One user message is a *turn*: the model takes steps, each step may ask for
//! tool calls, the calls go through the pipeline (approval → timeout → truncation → event) and
//! their results go back to the model, until it answers without a tool call.
use crate::approval::{ApprovalRequest, Approver, Decision, DenyAll, Kind, Policy, Question};
use crate::hooks::{self, Hooks, Outcome};
use crate::llm::{Delta, Llm, Reply, Request, UNPARSED, is_overflow};
use crate::message::{Block, Message, Role, ToolSpec, Usage};
use crate::session::{Session, SessionEvent};
use crate::tools::{ASK_USER, FOUND, MAX_OUTPUT, Registry, SUBAGENT, TODO_WRITE, TOOL_SEARCH, ToolCtx, ToolOutput, truncate};
use crate::{Error, Result};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Default steps per turn before the agent stops and asks the user to continue (see
/// [`Agent::max_steps`]). High enough for a long build or port; the repeat guard
/// ([`MAX_SAME_CALL`]) catches a model stuck in a loop much sooner.
pub const MAX_STEPS: u32 = 200;

/// Steps a helper (`subagent`) takes before it gives up: it only looks around.
pub const HELPER_MAX_STEPS: u32 = 60;

/// Share of the context window at which the conversation is summarized before the next step.
pub const COMPACT_AT: f64 = 0.8;

/// Share of the window kept free on top of the model's answer: what was added since the
/// provider last counted is only estimated.
const MARGIN: f64 = 0.05;

/// The room kept for the answer never brings the limit under this share of the window. Servers
/// that don't count the answer against the window (llama.cpp, Ollama) would summarize far too
/// early with a small window otherwise; one that does, and refuses a step for it, is handled
/// when it says so (see [`is_overflow`]).
const COMPACT_FLOOR: f64 = 0.6;

/// The provider refused the conversation as too long this many times in a row with hardly a step
/// between the summaries: the window is too small for the work, and the turn ends saying so.
const MAX_SQUEEZED: u32 = 3;

/// The conversation is summarized again only once it grew by this share of the window since the
/// last summary. With a small window the system prompt, the tools and the summary can be over the
/// limit on their own: summarizing every step then frees nothing and makes the model forget what
/// it just did.
const WORTH_COMPACTING: f64 = 0.1;

/// After a summary that failed, the next automatic one waits this many steps.
const COMPACT_AGAIN_AFTER: u32 = 5;

/// How far the conversation is cut down for the summary request when it doesn't fit the window
/// itself (see [`shrink`]).
const SHRINK_LEVELS: u8 = 4;

/// The user's own messages carried over a compaction word for word: this many characters in all,
/// and of each one.
const KEPT_WORDS: usize = 6_000;
const KEPT_MESSAGE: usize = 2_000;

/// The user's messages a summary finds unanswered (the one that starts a turn, ones sent while
/// the agent worked) stay as they are after it, up to this many characters in all, and no more
/// than a quarter of a known window: more is left to the summary, or it would be what doesn't fit.
const KEPT_UNANSWERED: usize = 32_000;

/// The newest steps stay word for word after a summary ([`SessionEvent::Compacted`]'s `tail`) in
/// up to this share of a known window, and no more than [`TAIL_CHARS`] characters: what the model
/// just read or saw, so it doesn't look again.
const TAIL_SHARE: f64 = 0.1;
const TAIL_CHARS: usize = 80_000;

/// Files listed with a summary (see [`files_of`]): this many changed and this many read, newest first.
const LISTED_FILES: usize = 30;

/// Added to the summary request when the conversation already starts with a summary.
const UPDATE_SUMMARY: &str = "\n\nThe conversation starts with the summary made the last time it was cut. Write the new summary as an update of it: carry over what still matters (the request, decisions, errors and fixes, what's left to do), change what's different now, and don't leave something out only because it's old.";

/// What marks a message the harness wrote to the model itself, in the user's place: in the
/// session log (`User::hook`) and in the chat ([`AgentEvent::Hook`]).
pub const NOTE: &str = "DevCat";

/// An answer cut off at the output limit is asked to go on this many times in a row.
const MAX_CUT_OFF: u32 = 3;

const EMPTY_REPLY: &str = "Your last message was empty. If the task is done, say so in a sentence or two (what you did, what's left). If not, go on with the next step.";

const CUT_OFF: &str = "Your message was cut off at the output limit. Go on exactly where it stopped, without repeating what you already wrote.";

const CUT_OFF_CALL: &str = "Not run: your message hit the output limit before this call's arguments were complete. Make the call smaller: write a long file in parts (`write` the first part, then add the rest with `edit`), and put a long script in a file instead of the command line.";

const EMPTY_SUMMARY: &str = "\n\nYour last reply had no text. Write the summary now, as plain text, without calling a tool.";

const MID_TURN: &str = "\n\nThe conversation was summarized while you were working. Nothing else changed: go on now with what is still open (the user's latest message, if you hadn't finished with it), without starting over and without asking the user to repeat anything.";

/// Added to the system prompt while the user plans (see [`Policy::Plan`]).
const PLAN_MODE: &str = "\
# Plan mode
The user switched on plan mode: look around first and come back with a plan; don't change anything yet. \
Reading, searching, read-only git and helpers run as usual, web pages and queries ask, and edits and commands are \
refused until the user starts the plan. When you know enough, answer with the plan: the steps, the files \
you'd change and how, and anything the user should decide. Keep it short.";

/// The same call (tool and arguments) this many times in a row runs; the next one is refused, so a
/// model stuck in a loop hears it instead of spending the turn.
pub const MAX_SAME_CALL: usize = 3;

/// Helpers (`subagent` calls of one step) that run at the same time; more wait for a free slot.
pub const MAX_HELPERS: usize = 4;

/// Tools a helper doesn't get: the ones that change something, talk to the user or start helpers.
const HELPER_HIDDEN: &[&str] = &["write", "edit", "shell", TODO_WRITE, ASK_USER, SUBAGENT];

/// Added to a helper's system prompt.
const HELPER: &str = "\
# You're a helper
Another agent gave you the task in the user message. Your final answer goes back to that agent, not to the user, \
and it can't ask you anything afterwards. You can read and search the project and use the other tools that don't \
change anything; you can't edit files, run commands or ask the user, whatever the instructions above say about that. \
Work through the task, then answer with what the other agent needs: the facts, with file paths and line numbers, and \
short code excerpts only where they matter. If you couldn't find something, say so and say where you looked. \
You get a limited number of steps, so look where the answer is likely instead of reading everything, and answer as \
soon as you can: an answer that says what's still unchecked is worth more than none.";

const COMPACT_PROMPT: &str = "\
The conversation is about to be cut to free the context window, and this summary will replace it. \
Don't call any tools: answer with the summary only, in this structure.

1. Request: what the user asked for, in their words where it matters, including later changes and corrections.
2. Done so far: what you found and changed (files with their paths, functions, commands run and their outcome).
3. Decisions: choices made with the user or by you, and the reasons.
4. Errors and fixes: what went wrong and how it was solved.
5. State now: what you were in the middle of, and the exact next step.
6. Still to do: the remaining steps and open questions.

Keep paths, names, numbers and error messages exact. Leave out what no longer matters.";

/// What the user wrote in `session`, word for word and oldest first: the newest messages that fit
/// in [`KEPT_WORDS`] characters, long ones cut to [`KEPT_MESSAGE`]. A summary paraphrases; what
/// was asked for should survive it as it was said. (The log keeps every message, also the ones
/// before an earlier compaction.)
fn user_words(session: &Session) -> Option<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut left = KEPT_WORDS;
    for e in session.events().iter().rev() {
        let SessionEvent::User { text, hook: None, .. } = e else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let cut: String = text.chars().take(KEPT_MESSAGE).collect();
        let cut = if cut.len() < text.len() { format!("{cut} […]") } else { cut };
        if cut.len() > left && !kept.is_empty() {
            break;
        }
        left = left.saturating_sub(cut.len());
        kept.push(cut);
    }
    if kept.is_empty() {
        return None;
    }
    kept.reverse();
    Some(kept.iter().enumerate().map(|(n, m)| format!("{}. {m}", n + 1)).collect::<Vec<_>>().join("\n\n"))
}

/// The files the conversation changed (`edit`, `write`) and read (`read`), each newest first and
/// at most [`LISTED_FILES`], relative to the project when inside it. From the tool calls in the
/// whole log, so a summary can't lose track of them, also across several summaries.
fn files_of(session: &Session) -> (Vec<String>, Vec<String>) {
    let (mut changed, mut read) = (Vec::<String>::new(), Vec::<String>::new());
    let cwd = Path::new(&session.cwd);
    for e in session.events().iter().rev() {
        let SessionEvent::Assistant { content, .. } = e else { continue };
        for b in content.iter().rev() {
            let Block::ToolUse { name, input, .. } = b else { continue };
            let Some(path) = input.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) else { continue };
            let path = Path::new(path).strip_prefix(cwd).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string());
            let list = match name.as_str() {
                "edit" | "write" => &mut changed,
                "read" => &mut read,
                _ => continue,
            };
            if list.len() < LISTED_FILES && !list.contains(&path) {
                list.push(path);
            }
        }
    }
    read.retain(|p| !changed.contains(p));
    (changed, read)
}

/// The tools the conversation found with `tool_search` or used: when the MCP tools are left out
/// of the requests ([`Registry::declared`]), these go with them from then on. Read from the log, so
/// a chat opened again has them too.
pub fn active_tools(session: &Session) -> HashSet<String> {
    let mut active = HashSet::new();
    let mut searches = HashSet::new();
    for e in session.events() {
        match e {
            SessionEvent::Assistant { content, .. } => {
                for b in content {
                    if let Block::ToolUse { id, name, .. } = b {
                        if name == TOOL_SEARCH {
                            searches.insert(id.clone());
                        } else {
                            active.insert(name.clone());
                        }
                    }
                }
            }
            SessionEvent::ToolResults { results, .. } => {
                for b in results {
                    if let Block::ToolResult { tool_use_id, content, is_error: false } = b
                        && searches.contains(tool_use_id)
                        && let Some(list) = content.lines().next().and_then(|l| l.strip_prefix(FOUND))
                    {
                        active.extend(list.split(", ").map(|n| n.trim().to_string()).filter(|n| !n.is_empty()));
                    }
                }
            }
            _ => {}
        }
    }
    active
}

/// The answer a conversation ends with: the text of the last step that said something, and of the
/// steps before it when the answer was cut off at the output limit and went on from there (the
/// last step alone would be its tail).
pub fn answer_of(events: &[SessionEvent]) -> String {
    let said = |e: &SessionEvent| match e {
        SessionEvent::Assistant { content, .. } => Some(content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect::<String>()),
        _ => None,
    };
    let Some(mut at) = events.iter().rposition(|e| said(e).is_some_and(|t| !t.trim().is_empty())) else { return String::new() };
    let mut parts = vec![said(&events[at]).unwrap_or_default()];
    while at >= 2
        && matches!(&events[at - 1], SessionEvent::User { hook: Some(h), text, .. } if h == NOTE && text == CUT_OFF)
        && let Some(before) = said(&events[at - 2])
    {
        parts.push(before);
        at -= 2;
    }
    parts.reverse();
    parts.concat().trim().to_string()
}

/// `s` with its middle left out when it's longer than `max` characters.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max * 2 / 3).collect();
    let tail: String = s.chars().rev().take(max / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n[… {} characters left out …]\n{tail}", s.chars().count() - max)
}

/// Long strings in a tool call's input (a file's content, a script) clipped to `max` characters.
fn clip_input(v: &Value, max: usize) -> Value {
    match v {
        Value::String(s) => Value::String(clip(s, max)),
        Value::Array(a) => Value::Array(a.iter().map(|x| clip_input(x, max)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), clip_input(x, max))).collect()),
        other => other.clone(),
    }
}

/// The conversation made smaller for the summary request, when the whole of it doesn't fit the
/// window anymore. Each level leaves out more: 1 and 2 clip tool results, tool inputs and long
/// messages (a pasted log) and drop reasoning and images, 3 and 4 also keep only the newer half,
/// then the newest quarter. Every tool call keeps its result, and the first message stays the
/// user's.
fn shrink(messages: &[Message], level: u8) -> Vec<Message> {
    if level == 0 {
        return messages.to_vec();
    }
    let (result, input, said) = if level == 1 { (4_000, 2_000, 8_000) } else { (600, 300, 2_000) };
    let mut out: Vec<Message> = messages
        .iter()
        .map(|m| Message {
            role: m.role,
            content: m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::Thinking { .. } => None,
                    Block::Image { .. } => Some(Block::Text { text: "[an image]".into() }),
                    Block::ToolResult { tool_use_id, content, is_error } => Some(Block::ToolResult { tool_use_id: tool_use_id.clone(), content: clip(content, result), is_error: *is_error }),
                    Block::ToolUse { id, name, input: i } => Some(Block::ToolUse { id: id.clone(), name: name.clone(), input: clip_input(i, input) }),
                    Block::Text { text } => Some(Block::Text { text: clip(text, said) }),
                })
                .collect(),
        })
        .filter(|m| !m.content.is_empty())
        .collect();
    if level >= 3 {
        let cut = if level == 3 { out.len() / 2 } else { out.len() - out.len() / 4 };
        // from one of the model's messages on: its tool calls have their results right after it
        let from = (cut..out.len()).find(|&i| out[i].role == Role::Assistant).or_else(|| (0..cut).rev().find(|&i| out[i].role == Role::Assistant));
        if let Some(from) = from.filter(|&f| f > 0) {
            let mut tail = vec![Message::user_text("[The earlier part of the conversation is left out here: it no longer fits the context window.]")];
            tail.extend(out.drain(from..));
            out = tail;
        }
    }
    out
}

/// The provider's "too long" (`e`) after the conversation was already summarized: what's left
/// can't be made smaller, so the error says what would help.
fn too_small(e: &Error) -> Error {
    let m = match e {
        Error::Provider(m) => m.clone(),
        other => other.to_string(),
    };
    Error::Provider(format!("{m} (the conversation was just summarized and still doesn't fit: the system prompt, the project's rules, the tools and the work at hand need more than this model's context window. Use a model with a larger window, or fewer rule files and MCP servers.)"))
}

/// The characters the tools' definitions add to every request.
fn specs_chars(specs: &[ToolSpec]) -> usize {
    specs.iter().map(|s| s.name.len() + s.description.len() + s.input_schema.to_string().len()).sum()
}

/// About how many characters a request with `messages` has (see [`Session::weight`]).
fn chars_of(messages: &[Message]) -> usize {
    messages.iter().flat_map(|m| &m.content).map(crate::session::block_chars).sum()
}

/// The latest `todo_write` list in `session` as `[x]` / `[>]` / `[ ]` lines, if a step is still
/// open (a finished list has nothing to carry on).
fn open_todos(session: &Session) -> Option<String> {
    let input = session.events().iter().rev().find_map(|e| match e {
        SessionEvent::Assistant { content, .. } => content.iter().rev().find_map(|b| match b {
            Block::ToolUse { name, input, .. } if name == TODO_WRITE => Some(input),
            _ => None,
        }),
        _ => None,
    })?;
    let todos = input.get("todos")?.as_array()?;
    let mut open = false;
    let lines: Vec<String> = todos.iter().filter_map(|t| {
        let content = t.get("content")?.as_str()?.trim();
        let mark = match t.get("status").and_then(Value::as_str) {
            Some("completed") => "[x]",
            Some("in_progress") => { open = true; "[>]" }
            _ => { open = true; "[ ]" }
        };
        (!content.is_empty()).then(|| format!("{mark} {content}"))
    }).collect();
    open.then(|| lines.join("
"))
}

/// One model step. [`Llm`] is the real one; tests use a scripted model.
#[async_trait]
pub trait Model: Send + Sync {
    fn name(&self) -> String;
    /// The most tokens it's asked to answer with (`max_tokens`): providers count them against
    /// the context window, so that much of it has to stay free. 0 when it isn't known.
    fn max_output(&self) -> u64 {
        0
    }
    async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply>;
}

#[async_trait]
impl Model for Llm {
    fn name(&self) -> String {
        self.config.model.clone()
    }
    fn max_output(&self) -> u64 {
        self.config.max_tokens as u64
    }
    async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply> {
        Llm::step(self, req, on).await
    }
}

/// What happens during a turn, for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    Text { text: String },
    Thinking { text: String },
    /// The model started a tool call; its input follows in `ToolCall`.
    ToolStart { id: String, name: String },
    /// A step finished: the full assistant message and its token use.
    Step { content: Vec<Block>, usage: Usage },
    /// The conversation is being summarized (automatically, or `/compact`).
    Compacting { tokens_before: u64 },
    /// The summary that now stands for the conversation before it.
    Compacted { summary: String, tokens_before: u64, usage: Usage },
    /// Summarizing didn't work (`message`): the conversation is as it was.
    CompactFailed { message: String },
    /// The model's step is tried again (`attempt` of `of`): the provider was busy or the
    /// connection broke (`reason`). What the step streamed so far (text, thinking, tool starts)
    /// is void: it comes again.
    Retry { attempt: u32, of: u32, reason: String },
    /// A tool call is about to run (after approval, if it needed one).
    ToolCall { id: String, name: String, summary: String, input: Value },
    ToolResult { id: String, content: String, is_error: bool },
    /// The helper started by `subagent` call `id` made a tool call.
    HelperTool { id: String, name: String, input: Value },
    /// The helper started by `subagent` call `id` finished a step; its tokens count for the chat.
    HelperStep { id: String, usage: Usage },
    /// A hook has something to tell the user: its `systemMessage`, that it failed, or (Stop) why
    /// the agent goes on. With `event` [`NOTE`] it's the harness's own: what it told the model in
    /// the user's place (that its answer was empty or cut off).
    Hook { event: String, message: String },
    /// A message the user sent while the agent worked is now part of the conversation, or
    /// (`blocked`) a UserPromptSubmit hook refused it: it's dropped, and a `Hook` event says why.
    UserMessage { text: String, blocked: bool },
}

/// Messages the user sends while a turn runs. The agent reads them before its next step (also
/// when it was about to answer), so the user doesn't have to stop it to say something.
#[derive(Clone, Default)]
pub struct Inbox(Arc<std::sync::Mutex<Vec<Queued>>>);

/// A queued message: its text, any images, and context for the model only (what a slash command
/// asks for, see [`Agent::turn_with`]).
pub type Queued = (String, Vec<Block>, Vec<String>);

impl Inbox {
    /// Queue a message (with any images) for the running turn.
    pub fn push(&self, text: String, images: Vec<Block>) {
        self.push_with(text, images, vec![]);
    }
    /// [`Inbox::push`] with context for the model only (the log keeps `text` as it is).
    pub fn push_with(&self, text: String, images: Vec<Block>, context: Vec<String>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push((text, images, context));
    }
    /// Everything queued, oldest first; the inbox is empty afterwards.
    pub fn take(&self) -> Vec<Queued> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnEnd {
    /// The model answered.
    Answered,
    /// [`Agent::max_steps`] steps without an answer; the user can say "continue".
    StepLimit,
}

/// Stops a running turn from another task.
#[derive(Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Default for Cancel {
    fn default() -> Self {
        Cancel(Arc::new(watch::channel(false).0))
    }
}

impl Cancel {
    pub fn cancel(&self) {
        // `send` stores nothing when no one is subscribed (e.g. a ticket run polling
        // `is_cancelled`), so the stop would be lost.
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
}

pub struct Agent {
    pub model: Arc<dyn Model>,
    pub tools: Registry,
    pub policy: Policy,
    pub approver: Arc<dyn Approver>,
    pub system: String,
    /// The model's context window in tokens; when known, the conversation is summarized at
    /// [`COMPACT_AT`] of it.
    pub context_window: Option<u64>,
    /// Commands the user set up to run at points of the loop (see [`crate::hooks`]); none by default.
    pub hooks: Arc<Hooks>,
    /// Commands (`shell`, `git`) run in the OS sandbox, where they can write only in the project
    /// (see [`crate::sandbox`]). They still ask as commands: it limits writes, not what a command
    /// reads or sends over the network.
    pub sandbox: bool,
    /// Messages sent while a turn runs (the app keeps a clone to put them in).
    pub inbox: Inbox,
    /// Steps per turn before it stops with [`TurnEnd::StepLimit`]; 0 = no limit. [`MAX_STEPS`]
    /// by default.
    pub max_steps: u32,
    /// Tools the user allowed for the rest of the session.
    allowed: HashSet<String>,
    /// A helper started by `subagent` (that call's id): it only looks, never asks, and starts no
    /// helpers itself.
    helper: Option<String>,
    /// SessionStart hooks ran (on the first turn).
    started: bool,
    /// A tool hook said `continue: false`: the step's other calls don't run, and the turn ends
    /// with this once their results are in.
    halt: Option<String>,
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, tools: Registry, policy: Policy, approver: Arc<dyn Approver>, system: String) -> Agent {
        Agent { model, tools, policy, approver, system, context_window: None, hooks: Arc::default(), sandbox: false, inbox: Inbox::default(), max_steps: MAX_STEPS, allowed: HashSet::new(), helper: None, started: false, halt: None }
    }

    /// Run the hooks of `event` (see [`Hooks::run`]); what they say to the user is sent on. Stop
    /// doesn't wait for them: they're dropped (and killed), and nothing they'd say counts.
    async fn hook(&self, session: &Session, event: &str, names: Option<&[&str]>, fields: Value, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Outcome {
        if !self.hooks.has(event) {
            return Outcome::default();
        }
        let ctx = self.hook_context(session);
        let o = tokio::select! {
            o = self.hooks.run(event, names, &ctx, fields) => o,
            _ = cancel.cancelled() => return Outcome::default(),
        };
        for message in &o.notes {
            on(AgentEvent::Hook { event: event.to_string(), message: message.clone() });
        }
        o
    }

    fn hook_context<'a>(&'a self, session: &'a Session) -> hooks::Context<'a> {
        hooks::Context {
            session_id: &session.id,
            transcript: session.path(),
            cwd: Path::new(&session.cwd),
            permission_mode: match self.policy {
                Policy::Ask => "default",
                Policy::AutoEdit => "acceptEdits",
                Policy::Bypass => "bypassPermissions",
                Policy::Plan => "plan",
            },
            model: self.model.name(),
            agent: self.helper.as_deref(),
        }
    }

    /// Summarize the conversation so far and continue from the summary (`/compact`). `focus` says
    /// what the summary should keep in particular.
    pub async fn compact(&mut self, session: &mut Session, focus: Option<&str>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<()> {
        self.compact_now(session, focus, false, true, cancel, on).await.map(|_| ())
    }

    /// [`Agent::compact`]; `mid_turn`: the agent is in the middle of its work and goes on right
    /// after. `keep`: the user's messages nothing answered yet stay as they are after the summary
    /// when they're small enough ([`KEPT_UNANSWERED`]). Returns whether the conversation was
    /// summarized (a PreCompact hook can stop the automatic one).
    async fn compact_now(&mut self, session: &mut Session, focus: Option<&str>, mid_turn: bool, keep: bool, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<bool> {
        let messages = session.messages();
        if messages.is_empty() {
            return Err(Error::Provider("There's nothing to compact yet.".into()));
        }
        // `/compact` passes a focus (maybe empty); the automatic one doesn't
        let trigger = if focus.is_some() { "manual" } else { "auto" };
        let pre = self.hook(session, "PreCompact", Some(&[trigger]), json!({ "trigger": trigger, "custom_instructions": focus.unwrap_or("") }), cancel, on).await;
        if let Some(why) = pre.stop.or(pre.block) {
            // the automatic one goes on without compacting
            return if trigger == "manual" { Err(Error::Hook(format!("A PreCompact hook stopped the compaction: {why}"))) } else { Ok(false) };
        }
        let tokens_before = session.context_tokens();
        on(AgentEvent::Compacting { tokens_before });
        let specs = self.tools.declared(&active_tools(session));
        let (mut summary, usage) = match self.summarize(&session.id, &messages, &specs, focus, session.last_summary().is_some(), cancel).await {
            Ok(s) => s,
            Err(e) => {
                on(AgentEvent::CompactFailed { message: e.to_string() });
                return Err(e);
            }
        };
        // A summary paraphrases. What must survive it as it was goes in word for word: what the
        // user asked for, and the checklist (it lives in a `todo_write` call the summary
        // replaces), so the model goes on with the same task and the same plan.
        if let Some(said) = user_words(session) {
            summary.push_str(&format!("\n\n## What the user wrote, word for word (oldest first)\n{said}"));
        }
        let (changed, read) = files_of(session);
        if !changed.is_empty() || !read.is_empty() {
            summary.push_str("\n\n## Files in this conversation (newest first)");
            if !changed.is_empty() {
                summary.push_str(&format!("\nChanged: {}", changed.join(", ")));
            }
            if !read.is_empty() {
                summary.push_str(&format!("\nRead: {}", read.join(", ")));
            }
        }
        if let Some(list) = open_todos(session) {
            summary.push_str(&format!("

## Your checklist (`todo_write`) when the conversation was summarized
{list}

Go on from the step marked [>] and keep the list up to date with `todo_write`."));
        }
        if mid_turn {
            summary.push_str(MID_TURN);
        }
        // The message that started this turn (or came while it ran) isn't history yet: it stays,
        // with its images, instead of being read by the model as a paraphrase of itself.
        // The newest steps stay as they were, when they're small enough: what the model just read
        // or ran is what it needs next (a tail takes the unanswered messages in too).
        let tail = match self.context_window {
            Some(w) if keep => session.tail_within(((w as f64 * TAIL_SHARE) as usize * 4).min(TAIL_CHARS)),
            _ => 0,
        };
        let unanswered = session.unanswered();
        let fits = unanswered.iter().sum::<usize>() <= self.context_window.map_or(KEPT_UNANSWERED, |w| KEPT_UNANSWERED.min(w as usize));
        let kept = if keep && fits && tail == 0 { unanswered.len() } else { 0 };
        session.append(SessionEvent::Compacted { summary: summary.clone(), model: self.model.name(), usage, tokens_before, kept, tail }).await?;
        on(AgentEvent::Compacted { summary, tokens_before, usage });
        self.hook(session, "PostCompact", Some(&[trigger]), json!({ "trigger": trigger }), cancel, on).await;
        Ok(true)
    }

    /// Ask the model for the summary of `messages`: its text, and the tokens asking took. A
    /// reply without text (a tool call instead, or nothing) is asked for once more. When the
    /// conversation is too long even for this request, it's cut down step by step ([`shrink`])
    /// until it fits: a summary of most of it beats a chat that can't go on.
    /// `update`: the conversation starts with an earlier summary, which the new one carries on.
    #[allow(clippy::too_many_arguments)]
    async fn summarize(&self, key: &str, messages: &[Message], specs: &[ToolSpec], focus: Option<&str>, update: bool, cancel: &Cancel) -> Result<(String, Usage)> {
        // Same system prompt and tools as every step (`specs`), so the provider's cache still applies.
        let system = self.system_now();
        let overhead = system.len() + specs_chars(specs);
        let mut usage = Usage::default();
        let mut level = 0;
        // what can be told ahead: with a known window, don't send what can't fit
        if let Some(room) = self.context_window.map(|w| w.saturating_sub(self.model.max_output())).filter(|r| *r > 0) {
            while level < SHRINK_LEVELS && ((chars_of(&shrink(messages, level)) + overhead) / 4) as u64 > room {
                level += 1;
            }
        }
        let mut again = false;
        loop {
            let mut ask = COMPACT_PROMPT.to_string();
            if update {
                ask.push_str(UPDATE_SUMMARY);
            }
            if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
                ask.push_str(&format!("\n\nThe user wants the summary to focus on: {f}"));
            }
            if again {
                ask.push_str(EMPTY_SUMMARY);
            }
            let mut messages = shrink(messages, level);
            match messages.last_mut() {
                Some(m) if m.role == Role::User => m.content.push(Block::Text { text: ask }),
                _ => messages.push(Message { role: Role::User, content: vec![Block::Text { text: ask }] }),
            }
            let req = Request { system: &system, messages: &messages, tools: specs, cache_key: Some(key) };
            let mut ignore = |_| {};
            let reply = tokio::select! {
                r = self.model.step(&req, &mut ignore) => r,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            let reply = match reply {
                Ok(r) => r,
                Err(e) if is_overflow(&e) && level < SHRINK_LEVELS => {
                    level += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            usage += reply.usage;
            let summary: String = reply.content.iter().filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("").trim().to_string();
            if !summary.is_empty() {
                return Ok((summary, usage));
            }
            if again {
                return Err(Error::Provider("The model returned an empty summary; the conversation was left as it was.".into()));
            }
            again = true;
        }
    }

    /// Up to how many tokens the conversation may grow before it's summarized: [`COMPACT_AT`] of
    /// the window, or less (down to [`COMPACT_FLOOR`]) when the model's answer (`max_tokens`,
    /// which most providers count against the window) needs more room than that leaves. None
    /// when the window isn't known.
    fn limit(&self) -> Option<u64> {
        let window = self.context_window? as f64;
        let room = window - self.model.max_output() as f64 - window * MARGIN;
        Some((window * COMPACT_AT).min(room).max(window * COMPACT_FLOOR) as u64)
    }

    /// About how many tokens the next request takes. `overhead`: the characters of the system
    /// prompt and the tools, which count when the provider doesn't (the log doesn't have them).
    fn context_now(&self, session: &Session, overhead: usize) -> u64 {
        let w = session.weight();
        match w.counted {
            Some(_) => w.tokens(),
            None => ((w.chars + overhead) / 4) as u64,
        }
    }

    /// Whether to summarize before the next step: the conversation has outgrown the limit, and
    /// enough of it is new since the last summary for another one to free anything.
    fn should_compact(&self, session: &Session, overhead: usize) -> bool {
        let (Some(limit), Some(window)) = (self.limit(), self.context_window) else { return false };
        let now = self.context_now(session, overhead);
        // (before the model's first step there's no conversation to summarize, only a message)
        if now <= limit || session.steps_since_compaction() == 0 {
            return false;
        }
        let Some(summary) = session.last_summary() else { return true };
        // what a compacted conversation weighs at the least: the system prompt, tools and summary
        let base = session.compacted_base().unwrap_or(((summary.len() + overhead) / 4) as u64);
        now.saturating_sub(base) as f64 >= window as f64 * WORTH_COMPACTING
    }

    /// Tell the model something in the user's place (`text`, as the next message), and the chat
    /// that it was told.
    async fn nudge(&self, session: &mut Session, text: &str, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<()> {
        on(AgentEvent::Hook { event: NOTE.into(), message: text.to_string() });
        session.append(SessionEvent::User { text: text.to_string(), images: vec![], context: vec![], hook: Some(NOTE.into()) }).await
    }

    /// Run one turn for `user_text`, writing everything to `session`.
    pub async fn turn(&mut self, session: &mut Session, user_text: &str, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        self.turn_with_images(session, user_text, vec![], cancel, on).await
    }

    /// A turn whose message carries images (`Block::Image`).
    pub async fn turn_with_images(&mut self, session: &mut Session, user_text: &str, images: Vec<Block>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        self.turn_with(session, user_text, images, vec![], cancel, on).await
    }

    /// A turn whose message comes with `extra` for the model only (what one of the user's slash
    /// commands asks for, see [`crate::commands`]): the log and the chat keep the message as
    /// typed, the model gets both.
    #[allow(clippy::too_many_arguments)]
    pub async fn turn_with(&mut self, session: &mut Session, user_text: &str, images: Vec<Block>, extra: Vec<String>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        // hooks on the user's messages (not a helper's prompt): SessionStart on the first turn, then
        // UserPromptSubmit; both may add context, and the second may block the message (it's not
        // recorded then)
        let mut context = extra;
        if self.helper.is_none() {
            if !self.started {
                let source = if session.events().iter().any(|e| matches!(e, SessionEvent::User { .. })) { "resume" } else { "startup" };
                context.extend(self.hook(session, "SessionStart", Some(&[source]), json!({ "source": source }), cancel, on).await.context);
                self.started = true;
            }
            let o = self.hook(session, "UserPromptSubmit", None, json!({ "prompt": user_text }), cancel, on).await;
            if let Some(why) = o.stop.or(o.block) {
                return Err(Error::Hook(format!("A hook blocked this message: {why}")));
            }
            context.extend(o.context);
        }
        session.append(SessionEvent::User { text: user_text.to_string(), images, context, hook: None }).await?;
        let res = self.run(session, cancel, on).await;
        match &res {
            Err(Error::Cancelled) => session.append(SessionEvent::Interrupted { reason: "Stopped by the user".into() }).await?,
            Err(e) => session.append(SessionEvent::Interrupted { reason: e.to_string() }).await?,
            Ok(TurnEnd::StepLimit) => session.append(SessionEvent::Interrupted { reason: format!("Stopped after {} steps", self.max_steps) }).await?,
            Ok(TurnEnd::Answered) => {}
        }
        res
    }

    async fn run(&mut self, session: &mut Session, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        let ctx = ToolCtx { cwd: session.cwd.clone().into(), sandbox: self.sandbox };
        // the last call and how many times in a row it was made (repeat guard)
        let mut last: Option<(String, Value)> = None;
        let mut same = 0;
        let system = self.system_now();
        // a Stop hook already sent the agent back to work this turn
        let mut stop_hook_active = false;
        let mut steps = 0u32;
        // the model was just told that its answer was empty (a second one in a row ends the turn)
        let mut told_empty = false;
        // answers in a row that were cut off at the output limit
        let mut cut_off = 0u32;
        // an automatic summary failed: the step before which it isn't tried again
        let mut compact_from = 0u32;
        // the provider refused the conversation as too long this many times in a row with hardly
        // a step since the summary before
        let mut squeezed = 0u32;
        // the model had answered, and came back only for messages sent meanwhile
        let mut answered = false;
        loop {
            // the tools this step declares: with many MCP tools, the ones found so far (see
            // `Registry::declared`)
            let specs = self.tools.declared(&active_tools(session));
            let overhead = system.len() + specs_chars(&specs);
            if self.max_steps > 0 && steps >= self.max_steps {
                // A helper's answer is all its caller gets: at the limit it's asked for what it
                // found, instead of throwing its work away.
                if self.helper.is_some() {
                    self.wrap_up(session, &system, &specs, cancel, on).await?;
                }
                return Ok(TurnEnd::StepLimit);
            }
            steps += 1;
            // what the user sent meanwhile goes in before the step (after the tool results), past
            // the UserPromptSubmit hooks like a message that starts a turn
            let mut queued = self.inbox.take().into_iter();
            let mut read = 0;
            while let Some((text, images, extra)) = queued.next() {
                let o = self.hook(session, "UserPromptSubmit", None, json!({ "prompt": text }), cancel, on).await;
                if cancel.is_cancelled() {
                    // stopped before its hooks said yes: this one and the rest stay unread (the app
                    // gives them back to the chat box), in the order they were sent
                    let later = self.inbox.take();
                    for (t, i, c) in std::iter::once((text, images, extra)).chain(queued).chain(later) {
                        self.inbox.push_with(t, i, c);
                    }
                    return Err(Error::Cancelled);
                }
                if let Some(why) = o.stop.or(o.block) {
                    on(AgentEvent::Hook { event: "UserPromptSubmit".into(), message: format!("A hook blocked a message you sent: {why}") });
                    on(AgentEvent::UserMessage { text, blocked: true });
                    continue;
                }
                session.append(SessionEvent::User { text: text.clone(), images, context: extra.into_iter().chain(o.context).collect(), hook: None }).await?;
                on(AgentEvent::UserMessage { text, blocked: false });
                read += 1;
            }
            // every one of them was refused by a hook: its answer stands, and there's nothing
            // new to ask the model about (a request ending with its own message isn't one)
            if std::mem::take(&mut answered) && read == 0 {
                return Ok(TurnEnd::Answered);
            }
            if steps >= compact_from && self.should_compact(session, overhead) {
                match self.compact_now(session, None, true, true, cancel, on).await {
                    Ok(_) => {}
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    // The step may still fit: go on, and try the summary again a few steps later.
                    // If it doesn't fit, the provider says so and it's summarized for that below.
                    // (the chat already heard why: `CompactFailed`)
                    Err(_) => compact_from = steps + COMPACT_AGAIN_AFTER,
                }
            }
            let messages = session.messages();
            let req = Request { system: &system, messages: &messages, tools: &specs, cache_key: Some(&session.id) };
            let mut forward = |d: Delta| {
                on(match d {
                    Delta::Text(text) => AgentEvent::Text { text },
                    Delta::Thinking(text) => AgentEvent::Thinking { text },
                    Delta::ToolStart { id, name } => AgentEvent::ToolStart { id, name },
                    Delta::Retry { attempt, of, reason } => AgentEvent::Retry { attempt, of, reason },
                })
            };
            let reply = tokio::select! {
                r = self.model.step(&req, &mut forward) => r,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            let mut reply = match reply {
                Ok(reply) => reply,
                // The conversation doesn't fit the model's window (the window isn't known, was
                // guessed too large, or the last results were bigger than estimated): summarize
                // it and take the step again. Each way of making it smaller is tried once: first
                // the steps since the last summary, then the user's unanswered messages that
                // summary kept whole. If it still doesn't fit, nothing more can be taken out,
                // and the turn ends with the provider's message.
                Err(e) if is_overflow(&e) => {
                    let fresh = session.steps_since_compaction() == 0;
                    let keep = if !fresh {
                        // A summary every step or two isn't work, it's forgetting: the window
                        // is too small for this, and going on would only spend tokens.
                        squeezed = if session.last_summary().is_some() && session.steps_since_compaction() < 3 { squeezed + 1 } else { 0 };
                        if squeezed >= MAX_SQUEEZED {
                            return Err(too_small(&e));
                        }
                        true
                    } else if session.last_summary().is_some() && (!session.unanswered().is_empty() || session.last_tail() > 0) {
                        // what the summary kept as it was (the newest steps, the user's
                        // messages) goes into a summary too
                        false
                    } else if session.last_summary().is_some() {
                        return Err(too_small(&e));
                    } else {
                        // a first message that's too long by itself: there's nothing to summarize
                        return Err(e);
                    };
                    match self.compact_now(session, None, true, keep, cancel, on).await {
                        Ok(true) => {
                            steps -= 1;
                            continue;
                        }
                        Ok(false) => return Err(e),
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(c) => return Err(Error::Provider(format!("the conversation no longer fits the model's context window ({e}), and it couldn't be summarized to make room ({c})"))),
                    }
                }
                Err(e) => return Err(e),
            };
            // A call whose arguments were cut off can't run. Its raw text would go back to the
            // model with every later step, so only its start is kept.
            for b in &mut reply.content {
                if let Block::ToolUse { input, .. } = b
                    && let Some(raw) = input.get(UNPARSED).and_then(Value::as_str)
                    && raw.chars().count() > 400
                {
                    *input = json!({ UNPARSED: format!("{}… ({} characters)", raw.chars().take(400).collect::<String>(), raw.chars().count()) });
                }
            }
            session.append(SessionEvent::Assistant { content: reply.content.clone(), model: self.model.name(), usage: reply.usage }).await?;
            on(AgentEvent::Step { content: reply.content.clone(), usage: reply.usage });

            let calls: Vec<(String, String, Value)> = reply.content.iter().filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            }).collect();
            if calls.is_empty() {
                // a message came while it answered: it reads that before ending
                if !self.inbox.is_empty() {
                    answered = true;
                    continue;
                }
                let answer: String = reply.content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect();
                // The answer stopped at the output limit: it isn't finished, so the model goes on
                // with it (a few times: a model that never ends would otherwise never stop).
                // (An answer with no text has nothing to go on from: that's the empty one below.)
                if reply.truncated && !answer.trim().is_empty() && cut_off < MAX_CUT_OFF {
                    cut_off += 1;
                    self.nudge(session, CUT_OFF, on).await?;
                    continue;
                }
                // Nothing at all (no text, no call): a hiccup of the provider more often than an
                // answer. It's asked once; a second empty reply in a row ends the turn as before.
                if answer.trim().is_empty() && !told_empty {
                    told_empty = true;
                    self.nudge(session, EMPTY_REPLY, on).await?;
                    continue;
                }
                told_empty = false;
                // Stop hooks (SubagentStop in a helper) may send the agent back to work
                let event = if self.helper.is_some() { "SubagentStop" } else { "Stop" };
                let names: &[&str] = &["helper"];
                let o = self.hook(session, event, self.helper.as_ref().map(|_| names), json!({ "stop_hook_active": stop_hook_active, "last_assistant_message": answer }), cancel, on).await;
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let (None, Some(why)) = (o.stop, o.block) {
                    stop_hook_active = true;
                    let text = format!("{event} hook feedback:\n{why}");
                    on(AgentEvent::Hook { event: event.to_string(), message: text.clone() });
                    session.append(SessionEvent::User { text, images: vec![], context: o.context, hook: Some(event.to_string()) }).await?;
                    continue;
                }
                return Ok(TurnEnd::Answered);
            }
            (cut_off, told_empty) = (0, false);
            // the repeat guard, decided before anything runs
            let mut repeated = HashSet::new();
            for (id, name, input) in &calls {
                let key = (name.clone(), input.clone());
                same = if last.as_ref() == Some(&key) { same + 1 } else { 1 };
                last = Some(key);
                if same > MAX_SAME_CALL {
                    repeated.insert(id.clone());
                }
            }
            // Helpers first, all at once: they only look, so they can't get in the way of the
            // other calls. Each result is reported as soon as it's in.
            let jobs: Vec<(String, Value)> = calls.iter().filter(|(id, name, _)| name == SUBAGENT && self.helper.is_none() && !repeated.contains(id)).map(|(id, _, input)| (id.clone(), input.clone())).collect();
            let mut helped = if cancel.is_cancelled() { HashMap::new() } else { self.helpers(session, jobs, cancel, on).await };
            let mut results = Vec::new();
            let mut usage = Usage::default();
            let mut took_ms = Vec::new();
            let mut images = Vec::new();
            // Calls that only look (read, glob, grep, read-only git) and stand next to each other
            // run at the same time: one after the other, a step that reads ten files waits for
            // each. They go through approval and the hooks one by one as ever, and their results
            // are in call order.
            // (Not with PostToolUse hooks: one that stops the agent must find the calls after it
            // not run yet, and a hook may not expect to run twice at once.)
            let hooked = self.hooks.has("PostToolUse") || self.hooks.has("PostToolUseFailure");
            let together: Vec<bool> = calls
                .iter()
                .map(|(id, name, input)| !hooked && !repeated.contains(id) && input.get(UNPARSED).is_none() && self.tools.get(name).is_some_and(|t| t.concurrent(input, &ctx)))
                .collect();
            let mut calls = calls.into_iter().enumerate().peekable();
            while let Some((n, (id, name, input))) = calls.next() {
                if let Some((out, used, took)) = helped.remove(&id) {
                    usage += used;
                    took_ms.push(took);
                    results.push(Block::ToolResult { tool_use_id: id, content: out.content, is_error: out.is_error });
                    continue;
                }
                if together[n] && calls.peek().is_some_and(|(m, _)| together[*m]) {
                    let mut batch = vec![(id, name, input)];
                    while let Some((_, call)) = calls.next_if(|(m, _)| together[*m]) {
                        batch.push(call);
                    }
                    let mut prepared = Vec::new();
                    for (id, name, input) in batch {
                        let p = if cancel.is_cancelled() {
                            Prepared::Done(ToolOutput::err("Not run: stopped by the user."))
                        } else if self.halt.is_some() {
                            Prepared::Done(ToolOutput::err("Not run: a hook stopped the agent."))
                        } else {
                            self.prepare(session, &ctx, &id, &name, input, cancel, on).await?
                        };
                        prepared.push((id, p));
                    }
                    let (this, shared, ctx) = (&*self, &*session, &ctx);
                    let runs = prepared.into_iter().map(|(id, p)| async move {
                        let started = std::time::Instant::now();
                        // (`on` is the loop's: what the hooks say is passed on once all are done)
                        let mut said = Vec::new();
                        let (out, halt) = match p {
                            Prepared::Done(out) => (out, None),
                            Prepared::Run(_) if cancel.is_cancelled() => (ToolOutput::err("Not run: stopped by the user."), None),
                            Prepared::Run(ready) => this.execute(shared, ctx, ready, cancel, &mut |e| said.push(e)).await,
                        };
                        (id, out, halt, said, started.elapsed().as_millis() as u64)
                    });
                    for (id, out, halt, said, took) in futures::future::join_all(runs).await {
                        said.into_iter().for_each(&mut *on);
                        if halt.is_some() && self.halt.is_none() {
                            self.halt = halt;
                        }
                        took_ms.push(took);
                        on(AgentEvent::ToolResult { id: id.clone(), content: out.content.clone(), is_error: out.is_error });
                        results.push(Block::ToolResult { tool_use_id: id, content: out.content, is_error: out.is_error });
                        images.extend(out.images);
                    }
                    continue;
                }
                let started = std::time::Instant::now();
                let out = if cancel.is_cancelled() {
                    ToolOutput::err("Not run: stopped by the user.")
                } else if self.halt.is_some() {
                    ToolOutput::err("Not run: a hook stopped the agent.")
                } else if repeated.contains(&id) {
                    ToolOutput::err(format!(
                        "Not run: you already made this exact `{name}` call {MAX_SAME_CALL} times in a row. Doing it again won't help: try something different, or tell the user what's in the way."
                    ))
                } else if reply.truncated && input.get(UNPARSED).is_some() {
                    ToolOutput::err(CUT_OFF_CALL)
                } else {
                    self.call(session, &ctx, &id, &name, input, cancel, on).await?
                };
                took_ms.push(started.elapsed().as_millis() as u64);
                on(AgentEvent::ToolResult { id: id.clone(), content: out.content.clone(), is_error: out.is_error });
                results.push(Block::ToolResult { tool_use_id: id, content: out.content, is_error: out.is_error });
                images.extend(out.images);
            }
            // Tools' images go after all the results (Anthropic wants the results first).
            results.extend(images);
            session.append(SessionEvent::ToolResults { results, usage, took_ms }).await?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some(why) = self.halt.take() {
                return Err(Error::Hook(why));
            }
        }
    }

    /// A helper is out of steps: one last request for its answer from what it found. Only what
    /// it says counts (a tool call here can't run anymore).
    async fn wrap_up(&mut self, session: &mut Session, system: &str, specs: &[ToolSpec], cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<()> {
        let ask = format!(
            "You've used all {} steps you get, so no more tool calls will run. Answer now from what you found so far: the facts, with file paths and line numbers. Say plainly what you didn't get to, so whoever asked can look there next.",
            self.max_steps
        );
        self.nudge(session, &ask, on).await?;
        let messages = session.messages();
        // the same tools as every step (providers want them for a conversation with tool calls)
        let req = Request { system, messages: &messages, tools: specs, cache_key: Some(&session.id) };
        let mut ignore = |_| {};
        let reply = tokio::select! {
            r = self.model.step(&req, &mut ignore) => r?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        let content: Vec<Block> = reply.content.into_iter().filter(|b| !matches!(b, Block::ToolUse { .. })).collect();
        session.append(SessionEvent::Assistant { content: content.clone(), model: self.model.name(), usage: reply.usage }).await?;
        on(AgentEvent::Step { content, usage: reply.usage });
        Ok(())
    }

    /// The system prompt for this turn: plan mode adds what it means.
    fn system_now(&self) -> String {
        if self.policy == Policy::Plan { format!("{}\n\n{PLAN_MODE}", self.system) } else { self.system.clone() }
    }

    /// Run `subagent` calls (id, input), up to [`MAX_HELPERS`] at a time, each in its own session
    /// in `<session>.agents/`. Returns each result with the tokens its helper used and how long it
    /// took (ms).
    async fn helpers(&self, session: &Session, jobs: Vec<(String, Value)>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> HashMap<String, (ToolOutput, Usage, u64)> {
        let mut done = HashMap::new();
        if jobs.is_empty() {
            return done;
        }
        // the helpers' events come through a channel: `on` can only be called from here
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runs: Vec<_> = jobs
            .into_iter()
            .map(|(id, input)| {
                let tx = tx.clone();
                async move {
                    // from when it gets a slot (not while it waits for one)
                    let started = std::time::Instant::now();
                    let (out, usage) = self.helper(session, &id, &input, cancel, tx).await;
                    (id, out, usage, started.elapsed().as_millis() as u64)
                }
            })
            .collect();
        drop(tx);
        let mut runs = futures::stream::iter(runs).buffer_unordered(MAX_HELPERS);
        loop {
            tokio::select! {
                Some(e) = rx.recv() => on(e),
                next = runs.next() => {
                    let Some((id, out, usage, took)) = next else { break };
                    while let Ok(e) = rx.try_recv() {
                        on(e);
                    }
                    let out = fit(session, &id, SUBAGENT, out).await;
                    on(AgentEvent::ToolResult { id: id.clone(), content: out.content.clone(), is_error: out.is_error });
                    done.insert(id, (out, usage, took));
                }
            }
        }
        done
    }

    /// One helper: a new agent with this one's model and the tools that only look, answering
    /// `input.prompt` in a new session in `<parent>.agents/`. Boxed, because a turn runs helpers
    /// and a helper runs a turn.
    fn helper<'a>(&'a self, parent: &'a Session, id: &'a str, input: &'a Value, cancel: &'a Cancel, tx: mpsc::UnboundedSender<AgentEvent>) -> BoxFuture<'a, (ToolOutput, Usage)> {
        Box::pin(async move {
            let (dir, cwd) = (parent.path().with_extension("agents"), parent.cwd.as_str());
            let mut input = input.clone();
            // PreToolUse hooks on the call itself (as `Task`): they may block it or change the prompt
            let foreign = |input: &Value| {
                let (tool_name, tool_input) = hooks::foreign_tool(SUBAGENT, input, Path::new(cwd));
                json!({ "tool_name": tool_name, "tool_input": tool_input, "tool_use_id": id })
            };
            let names = hooks::tool_names(SUBAGENT);
            let mut pre = Outcome::default();
            if self.hooks.has("PreToolUse") {
                let ctx = self.hook_context(parent);
                pre = tokio::select! {
                    o = self.hooks.run("PreToolUse", Some(&names), &ctx, foreign(&input)) => o,
                    _ = cancel.cancelled() => return (ToolOutput::err("Not run: stopped by the user."), Usage::default()),
                };
                for message in pre.notes.drain(..) {
                    let _ = tx.send(AgentEvent::Hook { event: "PreToolUse".into(), message });
                }
                if let Some(why) = pre.stop.take().or(pre.block.take()) {
                    return (ToolOutput::err(format!("Not run: a PreToolUse hook blocked it. {why}")), Usage::default());
                }
                if let Some(v) = pre.updated_input.take().and_then(|u| hooks::native_input(SUBAGENT, &input, &u)) {
                    input = v;
                }
            }
            let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if prompt.is_empty() {
                return (ToolOutput::err("Missing `prompt`: say what the helper should find out."), Usage::default());
            }
            let summary = self.tools.get(SUBAGENT).map(|t| t.summary(&input)).unwrap_or_default();
            let _ = tx.send(AgentEvent::ToolCall { id: id.to_string(), name: SUBAGENT.to_string(), summary, input: input.clone() });
            let mut session = match Session::create_named(&dir, id, cwd).await {
                Ok(s) => s,
                Err(e) => return (ToolOutput::err(format!("The helper couldn't start: {e}")), Usage::default()),
            };
            let mut helper = Agent {
                model: self.model.clone(),
                tools: self.tools.without(HELPER_HIDDEN),
                // plan mode's prompt is about the user's plan; a helper's limits are its own
                policy: if self.policy == Policy::Plan { Policy::Ask } else { self.policy },
                approver: Arc::new(DenyAll),
                system: format!("{}\n\n{HELPER}", self.system),
                context_window: self.context_window,
                hooks: self.hooks.clone(),
                sandbox: self.sandbox,
                inbox: Inbox::default(),
                max_steps: HELPER_MAX_STEPS,
                allowed: self.allowed.clone(),
                helper: Some(id.to_string()),
                started: true,
                halt: None,
            };
            let mut forward = |e: AgentEvent| {
                let e = match e {
                    AgentEvent::ToolCall { name, input, .. } => AgentEvent::HelperTool { id: id.to_string(), name, input },
                    AgentEvent::Step { usage, .. } | AgentEvent::Compacted { usage, .. } => AgentEvent::HelperStep { id: id.to_string(), usage },
                    // (what the harness told the helper isn't for the chat: it reads as said to the agent)
                    AgentEvent::Hook { event, .. } if event == NOTE => return,
                    e @ AgentEvent::Hook { .. } => e,
                    _ => return,
                };
                let _ = tx.send(e);
            };
            let res = helper.turn(&mut session, &prompt, cancel, &mut forward).await;
            // (all of it, when it was cut off at the output limit and went on)
            let answer = if session.messages().last().is_some_and(|m| m.role == Role::Assistant) { answer_of(session.events()) } else { String::new() };
            // when it didn't get to answer: the last thing it said, so its work isn't lost
            let so_far = || Some(answer_of(session.events())).filter(|t| !t.is_empty()).map(|t| format!(" What it had so far:\n\n{t}")).unwrap_or_default();
            let out = match res {
                Ok(TurnEnd::Answered) if !answer.is_empty() => ToolOutput::ok(answer),
                Ok(TurnEnd::Answered) => ToolOutput::err("The helper finished without an answer."),
                // out of steps, and asked for what it found: that's its answer, marked as partial
                Ok(TurnEnd::StepLimit) if !answer.is_empty() => ToolOutput::ok(format!(
                    "{answer}\n\n[The helper ran out of steps ({HELPER_MAX_STEPS}) before it was done, so this is what it found up to there. For what's missing, ask a narrower question or look yourself: the same question again would run out the same way.]"
                )),
                Ok(TurnEnd::StepLimit) => ToolOutput::err(format!("The helper stopped after {HELPER_MAX_STEPS} steps without an answer.{} Ask a narrower question or look yourself: the same question again would run out the same way.", so_far())),
                Err(Error::Cancelled) => ToolOutput::err("Stopped by the user."),
                Err(e) => ToolOutput::err(format!("The helper stopped: {e}.{}", so_far())),
            };
            let mut out = out;
            // PostToolUse hooks on the call, with the helper's answer
            let event = if out.is_error { "PostToolUseFailure" } else { "PostToolUse" };
            if self.hooks.has(event) {
                let mut fields = foreign(&input);
                fields["tool_response"] = json!({ "output": out.content });
                let ctx = self.hook_context(parent);
                let post = tokio::select! {
                    o = self.hooks.run(event, Some(&names), &ctx, fields) => o,
                    _ = cancel.cancelled() => Outcome::default(),
                };
                for message in post.notes {
                    let _ = tx.send(AgentEvent::Hook { event: event.into(), message });
                }
                if let Some(replaced) = post.updated_output {
                    out.content = replaced;
                }
                for c in pre.context.iter().chain(&post.context) {
                    out.content.push_str(&format!("\n\n{}", crate::session::hook_context(c)));
                }
            }
            (out, session.usage())
        })
    }

    /// The tool pipeline for one call: [`Agent::prepare`], then [`Agent::execute`].
    #[allow(clippy::too_many_arguments)]
    async fn call(&mut self, session: &mut Session, ctx: &ToolCtx, id: &str, name: &str, input: Value, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<ToolOutput> {
        Ok(match self.prepare(session, ctx, id, name, input, cancel, on).await? {
            Prepared::Done(out) => out,
            Prepared::Run(ready) => {
                let (out, halt) = self.execute(session, ctx, ready, cancel, on).await;
                if halt.is_some() {
                    self.halt = halt;
                }
                out
            }
        })
    }

    /// The first half of the pipeline, which runs one call at a time: PreToolUse hooks, the
    /// policy and the user's approval. Ends with the call ready to run (and the chat told it
    /// runs), or with its result when it won't.
    #[allow(clippy::too_many_arguments)]
    async fn prepare(&mut self, session: &mut Session, ctx: &ToolCtx, id: &str, name: &str, input: Value, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<Prepared> {
        // the MCP tools left out of the requests: found here, declared from the next step on
        if name == TOOL_SEARCH {
            let query = input.get("query").and_then(Value::as_str).unwrap_or("").trim().to_string();
            on(AgentEvent::ToolCall { id: id.to_string(), name: name.to_string(), summary: query.clone(), input: input.clone() });
            let found = self.tools.search(&query, 8);
            return Ok(Prepared::Done(if found.is_empty() {
                ToolOutput::ok(format!("No tools match \"{query}\". Try other words, or do without."))
            } else {
                let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
                let list: Vec<String> = found.iter().map(|s| format!("- {}: {}", s.name, clip(s.description.lines().next().unwrap_or(""), 200))).collect();
                ToolOutput::ok(format!("{FOUND}{}\n\nThey're in your tools from your next message on:\n{}", names.join(", "), list.join("\n")))
            }));
        }
        let Some(tool) = self.tools.get(name) else {
            return Ok(Prepared::Done(ToolOutput::err(format!("There is no tool named `{name}`."))));
        };
        if let Some(raw) = input.get(UNPARSED) {
            // its start is enough to recognise the call; all of it would fill the context again
            let raw = raw.as_str().map(String::from).unwrap_or_else(|| raw.to_string());
            return Ok(Prepared::Done(ToolOutput::err(format!("The arguments weren't valid JSON: {}", clip(&raw, 600)))));
        }
        let mut input = input;
        let mut kind = tool.kind(&input, ctx);
        let mut summary = tool.summary(&input);
        // Human in the loop: the question goes to the user, the answer back to the model.
        if name == ASK_USER {
            let options = input.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(|o| o.as_str().map(String::from)).collect()).unwrap_or_default();
            let q = Question { tool_use_id: id.to_string(), question: summary.clone(), options };
            on(AgentEvent::ToolCall { id: id.to_string(), name: name.to_string(), summary, input: input.clone() });
            let answer = tokio::select! {
                a = self.approver.ask(&q) => a,
                _ = cancel.cancelled() => return Ok(Prepared::Done(ToolOutput::err("Not answered: stopped by the user."))),
            };
            return Ok(Prepared::Done(match answer {
                Some(a) if !a.trim().is_empty() => ToolOutput::ok(format!("The user answered: {}", a.trim())),
                _ => ToolOutput::err("Nobody can answer questions right now. Decide yourself, and say what you assumed."),
            }));
        }
        // 0. PreToolUse hooks: they may block the call, rewrite it, allow it or make it ask
        let names = hooks::tool_names(name);
        let foreign = |input: &Value| {
            let (tool_name, tool_input) = hooks::foreign_tool(name, input, &ctx.cwd);
            json!({ "tool_name": tool_name, "tool_input": tool_input, "tool_use_id": id })
        };
        let pre = self.hook(session, "PreToolUse", Some(&names), foreign(&input), cancel, on).await;
        if cancel.is_cancelled() {
            return Ok(Prepared::Done(ToolOutput::err("Not run: stopped by the user.")));
        }
        if let Some(why) = pre.stop {
            self.halt = Some(format!("A PreToolUse hook stopped the agent: {why}"));
            return Ok(Prepared::Done(ToolOutput::err(format!("Not run: a PreToolUse hook stopped the agent. {why}"))));
        }
        if let Some(why) = pre.block {
            return Ok(Prepared::Done(ToolOutput::err(format!("Not run: a PreToolUse hook blocked it. {why}"))));
        }
        if let Some(updated) = &pre.updated_input {
            match hooks::native_input(name, &input, updated) {
                Some(v) => {
                    input = v;
                    kind = tool.kind(&input, ctx);
                    summary = tool.summary(&input);
                }
                None => on(AgentEvent::Hook { event: "PreToolUse".into(), message: format!("A hook's updatedInput for `{name}` was ignored: it can't be rewritten that way.") }),
            }
        }
        // what the user types to allow a call that can't be undone (after a hook's rewrite: the
        // text names what this call does)
        let confirm = tool.confirm(&input);
        // 1. helpers and plan mode: nothing that changes something runs, not even with a yes
        if self.helper.is_some() && (confirm.is_some() || matches!(kind, Kind::Edit | Kind::EditOutside | Kind::Exec)) {
            return Ok(Prepared::Done(ToolOutput::err("Not run: a helper only looks; it doesn't change files or run commands. If this needs doing, say so in your answer.")));
        }
        if self.policy.refuses(kind) {
            return Ok(Prepared::Done(ToolOutput::err("Not run: plan mode is on, so nothing is changed or run yet. Look around as much as you need, then answer with your plan; the user starts it.")));
        }
        // 2. approval (fail closed); a hook can allow it or make it ask, PermissionRequest hooks
        // can answer for the user, and a helper can't ask. A call with `confirm` always asks the
        // user: no policy, hook or earlier "always allow" answers for them.
        let mut ask = (self.policy.needs_approval(kind) && !self.allowed.contains(name) && !pre.allow) || pre.ask || confirm.is_some();
        if ask {
            let o = self.hook(session, "PermissionRequest", Some(&names), foreign(&input), cancel, on).await;
            let allow = o.allow && confirm.is_none();
            if o.block.is_some() || allow {
                session.append(SessionEvent::Approval { tool_use_id: id.to_string(), tool: name.to_string(), allowed: o.block.is_none(), by: "hook".into() }).await?;
            }
            if let Some(why) = o.block {
                return Ok(Prepared::Done(ToolOutput::err(format!("Not run: a PermissionRequest hook declined it. {why}"))));
            }
            ask = !allow;
        }
        if ask {
            if self.helper.is_some() {
                return Ok(Prepared::Done(ToolOutput::err("Not run: this needs the user's OK, and a helper can't ask. If you need it, say so in your answer.")));
            }
            let req = ApprovalRequest { tool_use_id: id.to_string(), tool: name.to_string(), kind, summary: summary.clone(), input: input.clone(), confirm: confirm.clone() };
            let (decision, by) = tokio::select! {
                d = self.approver.decide(&req) => d,
                _ = cancel.cancelled() => return Ok(Prepared::Done(ToolOutput::err("Not run: stopped by the user."))),
            };
            let allowed = decision.allows(confirm.as_deref());
            session.append(SessionEvent::Approval { tool_use_id: id.to_string(), tool: name.to_string(), allowed, by }).await?;
            match decision {
                Decision::Deny { reason } => {
                    let why = reason.filter(|r| !r.trim().is_empty()).map(|r| format!(" Reason: {r}")).unwrap_or_default();
                    return Ok(Prepared::Done(ToolOutput::err(format!("The user declined this call.{why}"))));
                }
                _ if !allowed => {
                    let want = confirm.unwrap_or_default();
                    return Ok(Prepared::Done(ToolOutput::err(format!("Not run: this can't be undone, so it runs only when the user types \"{want}\" to confirm it in DevCat, and they didn't."))));
                }
                Decision::AllowForSession if confirm.is_none() => {
                    self.allowed.insert(name.to_string());
                }
                _ => {}
            }
        }
        on(AgentEvent::ToolCall { id: id.to_string(), name: name.to_string(), summary, input: input.clone() });
        Ok(Prepared::Run(Ready { id: id.to_string(), name: name.to_string(), tool, input, context: pre.context }))
    }

    /// The second half: run the call (with its time limit), PostToolUse hooks, truncation. It
    /// only reads the agent, so calls that only look can run side by side (see
    /// [`Tool::concurrent`](crate::tools::Tool::concurrent)). Returns the result, and why the turn
    /// ends after this step when a hook stopped it.
    async fn execute(&self, session: &Session, ctx: &ToolCtx, ready: Ready, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> (ToolOutput, Option<String>) {
        let Ready { id, name, tool, input, context } = ready;
        let (id, name) = (id.as_str(), name.as_str());
        let names = hooks::tool_names(name);
        let foreign = |input: &Value| {
            let (tool_name, tool_input) = hooks::foreign_tool(name, input, &ctx.cwd);
            json!({ "tool_name": tool_name, "tool_input": tool_input, "tool_use_id": id })
        };
        // 3. timeout
        let limit = tool.timeout(&input);
        let after = foreign(&input);
        // a tool that panics fails its call, not the turn (and with it the chat)
        let run = std::panic::AssertUnwindSafe(tool.run(input, ctx)).catch_unwind();
        let mut out = tokio::select! {
            r = tokio::time::timeout(limit, run) => match r {
                Ok(Ok(out)) => out,
                Ok(Err(panic)) => {
                    let why = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_default();
                    ToolOutput::err(format!("The `{name}` tool crashed{}. That's a bug in the tool, not in your call: try another way, and tell the user.", if why.is_empty() { String::new() } else { format!(" ({why})") }))
                }
                Err(_) => ToolOutput::err(format!("Timed out after {} s.", limit.as_secs())),
            },
            _ = cancel.cancelled() => ToolOutput::err("Stopped by the user."),
        };
        // 4. PostToolUse hooks (PostToolUseFailure when it failed) see the whole output, and may
        // replace it or add feedback for the model
        let event = if out.is_error { "PostToolUseFailure" } else { "PostToolUse" };
        let mut fields = after;
        fields["tool_response"] = if out.is_error { json!({ "error": out.content }) } else { json!({ "output": out.content }) };
        if out.is_error {
            fields["error"] = json!(out.content);
        }
        let post = self.hook(session, event, Some(&names), fields, cancel, on).await;
        // it ran: its result stands, and the turn ends after this step
        let halt = post.stop.map(|why| format!("A {event} hook stopped the agent: {why}"));
        if let Some(replaced) = post.updated_output {
            out.content = replaced;
        }
        // 5. truncation; the hooks' context goes after it, so it's never cut
        let mut out = fit(session, id, name, out).await;
        for c in context.iter().chain(&post.context) {
            out.content.push_str(&format!("\n\n{}", crate::session::hook_context(c)));
        }
        (out, halt)
    }
}

/// A call through the first half of the pipeline ([`Agent::prepare`]).
enum Prepared {
    /// It won't run (a hook or the user said no, plan mode, a helper): its result.
    Done(ToolOutput),
    /// Allowed: what [`Agent::execute`] needs to run it.
    Run(Ready),
}

struct Ready {
    id: String,
    name: String,
    tool: Arc<dyn crate::tools::Tool>,
    input: Value,
    /// What PreToolUse hooks added for the model; it goes after the result.
    context: Vec<String>,
}

/// Output over [`MAX_OUTPUT`]: the model gets its start and end, and the whole of it is saved next
/// to the session for `read` (offset / limit) or `grep`. `read`'s own output isn't saved: its file
/// is on disk already, and the model pages through it with offset / limit.
async fn fit(session: &Session, id: &str, name: &str, out: ToolOutput) -> ToolOutput {
    if out.content.len() <= MAX_OUTPUT {
        return out;
    }
    let mut content = truncate(&out.content);
    if name != "read"
        && let Ok(file) = session.save_output(id, &out.content).await
    {
        content.push_str(&format!(
            "\n\n[The whole output ({} characters) is saved in {}. Read parts of it with `read` (offset / limit) or search it with `grep`.]",
            out.content.len(),
            file.display()
        ));
    }
    ToolOutput { content, ..out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn result(id: &str, content: String) -> Message {
        Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: id.into(), content, is_error: false }] }
    }

    fn call(id: &str, input: Value) -> Message {
        Message { role: Role::Assistant, content: vec![Block::Thinking { text: "t".repeat(500), signature: None }, Block::ToolUse { id: id.into(), name: "write".into(), input }] }
    }

    /// Every tool call is followed by its result, the roles alternate, and the user speaks first:
    /// what providers insist on.
    fn well_formed(messages: &[Message]) -> bool {
        let alternating = messages.windows(2).all(|w| w[0].role != w[1].role);
        let paired = messages.iter().enumerate().all(|(i, m)| {
            m.tool_uses().all(|(id, _, _)| messages.get(i + 1).is_some_and(|n| n.content.iter().any(|b| matches!(b, Block::ToolResult { tool_use_id, .. } if tool_use_id == id))))
                && m.content.iter().all(|b| match b {
                    Block::ToolResult { tool_use_id, .. } => i > 0 && messages[i - 1].tool_uses().any(|(id, _, _)| id == tool_use_id),
                    _ => true,
                })
        });
        alternating && paired && messages.first().is_some_and(|m| m.role == Role::User)
    }

    #[test]
    fn a_conversation_is_cut_down_for_its_summary_without_breaking_it() {
        let mut messages = vec![Message::user_text("the task")];
        for i in 0..10 {
            messages.push(call(&format!("c{i}"), json!({ "path": "a.txt", "content": "c".repeat(10_000), "n": i })));
            let mut r = result(&format!("c{i}"), format!("START{}END", "r".repeat(20_000)));
            if i == 4 {
                r.content.push(Block::Image { media_type: "image/png".into(), data: "A".repeat(50_000) });
            }
            messages.push(r);
        }
        assert_eq!(shrink(&messages, 0), messages);
        let sizes: Vec<usize> = (0..=SHRINK_LEVELS).map(|l| chars_of(&shrink(&messages, l))).collect();
        assert!(sizes.windows(2).all(|w| w[1] < w[0]), "each level is smaller: {sizes:?}");
        assert!(sizes[4] * 100 < sizes[0], "{sizes:?}");
        for level in 1..=SHRINK_LEVELS {
            let s = shrink(&messages, level);
            assert!(well_formed(&s), "level {level}");
            // the newest exchange is always there, with both ends of its output
            let last = s.last().unwrap();
            assert!(matches!(&last.content[0], Block::ToolResult { tool_use_id, content, .. } if tool_use_id == "c9" && content.starts_with("START") && content.ends_with("END")), "level {level}");
            assert!(!s.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::Thinking { .. } | Block::Image { .. })));
        }
        // the first levels keep the whole conversation, the task included
        assert_eq!(shrink(&messages, 2).len(), messages.len());
        assert_eq!(shrink(&messages, 2)[0].text(), "the task");
        // the last ones say that the start is missing
        assert!(shrink(&messages, 3).len() < messages.len() && shrink(&messages, 4).len() < shrink(&messages, 3).len());
        assert!(shrink(&messages, 4)[0].text().contains("left out"));
        // a conversation of one message stays as it is
        let one = [Message::user_text("hi")];
        assert_eq!(shrink(&one, 4), one);
    }

    /// A model with a given `max_tokens`, never asked.
    struct Answers(u64);

    #[async_trait]
    impl Model for Answers {
        fn name(&self) -> String {
            "answers".into()
        }
        fn max_output(&self) -> u64 {
            self.0
        }
        async fn step(&self, _: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply> {
            unreachable!()
        }
    }

    #[test]
    fn the_answer_gets_its_room_in_the_window_but_a_small_window_isnt_given_away() {
        let limit = |window: Option<u64>, max_tokens: u64| {
            let mut a = Agent::new(Arc::new(Answers(max_tokens)), Registry::default(), Policy::Ask, Arc::new(DenyAll), String::new());
            a.context_window = window;
            a.limit()
        };
        assert_eq!(limit(None, 16_000), None);
        // a large window: 80% of it, as ever
        assert_eq!(limit(Some(200_000), 16_000), Some(160_000));
        assert_eq!(limit(Some(1_000), 0), Some(800));
        // a middling one: the answer's 16,000 tokens and 5% stay free
        assert_eq!(limit(Some(64_000), 16_000), Some(44_800));
        // a small one: not under 60%, whatever the answer may take
        assert_eq!(limit(Some(32_000), 16_000), Some(19_200));
        assert_eq!(limit(Some(8_000), 16_000), Some(4_800));
    }

    #[test]
    fn a_pasted_wall_of_text_is_clipped_for_the_summary_too() {
        let messages = [Message::user_text("the task"), Message { role: Role::Assistant, content: vec![Block::Text { text: "OK.".into() }] }, Message::user_text(format!("START{}END", "log line\n".repeat(20_000)))];
        let sizes: Vec<usize> = (0..=2).map(|l| chars_of(&shrink(&messages, l))).collect();
        assert!(sizes[0] > 180_000 && sizes[1] < 8_200 && sizes[2] < 2_200, "{sizes:?}");
        let last = shrink(&messages, 2).pop().unwrap().text();
        assert!(last.starts_with("START") && last.ends_with("END"), "{last}");
    }

    #[test]
    fn an_answer_that_was_cut_off_and_went_on_is_read_whole() {
        let said = |t: &str| SessionEvent::Assistant { content: vec![Block::Thinking { text: "hm".into(), signature: None }, Block::Text { text: t.into() }], model: "m".into(), usage: Usage::default() };
        let told = |t: &str| SessionEvent::User { text: t.into(), images: vec![], context: vec![], hook: Some(NOTE.into()) };
        let call = SessionEvent::Assistant { content: vec![Block::ToolUse { id: "t".into(), name: "read".into(), input: json!({}) }], model: "m".into(), usage: Usage::default() };
        let mut events = vec![SessionEvent::user("explain", vec![]), said("Looking."), call.clone(), said("Part one, "), told(CUT_OFF), said("part two, "), told(CUT_OFF), said("part three.")];
        assert_eq!(answer_of(&events), "Part one, part two, part three.");
        // only what it went on from: not what it said before a tool call, or before another nudge
        events.extend([told(EMPTY_REPLY), said("Done.")]);
        assert_eq!(answer_of(&events), "Done.");
        // the last step that said something, whatever came after it
        events.extend([call, SessionEvent::Interrupted { reason: "Stopped by the user".into() }]);
        assert_eq!(answer_of(&events), "Done.");
        assert_eq!(answer_of(&events[..1]), "");
    }

    #[test]
    fn long_text_is_clipped_in_the_middle() {
        assert_eq!(clip("short", 10), "short");
        let c = clip(&format!("START{}END", "é".repeat(1_000)), 90);
        assert!(c.starts_with("START") && c.ends_with("END") && c.contains("[… 918 characters left out …]"), "{c}");
        assert_eq!(clip_input(&json!({ "path": "a", "lines": ["x".repeat(50)], "n": 3 }), 9), json!({ "path": "a", "lines": ["xxxxxx\n[… 41 characters left out …]\nxxx"], "n": 3 }));
    }

    /// A stop with no turn waiting on it yet is kept: the turn (or the poller) that looks later
    /// still sees it, and a wait started after it ends at once.
    #[tokio::test]
    async fn cancel_is_remembered() {
        let c = Cancel::default();
        c.cancel();
        assert!(c.is_cancelled(), "a stop with no one waiting is lost");
        assert!(c.clone().is_cancelled(), "clones share it");
        tokio::time::timeout(Duration::from_secs(1), c.cancelled()).await.expect("a wait after the stop ends at once");
    }
}
