//! The tools the agent can call, and the registry that holds them. The agent runs every call
//! through the same pipeline (see [`crate::agent`]): approval, timeout, output truncation, event.
mod edit;
mod fs;
mod git;
mod shell;
mod todo;
mod web;

use crate::approval::Kind;
use crate::message::{Block, ToolSpec};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use fs::{Edit, Glob, Grep, Read, Write};
pub use git::Git;
pub use shell::Shell;
#[cfg(unix)]
pub(crate) use shell::libc_setsid;
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

/// Name of the tool that starts helpers; the agent runs it itself (see [`crate::agent`]).
pub const SUBAGENT: &str = "subagent";

/// `subagent`: hand a question to a helper with a fresh conversation. The helper only looks
/// around, and its answer is the result; several calls in one step run at the same time.
pub struct Subagent;

#[async_trait]
impl Tool for Subagent {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SUBAGENT.into(),
            description: "Hand a self-contained question to a helper agent. It starts with an empty conversation, works through the question with the tools that don't change anything (read, glob, grep, read-only git, and whatever else runs without asking the user), and its final answer comes back as the result of this call. Use it for research that would otherwise fill your context: finding where and how something is done across many files, reading a lot of code or documentation to answer one question, checking several places. Several `subagent` calls in one message run at the same time, so split independent questions. The helper doesn't see this conversation: put everything it needs in `prompt` (what to find out, where to start, what you already know) and say what the answer should contain. It can't edit files, run commands or ask the user. Don't use it for a single read or search: do that yourself.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "What the helper is doing in 3-6 words, shown to the user (e.g. \"Find where sessions are saved\")." },
                    "prompt": { "type": "string", "description": "The task, complete and standalone: the helper sees nothing else." }
                },
                "required": ["description", "prompt"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("");
        let d = s("description");
        if d.is_empty() { s("prompt").lines().next().unwrap_or("").to_string() } else { d.to_string() }
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::err("subagent is run by the agent, not as a tool.")
    }
}

/// Output kept from one tool call; longer output keeps its start and end.
pub const MAX_OUTPUT: usize = 30_000;

pub struct ToolCtx {
    /// The project folder: relative paths start here, and edits outside it need approval.
    pub cwd: PathBuf,
    /// Commands run in the OS sandbox: they can write only in the project (see [`crate::sandbox`]).
    pub sandbox: bool,
}

impl ToolCtx {
    /// In `cwd`, without the sandbox.
    pub fn at(cwd: impl Into<PathBuf>) -> ToolCtx {
        ToolCtx { cwd: cwd.into(), sandbox: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// Images for the model to see (`Block::Image`), sent after the turn's tool results.
    pub images: Vec<Block>,
}

impl ToolOutput {
    pub fn ok(s: impl Into<String>) -> Self {
        ToolOutput { content: s.into(), is_error: false, images: vec![] }
    }
    pub fn err(s: impl Into<String>) -> Self {
        ToolOutput { content: s.into(), is_error: true, images: vec![] }
    }
    /// `content`, with an image (base64 `data`) the model sees.
    pub fn image(content: impl Into<String>, media_type: impl Into<String>, data: String) -> Self {
        ToolOutput { content: content.into(), is_error: false, images: vec![Block::Image { media_type: media_type.into(), data }] }
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
    /// For calls that can't be undone (dropping a database): what the user types to allow it (its
    /// name). Such a call always asks, whatever the policy, hooks or earlier answers, and only a
    /// [`Decision::Confirm`](crate::approval::Decision::Confirm) with that text runs it.
    fn confirm(&self, _input: &Value) -> Option<String> {
        None
    }
    /// Whether this call may run at the same time as the calls next to it in a step that may too:
    /// it only reads, asks nobody and depends on nothing they do (read, glob, grep, read-only
    /// git). Off by default; a tool turns it on only when that holds for every input it says yes
    /// to.
    fn concurrent(&self, _input: &Value, _ctx: &ToolCtx) -> bool {
        false
    }
    /// Whether this tool may be left out of the requests until the model looks for it with
    /// `tool_search` (see [`Registry::declared`]): an MCP server's, of which there can be
    /// hundreds. The built-in tools are always there.
    fn deferrable(&self) -> bool {
        false
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput;
}

#[derive(Clone, Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Registry {
    /// read, write, edit, glob, grep, shell, git, web_fetch, todo_write, ask_user and subagent.
    pub fn standard() -> Registry {
        let mut r = Registry::default();
        r.add(Read);
        r.add(Write);
        r.add(Edit);
        r.add(Glob);
        r.add(Grep);
        r.add(Shell);
        r.add(Git);
        r.add(WebFetch);
        r.add(TodoWrite);
        r.add(AskUser);
        r.add(Subagent);
        r
    }

    pub fn add(&mut self, t: impl Tool + 'static) {
        self.tools.push(Arc::new(t));
    }

    pub fn add_arc(&mut self, t: Arc<dyn Tool>) {
        self.tools.push(t);
    }

    /// The same tools without `names`.
    pub fn without(&self, names: &[&str]) -> Registry {
        Registry { tools: self.tools.iter().filter(|t| !names.contains(&t.spec().name.as_str())).cloned().collect() }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.spec().name == name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// Whether the tools that can wait ([`Tool::deferrable`]) are too many to send with every
    /// request: then they're found with `tool_search` instead (see [`Registry::declared`]).
    pub fn defers(&self) -> bool {
        self.tools.iter().filter(|t| t.deferrable()).map(|t| spec_chars(&t.spec())).sum::<usize>() > DEFER_ABOVE
    }

    /// The tools a request declares: all of them, or, when the ones that can wait are too many
    /// ([`Registry::defers`]), the others, `tool_search`, and the ones in `active` (found or used
    /// before in the conversation). Always in the registry's order, so a request differs from
    /// the one before only when a tool was found.
    pub fn declared(&self, active: &std::collections::HashSet<String>) -> Vec<ToolSpec> {
        if !self.defers() {
            return self.specs();
        }
        let mut out: Vec<ToolSpec> = self.tools.iter().filter(|t| !t.deferrable()).map(|t| t.spec()).collect();
        // which servers there are to look in (`mcp__<server>__<tool>`), with how many tools each
        let mut servers: Vec<(String, usize)> = Vec::new();
        for t in self.tools.iter().filter(|t| t.deferrable()) {
            let name = t.spec().name;
            let server = name.strip_prefix("mcp__").and_then(|n| n.split_once("__")).map_or(name.as_str(), |(s, _)| s).to_string();
            match servers.iter_mut().find(|(s, _)| *s == server) {
                Some((_, n)) => *n += 1,
                None => servers.push((server, 1)),
            }
        }
        let mut search = ToolSearch.spec();
        search.description.push_str(&format!(" Servers: {}.", servers.iter().map(|(s, n)| format!("{s} ({n} tools)")).collect::<Vec<_>>().join(", ")));
        out.push(search);
        out.extend(self.tools.iter().filter(|t| t.deferrable()).map(|t| t.spec()).filter(|s| active.contains(&s.name)));
        out
    }

    /// The tools that can wait that best match `query` (words, or exact names), best first, at
    /// most `max`.
    pub fn search(&self, query: &str, max: usize) -> Vec<ToolSpec> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.len() > 1).collect();
        let mut scored: Vec<(usize, ToolSpec)> = self
            .tools
            .iter()
            .filter(|t| t.deferrable())
            .map(|t| t.spec())
            .filter_map(|s| {
                let (name, desc) = (s.name.to_lowercase(), s.description.to_lowercase());
                // a name asked for by itself beats any number of words
                let named = q.split([',', ' ']).any(|n| n.trim() == name || name.ends_with(&format!("__{}", n.trim())));
                let score = if named { 1_000 } else { words.iter().map(|w| if name.contains(w) { 3 } else if desc.contains(w) { 1 } else { 0 }).sum() };
                (score > 0).then_some((score, s))
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().take(max).map(|(_, s)| s).collect()
    }
}

/// Characters of the tools that can wait above which they're found with `tool_search` instead of
/// going with every request: about 15,000 tokens (what DevCat points out as a heavy MCP server).
pub const DEFER_ABOVE: usize = 60_000;

fn spec_chars(s: &ToolSpec) -> usize {
    s.name.len() + s.description.len() + s.input_schema.to_string().len()
}

/// Name of the tool that finds the tools left out of the requests; the agent runs it itself.
pub const TOOL_SEARCH: &str = "tool_search";

/// How a `tool_search` result starts: the tools it makes available (the conversation's log is
/// where the agent reads them back, see `agent::active_tools`).
pub const FOUND: &str = "Tools now available: ";

/// `tool_search`: the MCP tools aren't all in the request (there are too many); this finds them.
pub struct ToolSearch;

#[async_trait]
impl Tool for ToolSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_SEARCH.into(),
            description: "Find tools from the user's MCP servers. There are too many to list here, so they're left out until you look for them: describe what you need (\"create a github issue\", \"query postgres\") or give tool names. The tools found can be called from your next message on, and stay available.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "What the tool should do, or tool names separated by commas" } },
                "required": ["query"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input.get("query").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::err("tool_search is answered by the agent, not run.")
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
        let ctx = ToolCtx::at(root);
        assert!(inside(&ctx, &resolve(&ctx, "src/main.rs")));
        assert!(inside(&ctx, &resolve(&ctx, "./a/../b.txt")));
        assert!(!inside(&ctx, &resolve(&ctx, "../other/x")));
        assert!(!inside(&ctx, &resolve(&ctx, if cfg!(windows) { "C:\\projX\\a" } else { "/projX/a" })));
    }

    /// An MCP server's tool, `size` characters of description.
    struct Mcp(&'static str, usize);

    #[async_trait]
    impl Tool for Mcp {
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: self.0.into(), description: format!("{} {}", self.0.replace("__", " "), "x".repeat(self.1)), input_schema: serde_json::json!({ "type": "object" }) }
        }
        fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
            Kind::Exec
        }
        fn summary(&self, _: &Value) -> String {
            String::new()
        }
        fn deferrable(&self) -> bool {
            true
        }
        async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
            ToolOutput::ok("")
        }
    }

    #[test]
    fn many_mcp_tools_wait_until_they_are_looked_for() {
        let names = |s: &[ToolSpec]| s.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        let mut r = Registry::default();
        r.add(Read);
        r.add(Mcp("mcp__github__create_issue", 100));
        // a few: all of them go with every request
        assert!(!r.defers());
        assert_eq!(names(&r.declared(&Default::default())), ["read", "mcp__github__create_issue"]);
        r.add(Mcp("mcp__github__list_pulls", DEFER_ABOVE));
        r.add(Mcp("mcp__pg__query", 100));
        assert!(r.defers());
        let d = r.declared(&Default::default());
        assert_eq!(names(&d), ["read", TOOL_SEARCH]);
        assert!(d[1].description.ends_with("Servers: github (2 tools), pg (1 tools)."), "{}", d[1].description);
        // found ones go along, in the registry's order
        let active = ["mcp__pg__query".to_string(), "mcp__github__create_issue".to_string()].into();
        assert_eq!(names(&r.declared(&active)), ["read", TOOL_SEARCH, "mcp__github__create_issue", "mcp__pg__query"]);
        assert_eq!(names(&r.search("create an issue", 8)), ["mcp__github__create_issue"]);
        assert_eq!(names(&r.search("query", 8)), ["mcp__pg__query"]);
        assert_eq!(names(&r.search("list_pulls, create_issue", 8)), ["mcp__github__create_issue", "mcp__github__list_pulls"]);
        assert!(r.search("zzz", 8).is_empty());
    }

    #[test]
    fn truncate_keeps_both_ends() {
        let s = format!("START{}END", "x\n".repeat(40_000));
        let t = truncate(&s);
        assert!(t.len() < MAX_OUTPUT + 200);
        assert!(t.starts_with("START") && t.ends_with("END") && t.contains("characters, about"));
    }
}
