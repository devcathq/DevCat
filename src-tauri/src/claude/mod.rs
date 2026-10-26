//! Claude Code integration: drives the native `claude` CLI binary (no npm / Node SDK)
//! in bidirectional stream-json mode, plus the workspace helpers the Claude view needs
//! (session list from ~/.claude/projects, file tree + previews, git status/diff).
//!
//! CONTRACT — implementation in progress. Protocol (verified against claude 2.1.280):
//!   spawn: claude -p --input-format stream-json --output-format stream-json --verbose
//!          --include-partial-messages --permission-prompt-tool stdio
//!          [--model M] [--permission-mode M] [--resume ID [--fork-session]]
//!   stdin  → {"type":"control_request","request_id":ID,"request":{"subtype":"initialize"}}   (first)
//!   stdin  → {"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
//!   stdout ← system(init|hook_*), assistant, user(tool_result), stream_event, result,
//!            control_response (to our requests),
//!            control_request {"subtype":"can_use_tool","tool_name","display_name","input",
//!                             "description","permission_suggestions","tool_use_id"}
//!   stdin  → {"type":"control_response","response":{"subtype":"success","request_id":ID,
//!            "response":{"behavior":"allow","updatedInput":{…}} | {"behavior":"deny","message":"…"}}}
//!   other control requests we send: interrupt, set_permission_mode {mode}, set_model {model}
//!
//! Events to the frontend:
//!   `claude-msg`  → { runId, msg }   (every stdout JSON object, unmodified)
//!   `claude-exit` → { runId, code, error? }
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeInfo {
    /// Absolute path of the claude binary, None if not found.
    pub path: Option<String>,
    pub version: Option<String>,
    /// From the initialize handshake (account email/org, models list, …) when available.
    pub account: Option<Value>,
    pub models: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    pub model: Option<String>,
    /// manual | acceptEdits | auto | plan | dontAsk | bypassPermissions
    pub permission_mode: Option<String>,
    /// Resume this session id.
    pub resume: Option<String>,
    /// With `resume`: continue as a new session id.
    #[serde(default)]
    pub fork: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeProject {
    pub cwd: String,
    pub session_count: usize,
    /// RFC3339 of the newest session file.
    pub last_active: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    /// ai-title if present, else first user prompt (trimmed).
    pub title: String,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub message_count: usize,
    pub git_branch: Option<String>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    /// git porcelain status letter(s) for files under a repo, e.g. "M", "A", "??".
    pub git: Option<String>,
    /// Ignored by .gitignore (only listed when show_ignored = true).
    #[serde(default)]
    pub ignored: bool,
    /// Directory contains a `.git` entry (repo, submodule or worktree).
    #[serde(default)]
    pub is_repo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetData {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    /// text | markdown | image | pdf | sheet | csv | docx | binary | too_large
    pub kind: String,
    pub path: String,
    pub mime: Option<String>,
    pub size: u64,
    /// text / markdown / csv / docx (extracted text)
    pub text: Option<String>,
    /// Monaco language id for text.
    pub language: Option<String>,
    /// image / pdf bytes, base64.
    pub data_base64: Option<String>,
    pub sheets: Option<Vec<SheetData>>,
    pub truncated: bool,
    /// kind "text" only: can be edited and saved with `fs_write_text`.
    pub editable: bool,
    /// Why an otherwise editable text file is read-only (truncated, not UTF-8, no permission).
    pub readonly_reason: Option<String>,
    /// On-disk version token ("<mtime_ns>-<len>") taken before reading, for conflict detection.
    pub version: Option<String>,
    /// text kinds: dominant line ending, "lf" or "crlf".
    pub line_ending: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFile {
    pub path: String,
    pub old_path: Option<String>,
    /// Index (staged) status letter: M A D R C U or ' '
    pub index: String,
    /// Worktree status letter: M D ? or ' '
    pub worktree: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repo: bool,
    pub root: Option<String>,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<GitFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVersions {
    pub original: String,
    pub modified: String,
    pub binary: bool,
}

pub mod runner;
pub mod review;
pub mod headless;
pub mod sessions;
pub mod files;
pub mod open_external;
pub mod clipboard;
pub mod git;
pub(crate) mod locate;
pub mod pty;
pub mod remote;
pub mod sftp;
pub mod ssh_forward;

#[allow(dead_code)]
fn _keep(_: AppResult<()>) {}
