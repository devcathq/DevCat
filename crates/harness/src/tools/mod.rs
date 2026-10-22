//! The tools the agent can call, and the registry that holds them. The agent runs every call
//! through the same pipeline (see [`crate::agent`]): approval, timeout, output truncation, event.
mod fs;
mod shell;
mod todo;
mod web;

use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use fs::{Edit, Glob, Grep, Read, Write};
pub use shell::Shell;
pub use todo::{TODO_WRITE, TodoWrite};
pub use web::WebFetch;

/// Name of the tool the agent handles itself: it asks the user through the [`crate::Approver`].
pub const ASK_USER: &str = "ask_user";

/// `ask_user`: the model asks the user a question, with suggested answers (human in the loop).
pub struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ASK_USER.into(),
            description: "Ask the user a question and wait for the answer. Use it when a decision is theirs to make (which option, naming, scope, a risky step), not for things you can find out yourself. Offer 2-4 short options when you can; the user may also answer in their own words.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" }, "maxItems": 6 }
                },
                "required": ["question"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input.get("question").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    fn timeout(&self, _: &Value) -> Duration {
        // the user may be away: the agent waits as long as the turn runs
        Duration::from_secs(60 * 60 * 24)
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::err("ask_user is answered by the agent, not run.")
    }
}

/// Output kept from one tool call; longer output keeps its start and end.
pub const MAX_OUTPUT: usize = 30_000;

pub struct ToolCtx {
    /// The project folder: relative paths start here, and edits outside it need approval.
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(s: impl Into<String>) -> Self {
        ToolOutput { content: s.into(), is_error: false }
    }
    pub fn err(s: impl Into<String>) -> Self {
        ToolOutput { content: s.into(), is_error: true }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// What this call does (decides whether it needs approval).
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind;
    /// One line describing this call, for approvals and tool cards.
    fn summary(&self, input: &Value) -> String;
    /// Longest the call may run.
    fn timeout(&self, _input: &Value) -> Duration {
        Duration::from_secs(60)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput;
}

#[derive(Clone, Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    /// read, write, edit, glob, grep, shell, web_fetch, todo_write and ask_user.
    pub fn standard() -> Registry {
        let mut r = Registry::default();
        r.add(Read);
        r.add(Write);
        r.add(Edit);
        r.add(Glob);
        r.add(Grep);
        r.add(Shell);
        r.add(WebFetch);
        r.add(TodoWrite);
        r.add(AskUser);
        r
    }

    pub fn add(&mut self, t: impl Tool + 'static) {
        self.tools.push(Arc::new(t));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.spec().name == name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }
}

/// `path` from the model, relative to the project unless absolute.
pub fn resolve(ctx: &ToolCtx, path: &str) -> PathBuf {
    let p = Path::new(path.trim());
    let joined = if p.is_absolute() { p.to_path_buf() } else { ctx.cwd.join(p) };
    normalize(&joined)
}

/// Whether `path` (already resolved) is inside the project folder.
pub fn inside(ctx: &ToolCtx, path: &Path) -> bool {
    let root = normalize(&ctx.cwd);
    // compare case-insensitively on Windows (C:\Proj and c:\proj are the same folder)
    if cfg!(windows) {
        let (r, p) = (root.to_string_lossy().to_lowercase(), path.to_string_lossy().to_lowercase());
        Path::new(&p).starts_with(Path::new(&r))
    } else {
        path.starts_with(&root)
    }
}

/// Remove `.` and `..` without touching the disk (the file may not exist yet).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Keep the start and the end of long output, with a note on what was cut.
pub fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT {
        return s.to_string();
    }
    let head_end = floor_char(s, MAX_OUTPUT * 2 / 3);
    let tail_start = floor_char(s, s.len() - MAX_OUTPUT / 3);
    let cut_lines = s[head_end..tail_start].lines().count();
    format!("{}\n\n[… {} characters, about {cut_lines} lines, cut …]\n\n{}", &s[..head_end], tail_start - head_end, &s[tail_start..])
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A string field of the input, or an error the model can act on.
pub(crate) fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    input.get(key).and_then(Value::as_str).ok_or_else(|| ToolOutput::err(format!("Missing required string parameter `{key}`.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_and_outside() {
        let root = if cfg!(windows) { "C:\\proj" } else { "/proj" };
        let ctx = ToolCtx { cwd: PathBuf::from(root) };
        assert!(inside(&ctx, &resolve(&ctx, "src/main.rs")));
        assert!(inside(&ctx, &resolve(&ctx, "./a/../b.txt")));
        assert!(!inside(&ctx, &resolve(&ctx, "../other/x")));
        assert!(!inside(&ctx, &resolve(&ctx, if cfg!(windows) { "C:\\projX\\a" } else { "/projX/a" })));
    }

    #[test]
    fn truncate_keeps_both_ends() {
        let s = format!("START{}END", "x\n".repeat(40_000));
        let t = truncate(&s);
        assert!(t.len() < MAX_OUTPUT + 200);
        assert!(t.starts_with("START") && t.ends_with("END") && t.contains("characters, about"));
    }
}
