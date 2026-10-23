//! DevCat's own coding-agent harness (issue #11): runs an agent against any model provider
//! without an external CLI.
//!
//! The design follows DeepSeek Harness (<https://github.com/deepseek-ai/deepseek-harness>, MIT):
//! - The **session is an append-only event log** (JSONL). The conversation sent to the model,
//!   resume and the UI are projections of it ([`session`]).
//! - A **provider-neutral message vocabulary** with one thin adapter per wire protocol
//!   ([`message`], [`llm`]).
//! - **One guarded tool pipeline**: approval, then a timeout, then output truncation, then an
//!   event ([`tools`], [`approval`]). Approvals fail closed.
//!
//! [`Agent`] ties them together: one user message runs model steps and tool calls until the model
//! answers without asking for a tool.

pub mod agent;
pub mod approval;
pub mod hooks;
pub mod llm;
pub mod mcp;
pub mod message;
pub mod prompt;
pub mod session;
pub mod tools;

pub use agent::{Agent, AgentEvent, Cancel, Model, TurnEnd};
pub use approval::{Approver, Decision, Policy};
pub use llm::{Llm, LlmConfig, Protocol};
pub use message::{Block, Message, Role, Usage};
pub use session::{Session, SessionEvent};

/// Errors from the harness. Tool failures are not errors: they go back to the model as results.
#[derive(Debug)]
pub enum Error {
    /// The model provider answered with an error or an unexpected response.
    Provider(String),
    /// Network or I/O failure.
    Io(String),
    /// The session file is unreadable or from an unknown format version.
    Session(String),
    /// The turn was stopped by the user.
    Cancelled,
    /// A hook stopped it (a blocked prompt, `continue: false`), with its reason.
    Hook(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Provider(m) => write!(f, "model provider: {m}"),
            Error::Io(m) => write!(f, "{m}"),
            Error::Session(m) => write!(f, "session: {m}"),
            Error::Cancelled => write!(f, "stopped"),
            Error::Hook(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
