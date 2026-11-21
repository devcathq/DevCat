//! The user's SSH sessions for agents: `ssh_read`, `ssh_glob`, `ssh_grep` (reading), `ssh_run`,
//! `ssh_write` and `ssh_edit` (changing), on a host the user saved as an SSH session, logging in
//! as its terminals do (key, jump host, saved password). Nothing is installed on the host. They
//! are the DevCat Agent's own tools for a folder on an SSH host (`agent_ssh.rs`) with a `session`
//! input, so an agent working on this computer (Claude Code over `devcat mcp`, or the DevCat
//! Agent in a local folder) can work on a server too.
//!
//! An SSH session logs in with the user's own credentials, so it's opt-in: its AI access is Off
//! until the user sets it (Edit connection → Advanced), unlike other connections. Read lets agents
//! read and search files; Full also run commands and change files (over MCP; the DevCat Agent asks
//! the user first, as for any command). Only where [`Scope::ssh`] says so: never a phone's or a
//! chat app's chat, nor an unattended run (AGENTS.md, "the SSH workbench is desktop-only").
//! Files that hold secrets (keys, `.env`, credentials) aren't read with `ssh_read`.
use super::{arg, in_reach, Scope};
use crate::agent_ssh::{self, Host};
use crate::models::{ConnectionConfig, ConnectionKind, McpAccess};
use crate::state::St;
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long what a host said about itself is kept (its home folder, shell, ripgrep…).
const PROBED_FOR: Duration = Duration::from_secs(10 * 60);

/// A session's AI access: its own setting, Off when it has none (an SSH session logs in as the
/// user, so it's opt-in, not its project's default).
pub fn access(c: &ConnectionConfig) -> McpAccess {
    c.mcp_access.unwrap_or(McpAccess::Off)
}

/// The SSH sessions `scope` may use, with their access.
pub fn sessions(scope: &Scope) -> Vec<(ConnectionConfig, McpAccess)> {
    if !scope.ssh || scope.unattended || scope.room.is_some() {
        return vec![];
    }
    let Some(st) = St::try_get() else { return vec![] };
    let projects = st.projects.list().unwrap_or_default();
    let current = scope.current(&projects);
    st.store
        .load_all()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.kind == ConnectionKind::Ssh && access(c) != McpAccess::Off && in_reach(c.project_id.as_deref(), &projects, current.as_ref()))
        .map(|c| {
            let a = access(&c);
            (c, a)
        })
        .collect()
}

/// The `ssh_*` tools, when `scope` has a session it may use; the changing ones only where they
/// can work (over MCP: a session with Full access).
pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, scope: &Arc<Scope>) {
    let list = sessions(scope);
    if list.is_empty() {
        return;
    }
    for op in [Op::Read, Op::Glob, Op::Grep] {
        out.push(Arc::new(SshTool { scope: scope.clone(), op }));
    }
    if !scope.mcp || list.iter().any(|(_, a)| *a == McpAccess::Full) {
        for op in [Op::Run, Op::Write, Op::Edit] {
            out.push(Arc::new(SshTool { scope: scope.clone(), op }));
        }
    }
}

/// One line per session, for `devcat_connections` and the MCP instructions.
pub fn lines(scope: &Scope) -> Vec<String> {
    sessions(scope)
        .into_iter()
        .map(|(c, a)| {
            let who = if c.user.trim().is_empty() { c.host.clone() } else { format!("{}@{}", c.user.trim(), c.host) };
            let what = if a == McpAccess::Full { "read, run commands and change files" } else { "read and search files only" };
            format!("- {} (ssh {who}; {what}) id {}", c.name, c.id)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Read,
    Glob,
    Grep,
    Run,
    Write,
    Edit,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Read => "ssh_read",
            Op::Glob => "ssh_glob",
            Op::Grep => "ssh_grep",
            Op::Run => "ssh_run",
            Op::Write => "ssh_write",
            Op::Edit => "ssh_edit",
        }
    }

    fn changes(self) -> bool {
        matches!(self, Op::Run | Op::Write | Op::Edit)
    }

    /// The tool it runs, on `host`.
    fn inner(self, host: Arc<Host>) -> Box<dyn Tool> {
        match self {
            Op::Read => Box::new(agent_ssh::Read(host)),
            Op::Glob => Box::new(agent_ssh::Glob(host)),
            Op::Grep => Box::new(agent_ssh::Grep(host)),
            Op::Run => Box::new(agent_ssh::Shell(host)),
            Op::Write => Box::new(agent_ssh::Write(host)),
            Op::Edit => Box::new(agent_ssh::Edit(host)),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Op::Read => "Read a text file on one of the user's SSH sessions (a server they saved in DevCat), with numbered lines; on a folder, list it. `path` is on the host: absolute, `~/…`, or relative to the session's start folder (its home folder unless set). Up to 2000 lines; `offset` / `limit` for more. Files that hold secrets (keys, .env, credentials) aren't read.",
            Op::Glob => "Find files by name pattern on one of the user's SSH sessions, e.g. `**/*.py`, under `path` (default: the session's start folder). Respects .gitignore where the host has ripgrep or git.",
            Op::Grep => "Search file contents with a regular expression on one of the user's SSH sessions, under `path` (a folder or a file; default: the session's start folder). Returns `path:line: text`. Rust regex syntax where the host has ripgrep, else Perl-compatible or extended.",
            Op::Run => "Run a shell command on one of the user's SSH sessions (a server they saved in DevCat; DevCat logs in as the session does) and return its output and exit code. A new login shell each time, in `folder` (default: the session's start folder). Not interactive: no editors, pagers or servers that never exit. Default timeout 120 s, at most 600 s.",
            Op::Write => "Create a file, or replace a file's whole content, on one of the user's SSH sessions. Creates missing folders. `path` is on the host. Prefer `ssh_edit` to change part of a file.",
            Op::Edit => "Replace an exact piece of text in a file on one of the user's SSH sessions. `old_string` must appear exactly once (include surrounding lines), unless `replace_all`. Read the file with `ssh_read` first.",
        }
    }
}

struct SshTool {
    scope: Arc<Scope>,
    op: Op,
}

/// What a host said about itself, and when.
type Probed = HashMap<String, (Instant, Arc<Host>)>;

/// What hosts said about themselves, by session id (see [`PROBED_FOR`]).
static PROBED: once_cell::sync::Lazy<parking_lot::Mutex<Probed>> = once_cell::sync::Lazy::new(Default::default);

/// The host of session `c`, in its start folder.
async fn host_of(c: &ConnectionConfig) -> Result<Arc<Host>, ToolOutput> {
    if let Some((at, h)) = PROBED.lock().get(&c.id)
        && at.elapsed() < PROBED_FOR
        && h.unreachable.is_none()
    {
        return Ok(h.clone());
    }
    let start = c.options.get("startDir").map(|d| d.trim()).filter(|d| !d.is_empty()).unwrap_or("~");
    let start = if start.starts_with('/') || start.starts_with('~') { start.to_string() } else { format!("~/{start}") };
    let uri = format!("{}{}{}/{}", crate::claude::remote::SCHEME, crate::claude::remote::SESSION_PREFIX, c.id, start.trim_start_matches('/'));
    let h = Arc::new(agent_ssh::probe(&uri).await.map_err(|e| ToolOutput::err(e.to_string()))?);
    if let Some(why) = &h.unreachable {
        return Err(ToolOutput::err(why.clone()));
    }
    PROBED.lock().insert(c.id.clone(), (Instant::now(), h.clone()));
    Ok(h)
}

impl SshTool {
    /// The session the model names, if this scope may use it for this.
    fn session(&self, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
        let list = sessions(&self.scope);
        let names = || list.iter().map(|(c, _)| c.name.as_str()).collect::<Vec<_>>().join(", ");
        let Some(want) = arg(input, "session") else {
            return Err(ToolOutput::err(format!("Missing `session`: the name or id of an SSH session. Sessions: {}.", names())));
        };
        let found: Vec<&(ConnectionConfig, McpAccess)> = match list.iter().find(|(c, _)| c.id == want) {
            Some(c) => vec![c],
            None => list.iter().filter(|(c, _)| c.name.eq_ignore_ascii_case(&want)).collect(),
        };
        let (c, a) = match found.as_slice() {
            [one] => (*one).clone(),
            [] => return Err(ToolOutput::err(format!("No SSH session agents may use is called \"{want}\". Sessions: {}. The user turns AI access on for a session in DevCat (Edit connection → Advanced).", names()))),
            _ => return Err(ToolOutput::err(format!("Several SSH sessions are called \"{want}\"; use the id: {}.", found.iter().map(|(c, _)| c.id.as_str()).collect::<Vec<_>>().join(", ")))),
        };
        if self.op.changes() && self.scope.mcp && a != McpAccess::Full {
            return Err(ToolOutput::err(format!(
                "DevCat only lets agents read files on {} over MCP. To allow commands and changes, the user sets the session's AI access to \"Full\" in DevCat (Edit connection → Advanced).",
                c.name
            )));
        }
        Ok(c)
    }
}

/// A file that holds secrets, by its path on the host (any case: macOS hosts don't mind), or
/// what the host's processes hold (`/proc`: their environment, memory).
fn secret(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower == "/proc" || lower.starts_with("/proc/") || crate::integrations::text::secret_like(std::path::Path::new(&lower))
}

/// Why `path` (from the model) isn't read for agents, if it isn't: by its name, and by what it
/// links to on the host.
async fn refused(host: &Host, path: &str) -> Option<String> {
    let full = host.path(path);
    let real = host.real_path(&full).await;
    [Some(full), real].iter().flatten().any(|p| secret(p)).then(|| format!("{path} looks like it holds secrets (a key, .env, credentials or /proc): DevCat doesn't read it for agents."))
}

/// `ssh_grep`'s output without the lines of secret files (`path:line: text`, paths relative to
/// `root`).
fn without_secrets(out: ToolOutput, host: &Host, root: &str) -> ToolOutput {
    if out.is_error {
        return out;
    }
    let base = host.path(root);
    let kept: Vec<&str> = out
        .content
        .lines()
        .filter(|l| match l.split_once(':') {
            Some((rel, _)) if !l.starts_with('(') => !secret(&format!("{base}/{rel}")) && !secret(rel),
            _ => true,
        })
        .collect();
    if kept.iter().all(|l| l.starts_with('(')) {
        return ToolOutput::ok("No matches.");
    }
    ToolOutput { content: kept.join("\n"), ..out }
}

#[async_trait]
impl Tool for SshTool {
    fn spec(&self) -> ToolSpec {
        // the inner tool's input, with `session` (and `folder` for a command)
        let mut schema = self.op.inner(Arc::new(placeholder())).spec().input_schema;
        let props = schema["properties"].as_object_mut().expect("tool schema");
        props.insert("session".into(), json!({ "type": "string", "description": "The SSH session: name or id (see `devcat_connections`)" }));
        if self.op == Op::Run {
            props.insert("folder".into(), json!({ "type": "string", "description": "Folder on the host to run in (default: the session's start folder)" }));
        }
        let required = schema["required"].as_array_mut().expect("tool schema");
        required.insert(0, json!("session"));
        ToolSpec { name: self.op.name().into(), description: self.op.description().into(), input_schema: schema }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        match self.op {
            Op::Read | Op::Glob | Op::Grep => Kind::Read,
            Op::Run => Kind::Exec,
            // never in the chat's project: on another machine
            Op::Write | Op::Edit => Kind::EditOutside,
        }
    }
    fn summary(&self, input: &Value) -> String {
        let what = self.op.inner(Arc::new(placeholder())).summary(input);
        format!("{}: {what}", arg(input, "session").unwrap_or_else(|| "?".into()))
    }
    fn timeout(&self, input: &Value) -> Duration {
        // a first call connects (and may start the session's VPN) before running
        self.op.inner(Arc::new(placeholder())).timeout(input) + Duration::from_secs(45)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match self.session(&input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let mut host = match host_of(&c).await {
            Ok(h) => h,
            Err(e) => return e,
        };
        if matches!(self.op, Op::Read | Op::Grep)
            && let Some(why) = refused(&host, &arg(&input, "path").unwrap_or_else(|| ".".into())).await
        {
            return ToolOutput::err(why);
        }
        if self.op == Op::Run
            && let Some(f) = arg(&input, "folder")
        {
            let mut h = (*host).clone();
            h.root = host.path(&f);
            host = Arc::new(h);
        }
        let mut inner_input = input.clone();
        if let Some(m) = inner_input.as_object_mut() {
            m.remove("session");
            m.remove("folder");
        }
        let root = arg(&input, "path").unwrap_or_else(|| ".".into());
        let out = self.op.inner(host.clone()).run(inner_input, ctx).await;
        // a search over a folder doesn't show what's in its secret files either
        if self.op == Op::Grep { without_secrets(out, &host, &root) } else { out }
    }
}

/// A host for building specs and summaries (they don't depend on it).
fn placeholder() -> Host {
    Host {
        uri: String::new(),
        label: "the host".into(),
        root: "~".into(),
        home: None,
        os: String::new(),
        shell: "bash".into(),
        rg: true,
        timeout: true,
        git: None,
        rules: vec![],
        unreachable: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, access: Option<McpAccess>) -> ConnectionConfig {
        serde_json::from_value(json!({ "id": id, "name": id, "kind": "ssh", "host": "box", "user": "me", "mcpAccess": access })).unwrap()
    }

    #[test]
    fn ssh_sessions_are_off_for_agents_until_the_user_turns_them_on() {
        assert_eq!(access(&session("a", None)), McpAccess::Off);
        assert_eq!(access(&session("b", Some(McpAccess::Read))), McpAccess::Read);
        assert_eq!(access(&session("c", Some(McpAccess::Full))), McpAccess::Full);
    }

    #[test]
    fn specs_ask_for_the_session_and_keep_the_tools_inputs() {
        let scope = Arc::new(Scope { mcp: true, ssh: true, ..Scope::default() });
        let run = SshTool { scope: scope.clone(), op: Op::Run }.spec();
        assert_eq!(run.name, "ssh_run");
        assert_eq!(run.input_schema["required"], json!(["session", "command"]));
        assert!(run.input_schema["properties"].get("folder").is_some());
        let edit = SshTool { scope: scope.clone(), op: Op::Edit }.spec();
        assert_eq!(edit.input_schema["required"], json!(["session", "path", "old_string", "new_string"]));
        let t = SshTool { scope, op: Op::Write };
        assert_eq!(t.kind(&json!({}), &ToolCtx::at("/p")), Kind::EditOutside);
        assert_eq!(t.summary(&json!({ "session": "edge", "path": "/etc/x" })), "edge: /etc/x");
    }

    #[test]
    fn secrets_on_the_host_are_not_read() {
        assert!(secret("/home/me/.ssh/id_ed25519") && secret("/srv/app/.env") && secret("/srv/app/certs/server.key"));
        assert!(secret("/srv/app/.GIT/config") && secret("/proc/1/environ") && secret("/proc"));
        assert!(!secret("/srv/app/src/main.py") && !secret("/etc/nginx/nginx.conf") && !secret("/process/x"));
        let h = placeholder();
        let hits = ToolOutput::ok("src/a.py:3: key = 1\ncerts/server.key:1: -----BEGIN\n(more than 200 matches; showing the first 200)");
        assert_eq!(without_secrets(hits, &h, ".").content, "src/a.py:3: key = 1\n(more than 200 matches; showing the first 200)");
        assert_eq!(without_secrets(ToolOutput::ok("id_rsa:1: x"), &h, "~/.ssh").content, "No matches.");
    }

    /// Phones, chat apps and unattended runs never get them, whatever the sessions say.
    #[test]
    fn only_where_the_scope_allows_ssh() {
        for scope in [Scope::default(), Scope { ssh: true, unattended: true, ..Scope::default() }, Scope { ssh: true, room: Some("r".into()), ..Scope::default() }] {
            assert!(sessions(&scope).is_empty());
        }
    }
}
