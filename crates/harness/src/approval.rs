//! Who may run what. Each tool call has a [`Kind`]; the session's [`Policy`] says which kinds
//! need a yes from the user, and an [`Approver`] (the chat UI, the phone) gives it. Anything that
//! goes wrong while asking counts as a no.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// Edits and commands need approval.
    #[default]
    Ask,
    /// Edits inside the project run without asking; commands and outside edits still ask.
    AutoEdit,
    /// Nothing asks.
    Bypass,
}

/// What a tool call does, for approval purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Reads files or searches. Never asks.
    Read,
    /// Changes a file inside the project.
    Edit,
    /// Changes a file outside the project.
    EditOutside,
    /// Runs a command.
    Exec,
    /// Reads a URL. Asks like a command: the URL itself can carry data out of the project.
    Fetch,
    /// Reads data from one of the user's connections (database rows, Redis keys, cluster objects,
    /// logs). Asks in `Ask` only: the data goes to the model's provider.
    Query,
}

impl Policy {
    pub fn needs_approval(self, kind: Kind) -> bool {
        !matches!((self, kind), (_, Kind::Read) | (Policy::Bypass, _) | (Policy::AutoEdit, Kind::Edit | Kind::Query))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub tool_use_id: String,
    pub tool: String,
    pub kind: Kind,
    /// One line for the prompt: the command, or the file being changed.
    pub summary: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum Decision {
    Allow,
    /// Allow this tool for the rest of the session without asking again.
    AllowForSession,
    Deny { reason: Option<String> },
}

/// A question the model asks the user (the `ask_user` tool).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub tool_use_id: String,
    pub question: String,
    /// Suggested answers to pick from; the user can always type their own.
    pub options: Vec<String>,
}

#[async_trait]
pub trait Approver: Send + Sync {
    /// Ask the user. `by` in the returned pair names who answered (for the audit log).
    async fn decide(&self, req: &ApprovalRequest) -> (Decision, String);
    /// Put a question to the user and wait for the answer (None = nobody can answer).
    async fn ask(&self, _q: &Question) -> Option<String> {
        None
    }
}

/// Refuses everything: the default when nobody can answer (fail closed).
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn decide(&self, _: &ApprovalRequest) -> (Decision, String) {
        (Decision::Deny { reason: Some("No one is available to approve this.".into()) }, "policy".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_table() {
        use Kind::*;
        let row = |p: Policy| [Read, Edit, EditOutside, Exec, Fetch, Query].map(|k| p.needs_approval(k));
        assert_eq!(row(Policy::Ask), [false, true, true, true, true, true]);
        assert_eq!(row(Policy::AutoEdit), [false, false, true, true, true, false]);
        assert_eq!(row(Policy::Bypass), [false, false, false, false, false, false]);
    }
}
