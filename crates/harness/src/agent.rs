//! The agent loop. One user message is a *turn*: the model takes steps, each step may ask for
//! tool calls, the calls go through the pipeline (approval → timeout → truncation → event) and
//! their results go back to the model, until it answers without a tool call.
use crate::approval::{ApprovalRequest, Approver, Decision, DenyAll, Kind, Policy, Question};
use crate::hooks::{self, Hooks, Outcome};
use crate::llm::{Delta, Llm, Reply, Request};
use crate::message::{Block, Role, ToolSpec, Usage};
use crate::session::{Session, SessionEvent};
use crate::tools::{ASK_USER, MAX_OUTPUT, Registry, SUBAGENT, TODO_WRITE, ToolCtx, ToolOutput, truncate};
use crate::{Error, Result};
use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Default steps per turn before the agent stops and asks the user to continue (see
/// [`Agent::max_steps`]); helpers always stop here.
pub const MAX_STEPS: u32 = 60;

/// Share of the context window at which the conversation is summarized before the next step.
pub const COMPACT_AT: f64 = 0.8;

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
short code excerpts only where they matter. If you couldn't find something, say so and say where you looked.";

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

/// One model step. [`Llm`] is the real one; tests use a scripted model.
#[async_trait]
pub trait Model: Send + Sync {
    fn name(&self) -> String;
    async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply>;
}

#[async_trait]
impl Model for Llm {
    fn name(&self) -> String {
        self.config.model.clone()
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
    /// A tool call is about to run (after approval, if it needed one).
    ToolCall { id: String, name: String, summary: String, input: Value },
    ToolResult { id: String, content: String, is_error: bool },
    /// The helper started by `subagent` call `id` made a tool call.
    HelperTool { id: String, name: String, input: Value },
    /// The helper started by `subagent` call `id` finished a step; its tokens count for the chat.
    HelperStep { id: String, usage: Usage },
    /// A hook has something to tell the user: its `systemMessage`, that it failed, or (Stop) why
    /// the agent goes on.
    Hook { event: String, message: String },
    /// A message the user sent while the agent worked is now part of the conversation, or
    /// (`blocked`) a UserPromptSubmit hook refused it: it's dropped, and a `Hook` event says why.
    UserMessage { text: String, blocked: bool },
}

/// Messages the user sends while a turn runs. The agent reads them before its next step (also
/// when it was about to answer), so the user doesn't have to stop it to say something.
#[derive(Clone, Default)]
pub struct Inbox(Arc<std::sync::Mutex<Vec<Queued>>>);

/// A queued message: its text and any images.
pub type Queued = (String, Vec<Block>);

impl Inbox {
    /// Queue a message (with any images) for the running turn.
    pub fn push(&self, text: String, images: Vec<Block>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push((text, images));
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
        let _ = self.0.send(true);
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
        let mut messages = session.messages();
        if messages.is_empty() {
            return Err(Error::Provider("There's nothing to compact yet.".into()));
        }
        // `/compact` passes a focus (maybe empty); the automatic one doesn't
        let trigger = if focus.is_some() { "manual" } else { "auto" };
        let pre = self.hook(session, "PreCompact", Some(&[trigger]), json!({ "trigger": trigger, "custom_instructions": focus.unwrap_or("") }), cancel, on).await;
        if let Some(why) = pre.stop.or(pre.block) {
            // the automatic one goes on without compacting
            return if trigger == "manual" { Err(Error::Hook(format!("A PreCompact hook stopped the compaction: {why}"))) } else { Ok(()) };
        }
        let tokens_before = session.context_tokens();
        on(AgentEvent::Compacting { tokens_before });
        let mut ask = COMPACT_PROMPT.to_string();
        if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
            ask.push_str(&format!("\n\nThe user wants the summary to focus on: {f}"));
        }
        match messages.last_mut() {
            Some(m) if m.role == crate::message::Role::User => m.content.push(Block::Text { text: ask }),
            _ => messages.push(crate::message::Message { role: crate::message::Role::User, content: vec![Block::Text { text: ask }] }),
        }
        // Same system prompt and tools as every step, so the provider's cache still applies.
        let specs = self.tools.specs();
        let system = self.system_now();
        let req = Request { system: &system, messages: &messages, tools: &specs, cache_key: Some(&session.id) };
        let mut ignore = |_| {};
        let reply = tokio::select! {
            r = self.model.step(&req, &mut ignore) => r?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        let summary: String = reply.content.iter().filter_map(|b| match b {
            Block::Text { text } => Some(text.as_str()),
            _ => None,
        }).collect::<Vec<_>>().join("").trim().to_string();
        if summary.is_empty() {
            return Err(Error::Provider("The model returned an empty summary; the conversation was left as it was.".into()));
        }
        session.append(SessionEvent::Compacted { summary: summary.clone(), model: self.model.name(), usage: reply.usage, tokens_before }).await?;
        on(AgentEvent::Compacted { summary, tokens_before, usage: reply.usage });
        self.hook(session, "PostCompact", Some(&[trigger]), json!({ "trigger": trigger }), cancel, on).await;
        Ok(())
    }

    /// Whether the conversation has outgrown [`COMPACT_AT`] of the context window.
    fn over_limit(&self, session: &Session) -> bool {
        self.context_window.is_some_and(|w| session.context_tokens() as f64 > w as f64 * COMPACT_AT)
    }

    /// Run one turn for `user_text`, writing everything to `session`.
    pub async fn turn(&mut self, session: &mut Session, user_text: &str, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        self.turn_with_images(session, user_text, vec![], cancel, on).await
    }

    /// A turn whose message carries images (`Block::Image`).
    pub async fn turn_with_images(&mut self, session: &mut Session, user_text: &str, images: Vec<Block>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        // hooks on the user's messages (not a helper's prompt): SessionStart on the first turn, then
        // UserPromptSubmit; both may add context, and the second may block the message (it's not
        // recorded then)
        let mut context = Vec::new();
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
        let specs: Vec<ToolSpec> = self.tools.specs();
        let ctx = ToolCtx { cwd: session.cwd.clone().into(), sandbox: self.sandbox };
        // the last call and how many times in a row it was made (repeat guard)
        let mut last: Option<(String, Value)> = None;
        let mut same = 0;
        let system = self.system_now();
        // a Stop hook already sent the agent back to work this turn
        let mut stop_hook_active = false;
        let mut steps = 0u32;
        loop {
            if self.max_steps > 0 && steps >= self.max_steps {
                return Ok(TurnEnd::StepLimit);
            }
            steps += 1;
            // what the user sent meanwhile goes in before the step (after the tool results), past
            // the UserPromptSubmit hooks like a message that starts a turn
            let mut queued = self.inbox.take().into_iter();
            while let Some((text, images)) = queued.next() {
                let o = self.hook(session, "UserPromptSubmit", None, json!({ "prompt": text }), cancel, on).await;
                if cancel.is_cancelled() {
                    // stopped before its hooks said yes: this one and the rest stay unread (the app
                    // gives them back to the chat box), in the order they were sent
                    let later = self.inbox.take();
                    for (t, i) in std::iter::once((text, images)).chain(queued).chain(later) {
                        self.inbox.push(t, i);
                    }
                    return Err(Error::Cancelled);
                }
                if let Some(why) = o.stop.or(o.block) {
                    on(AgentEvent::Hook { event: "UserPromptSubmit".into(), message: format!("A hook blocked a message you sent: {why}") });
                    on(AgentEvent::UserMessage { text, blocked: true });
                    continue;
                }
                session.append(SessionEvent::User { text: text.clone(), images, context: o.context, hook: None }).await?;
                on(AgentEvent::UserMessage { text, blocked: false });
            }
            if self.over_limit(session) {
                self.compact(session, None, cancel, on).await?;
            }
            let messages = session.messages();
            let req = Request { system: &system, messages: &messages, tools: &specs, cache_key: Some(&session.id) };
            let mut forward = |d: Delta| {
                on(match d {
                    Delta::Text(text) => AgentEvent::Text { text },
                    Delta::Thinking(text) => AgentEvent::Thinking { text },
                    Delta::ToolStart { id, name } => AgentEvent::ToolStart { id, name },
                })
            };
            let reply = tokio::select! {
                r = self.model.step(&req, &mut forward) => r?,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            session.append(SessionEvent::Assistant { content: reply.content.clone(), model: self.model.name(), usage: reply.usage }).await?;
            on(AgentEvent::Step { content: reply.content.clone(), usage: reply.usage });

            let calls: Vec<(String, String, Value)> = reply.content.iter().filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            }).collect();
            if calls.is_empty() {
                // a message came while it answered: it reads that before ending
                if !self.inbox.is_empty() {
                    continue;
                }
                // Stop hooks (SubagentStop in a helper) may send the agent back to work
                let event = if self.helper.is_some() { "SubagentStop" } else { "Stop" };
                let answer: String = reply.content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect();
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
            for (id, name, input) in calls {
                if let Some((out, used, took)) = helped.remove(&id) {
                    usage += used;
                    took_ms.push(took);
                    results.push(Block::ToolResult { tool_use_id: id, content: out.content, is_error: out.is_error });
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
                max_steps: MAX_STEPS,
                allowed: self.allowed.clone(),
                helper: Some(id.to_string()),
                started: true,
                halt: None,
            };
            let mut forward = |e: AgentEvent| {
                let e = match e {
                    AgentEvent::ToolCall { name, input, .. } => AgentEvent::HelperTool { id: id.to_string(), name, input },
                    AgentEvent::Step { usage, .. } | AgentEvent::Compacted { usage, .. } => AgentEvent::HelperStep { id: id.to_string(), usage },
                    e @ AgentEvent::Hook { .. } => e,
                    _ => return,
                };
                let _ = tx.send(e);
            };
            let res = helper.turn(&mut session, &prompt, cancel, &mut forward).await;
            let answer = session.messages().last().filter(|m| m.role == Role::Assistant).map(|m| m.text().trim().to_string()).unwrap_or_default();
            let out = match res {
                Ok(TurnEnd::Answered) if !answer.is_empty() => ToolOutput::ok(answer),
                Ok(TurnEnd::Answered) => ToolOutput::err("The helper finished without an answer."),
                Ok(TurnEnd::StepLimit) => ToolOutput::err(format!(
                    "The helper stopped after {MAX_STEPS} steps without an answer.{}",
                    if answer.is_empty() { String::new() } else { format!(" What it had so far:\n\n{answer}") }
                )),
                Err(Error::Cancelled) => ToolOutput::err("Stopped by the user."),
                Err(e) => ToolOutput::err(format!("The helper stopped: {e}")),
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

    /// The tool pipeline for one call.
    #[allow(clippy::too_many_arguments)]
    async fn call(&mut self, session: &mut Session, ctx: &ToolCtx, id: &str, name: &str, input: Value, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<ToolOutput> {
        let Some(tool) = self.tools.get(name) else {
            return Ok(ToolOutput::err(format!("There is no tool named `{name}`.")));
        };
        if let Some(raw) = input.get("_unparsed_arguments") {
            return Ok(ToolOutput::err(format!("The arguments weren't valid JSON: {raw}")));
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
                _ = cancel.cancelled() => return Ok(ToolOutput::err("Not answered: stopped by the user.")),
            };
            return Ok(match answer {
                Some(a) if !a.trim().is_empty() => ToolOutput::ok(format!("The user answered: {}", a.trim())),
                _ => ToolOutput::err("Nobody can answer questions right now. Decide yourself, and say what you assumed."),
            });
        }
        // 0. PreToolUse hooks: they may block the call, rewrite it, allow it or make it ask
        let names = hooks::tool_names(name);
        let foreign = |input: &Value| {
            let (tool_name, tool_input) = hooks::foreign_tool(name, input, &ctx.cwd);
            json!({ "tool_name": tool_name, "tool_input": tool_input, "tool_use_id": id })
        };
        let pre = self.hook(session, "PreToolUse", Some(&names), foreign(&input), cancel, on).await;
        if cancel.is_cancelled() {
            return Ok(ToolOutput::err("Not run: stopped by the user."));
        }
        if let Some(why) = pre.stop {
            self.halt = Some(format!("A PreToolUse hook stopped the agent: {why}"));
            return Ok(ToolOutput::err(format!("Not run: a PreToolUse hook stopped the agent. {why}")));
        }
        if let Some(why) = pre.block {
            return Ok(ToolOutput::err(format!("Not run: a PreToolUse hook blocked it. {why}")));
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
            return Ok(ToolOutput::err("Not run: a helper only looks; it doesn't change files or run commands. If this needs doing, say so in your answer."));
        }
        if self.policy.refuses(kind) {
            return Ok(ToolOutput::err("Not run: plan mode is on, so nothing is changed or run yet. Look around as much as you need, then answer with your plan; the user starts it."));
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
                return Ok(ToolOutput::err(format!("Not run: a PermissionRequest hook declined it. {why}")));
            }
            ask = !allow;
        }
        if ask {
            if self.helper.is_some() {
                return Ok(ToolOutput::err("Not run: this needs the user's OK, and a helper can't ask. If you need it, say so in your answer."));
            }
            let req = ApprovalRequest { tool_use_id: id.to_string(), tool: name.to_string(), kind, summary: summary.clone(), input: input.clone(), confirm: confirm.clone() };
            let (decision, by) = tokio::select! {
                d = self.approver.decide(&req) => d,
                _ = cancel.cancelled() => return Ok(ToolOutput::err("Not run: stopped by the user.")),
            };
            let allowed = decision.allows(confirm.as_deref());
            session.append(SessionEvent::Approval { tool_use_id: id.to_string(), tool: name.to_string(), allowed, by }).await?;
            match decision {
                Decision::Deny { reason } => {
                    let why = reason.filter(|r| !r.trim().is_empty()).map(|r| format!(" Reason: {r}")).unwrap_or_default();
                    return Ok(ToolOutput::err(format!("The user declined this call.{why}")));
                }
                _ if !allowed => {
                    let want = confirm.unwrap_or_default();
                    return Ok(ToolOutput::err(format!("Not run: this can't be undone, so it runs only when the user types \"{want}\" to confirm it in DevCat, and they didn't.")));
                }
                Decision::AllowForSession if confirm.is_none() => {
                    self.allowed.insert(name.to_string());
                }
                _ => {}
            }
        }
        on(AgentEvent::ToolCall { id: id.to_string(), name: name.to_string(), summary, input: input.clone() });
        // 3. timeout
        let limit = tool.timeout(&input);
        let after = foreign(&input);
        let mut out = tokio::select! {
            r = tokio::time::timeout(limit, tool.run(input, ctx)) => r.unwrap_or_else(|_| ToolOutput::err(format!("Timed out after {} s.", limit.as_secs()))),
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
        if let Some(why) = post.stop {
            self.halt = Some(format!("A {event} hook stopped the agent: {why}"));
        }
        if let Some(replaced) = post.updated_output {
            out.content = replaced;
        }
        // 5. truncation; the hooks' context goes after it, so it's never cut
        let mut out = fit(session, id, name, out).await;
        for c in pre.context.iter().chain(&post.context) {
            out.content.push_str(&format!("\n\n{}", crate::session::hook_context(c)));
        }
        Ok(out)
    }
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
