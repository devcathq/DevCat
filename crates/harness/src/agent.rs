//! The agent loop. One user message is a *turn*: the model takes steps, each step may ask for
//! tool calls, the calls go through the pipeline (approval → timeout → truncation → event) and
//! their results go back to the model, until it answers without a tool call.
use crate::approval::{ApprovalRequest, Approver, Decision, Policy, Question};
use crate::llm::{Delta, Llm, Reply, Request};
use crate::message::{Block, ToolSpec, Usage};
use crate::session::{Session, SessionEvent};
use crate::tools::{ASK_USER, Registry, ToolCtx, ToolOutput, truncate};
use crate::{Error, Result};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::watch;

/// Steps per turn before the agent stops and asks the user to continue.
pub const MAX_STEPS: u32 = 60;

/// Share of the context window at which the conversation is summarized before the next step.
pub const COMPACT_AT: f64 = 0.8;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnEnd {
    /// The model answered.
    Answered,
    /// [`MAX_STEPS`] steps without an answer; the user can say "continue".
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
    /// Tools the user allowed for the rest of the session.
    allowed: HashSet<String>,
}

impl Agent {
    pub fn new(model: Arc<dyn Model>, tools: Registry, policy: Policy, approver: Arc<dyn Approver>, system: String) -> Agent {
        Agent { model, tools, policy, approver, system, context_window: None, allowed: HashSet::new() }
    }

    /// Summarize the conversation so far and continue from the summary (`/compact`). `focus` says
    /// what the summary should keep in particular.
    pub async fn compact(&mut self, session: &mut Session, focus: Option<&str>, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<()> {
        let mut messages = session.messages();
        if messages.is_empty() {
            return Err(Error::Provider("There's nothing to compact yet.".into()));
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
        let req = Request { system: &self.system, messages: &messages, tools: &specs, cache_key: Some(&session.id) };
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
        session.append(SessionEvent::User { text: user_text.to_string(), images }).await?;
        let res = self.run(session, cancel, on).await;
        match &res {
            Err(Error::Cancelled) => session.append(SessionEvent::Interrupted { reason: "Stopped by the user".into() }).await?,
            Err(e) => session.append(SessionEvent::Interrupted { reason: e.to_string() }).await?,
            Ok(TurnEnd::StepLimit) => session.append(SessionEvent::Interrupted { reason: format!("Stopped after {MAX_STEPS} steps") }).await?,
            Ok(TurnEnd::Answered) => {}
        }
        res
    }

    async fn run(&mut self, session: &mut Session, cancel: &Cancel, on: &mut (dyn FnMut(AgentEvent) + Send)) -> Result<TurnEnd> {
        let specs: Vec<ToolSpec> = self.tools.specs();
        let ctx = ToolCtx { cwd: session.cwd.clone().into() };
        for _ in 0..MAX_STEPS {
            if self.over_limit(session) {
                self.compact(session, None, cancel, on).await?;
            }
            let messages = session.messages();
            let req = Request { system: &self.system, messages: &messages, tools: &specs, cache_key: Some(&session.id) };
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
                return Ok(TurnEnd::Answered);
            }
            let mut results = Vec::new();
            for (id, name, input) in calls {
                let out = if cancel.is_cancelled() {
                    ToolOutput::err("Not run: stopped by the user.")
                } else {
                    self.call(session, &ctx, &id, &name, input, cancel, on).await?
                };
                on(AgentEvent::ToolResult { id: id.clone(), content: out.content.clone(), is_error: out.is_error });
                results.push(Block::ToolResult { tool_use_id: id, content: out.content, is_error: out.is_error });
            }
            session.append(SessionEvent::ToolResults { results }).await?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }
        Ok(TurnEnd::StepLimit)
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
        let kind = tool.kind(&input, ctx);
        let summary = tool.summary(&input);
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
        // 1. approval (fail closed)
        if self.policy.needs_approval(kind) && !self.allowed.contains(name) {
            let req = ApprovalRequest { tool_use_id: id.to_string(), tool: name.to_string(), kind, summary: summary.clone(), input: input.clone() };
            let (decision, by) = tokio::select! {
                d = self.approver.decide(&req) => d,
                _ = cancel.cancelled() => return Ok(ToolOutput::err("Not run: stopped by the user.")),
            };
            let allowed = !matches!(decision, Decision::Deny { .. });
            session.append(SessionEvent::Approval { tool_use_id: id.to_string(), tool: name.to_string(), allowed, by }).await?;
            match decision {
                Decision::Allow => {}
                Decision::AllowForSession => {
                    self.allowed.insert(name.to_string());
                }
                Decision::Deny { reason } => {
                    let why = reason.filter(|r| !r.trim().is_empty()).map(|r| format!(" Reason: {r}")).unwrap_or_default();
                    return Ok(ToolOutput::err(format!("The user declined this call.{why}")));
                }
            }
        }
        on(AgentEvent::ToolCall { id: id.to_string(), name: name.to_string(), summary, input: input.clone() });
        // 2. timeout, 3. truncation
        let limit = tool.timeout(&input);
        let out = tokio::select! {
            r = tokio::time::timeout(limit, tool.run(input, ctx)) => r.unwrap_or_else(|_| ToolOutput::err(format!("Timed out after {} s.", limit.as_secs()))),
            _ = cancel.cancelled() => ToolOutput::err("Stopped by the user."),
        };
        Ok(ToolOutput { content: truncate(&out.content), is_error: out.is_error })
    }
}
