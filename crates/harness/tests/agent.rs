//! The agent loop end to end with a scripted model: tool calls, approvals, cancellation, resume.
use async_trait::async_trait;
use devcat_harness::approval::{ApprovalRequest, DenyAll, Kind};
use devcat_harness::llm::{Delta, Reply, Request};
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Registry, Tool, ToolCtx, ToolOutput};
use devcat_harness::{Agent, AgentEvent, Approver, Block, Cancel, Decision, Error, Model, Policy, Role, Session, SessionEvent, TurnEnd, Usage};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Answers each step from a script and records what it was sent.
struct Scripted {
    steps: Mutex<Vec<Vec<Block>>>,
    seen: Mutex<Vec<usize>>, // messages per request
    systems: Mutex<Vec<String>>, // system prompt per request
}

impl Scripted {
    fn new(steps: Vec<Vec<Block>>) -> Arc<Scripted> {
        Arc::new(Scripted { steps: Mutex::new(steps.into_iter().rev().collect()), seen: Mutex::default(), systems: Mutex::default() })
    }
}

#[async_trait]
impl Model for Scripted {
    fn name(&self) -> String {
        "scripted".into()
    }
    async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        self.seen.lock().unwrap().push(req.messages.len());
        self.systems.lock().unwrap().push(req.system.to_string());
        let content = self.steps.lock().unwrap().pop().expect("the model was asked more steps than scripted");
        for b in &content {
            if let Block::Text { text } = b {
                on(Delta::Text(text.clone()));
            }
        }
        Ok(Reply { content, usage: Usage { input_tokens: 100, output_tokens: 10, cache_read_tokens: 0 }, truncated: false })
    }
}

/// A model that never answers (to test stopping).
struct Hang;

#[async_trait]
impl Model for Hang {
    fn name(&self) -> String {
        "hang".into()
    }
    async fn step(&self, _: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        std::future::pending().await
    }
}

struct Answer(Decision, Mutex<Vec<String>>);

#[async_trait]
impl Approver for Answer {
    async fn decide(&self, req: &ApprovalRequest) -> (Decision, String) {
        self.1.lock().unwrap().push(format!("{}: {}", req.tool, req.summary));
        (self.0.clone(), "test".into())
    }
}

fn tool(id: &str, name: &str, input: Value) -> Block {
    Block::ToolUse { id: id.into(), name: name.into(), input }
}

fn text(t: &str) -> Block {
    Block::Text { text: t.into() }
}

async fn project() -> (PathBuf, Session) {
    let dir = std::env::temp_dir().join(format!("devcat-harness-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    std::fs::write(dir.join("proj/notes.txt"), "version = 1\n").unwrap();
    let s = Session::create(&dir.join("sessions"), dir.join("proj").to_str().unwrap()).await.unwrap();
    (dir, s)
}

#[tokio::test]
async fn edits_a_file_after_approval_and_answers() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![
        vec![text("Let me look."), tool("t1", "read", json!({ "path": "notes.txt" }))],
        vec![tool("t2", "edit", json!({ "path": "notes.txt", "old_string": "version = 1", "new_string": "version = 2" }))],
        vec![text("Bumped the version to 2.")],
    ]);
    let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, approver.clone(), "system".into());
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "bump the version", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();

    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(std::fs::read_to_string(dir.join("proj/notes.txt")).unwrap(), "version = 2\n");
    // only the edit asked: reads never do
    assert_eq!(*approver.1.lock().unwrap(), vec!["edit: notes.txt"]);
    // each step saw the growing conversation
    assert_eq!(*model.seen.lock().unwrap(), vec![1, 3, 5]);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolResult { id, is_error: false, content } if id == "t1" && content.contains("version = 1"))));

    // the log has everything, and resumes into the same conversation
    let resumed = Session::open(session.path()).await.unwrap();
    assert!(resumed.events().iter().any(|e| matches!(e, SessionEvent::Approval { allowed: true, tool, .. } if tool == "edit")));
    assert_eq!(resumed.messages().len(), 6);
    assert_eq!(resumed.messages().last().unwrap().text(), "Bumped the version to 2.");
    assert_eq!(resumed.usage().input_tokens, 300);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_refused_command_goes_back_to_the_model_as_an_error() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![vec![tool("t1", "shell", json!({ "command": "echo should-not-run > ran.txt" }))], vec![text("OK, I won't run it.")]]);
    // nobody to ask: fail closed
    let mut agent = Agent::new(model, Registry::standard(), Policy::AutoEdit, Arc::new(DenyAll), "system".into());
    let mut results = Vec::new();
    agent
        .turn(&mut session, "run it", &Cancel::default(), &mut |e| {
            if let AgentEvent::ToolResult { content, is_error, .. } = e {
                results.push((content, is_error));
            }
        })
        .await
        .unwrap();
    assert!(!dir.join("proj/ran.txt").exists());
    assert_eq!(results.len(), 1);
    assert!(results[0].1 && results[0].0.starts_with("The user declined this call."));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn allow_for_session_stops_asking_and_bypass_never_asks() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![
        vec![tool("a", "write", json!({ "path": "a.txt", "content": "a" }))],
        vec![tool("b", "write", json!({ "path": "b.txt", "content": "b" }))],
        vec![text("done")],
    ]);
    let approver = Arc::new(Answer(Decision::AllowForSession, Mutex::default()));
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, approver.clone(), "s".into());
    agent.turn(&mut session, "write two files", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!(approver.1.lock().unwrap().len(), 1);
    assert!(dir.join("proj/a.txt").exists() && dir.join("proj/b.txt").exists());

    let model = Scripted::new(vec![vec![tool("c", "shell", json!({ "command": "echo hi" }))], vec![text("done")]]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Bypass, Arc::new(DenyAll), "s".into());
    let mut ok = false;
    agent.turn(&mut session, "say hi", &Cancel::default(), &mut |e| {
        if let AgentEvent::ToolResult { content, is_error: false, .. } = e {
            ok = content.trim() == "hi";
        }
    }).await.unwrap();
    assert!(ok);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stopping_a_turn_is_recorded_and_the_session_stays_usable() {
    let (dir, mut session) = project().await;
    let mut agent = Agent::new(Arc::new(Hang), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        c2.cancel();
    });
    let r = agent.turn(&mut session, "think forever", &cancel, &mut |_| {}).await;
    assert!(matches!(r, Err(Error::Cancelled)));
    let resumed = Session::open(session.path()).await.unwrap();
    assert!(matches!(resumed.events().last(), Some(SessionEvent::Interrupted { .. })));
    assert_eq!(resumed.messages().len(), 1);
    assert_eq!(resumed.messages()[0].role, Role::User);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn unknown_tools_and_bad_arguments_are_reported_to_the_model() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![
        vec![tool("x", "teleport", json!({})), tool("y", "read", json!({ "_unparsed_arguments": "{\"path\": " }))],
        vec![text("Sorry.")],
    ]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut errs = Vec::new();
    agent.turn(&mut session, "go", &Cancel::default(), &mut |e| {
        if let AgentEvent::ToolResult { content, is_error: true, .. } = e {
            errs.push(content);
        }
    }).await.unwrap();
    assert_eq!(errs.len(), 2);
    assert!(errs[0].contains("no tool named `teleport`") && errs[1].contains("weren't valid JSON"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn compacts_when_the_context_fills_up_and_continues_from_the_summary() {
    let (dir, mut session) = project().await;
    // every scripted step reports 110 tokens; a 120-token window compacts above 96
    let model = Scripted::new(vec![
        vec![tool("t1", "read", json!({ "path": "notes.txt" }))],
        vec![text("1. Request: bump the version.\n5. State now: notes.txt read, version = 1.")],
        vec![text("The version is 1.")],
    ]);
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "system".into());
    agent.context_window = Some(120);
    let mut events = Vec::new();
    agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();

    // step 1, then the summary request (whole conversation + the ask), then a step from the summary only
    assert_eq!(*model.seen.lock().unwrap(), vec![1, 3, 1]);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Compacted { summary, .. } if summary.contains("State now"))));
    let msgs = session.messages();
    assert!(msgs[0].text().starts_with("This session continues an earlier conversation") && msgs[0].text().contains("version = 1"));
    assert_eq!(msgs.last().unwrap().text(), "The version is 1.");
    // the full history is still in the log, and resuming keeps the summary as the start
    let resumed = Session::open(session.path()).await.unwrap();
    assert!(resumed.events().iter().any(|e| matches!(e, SessionEvent::User { text, .. } if text == "what's the version?")));
    assert_eq!(resumed.messages().len(), msgs.len());
    // the summary step counts in the session's usage
    assert_eq!(resumed.usage().input_tokens, 300);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn compact_on_request_and_nothing_to_compact() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![vec![text("hi")], vec![text("Summary: greeted.")]]);
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "system".into());
    assert!(agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.is_err());
    agent.turn(&mut session, "hello", &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.compact(&mut session, Some("the greeting"), &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!(session.messages().len(), 1);
    assert!(session.messages()[0].text().contains("Summary: greeted."));
    assert!(session.context_tokens() < 20);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn todo_list_needs_no_approval_and_fetch_asks() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![
        vec![tool("t1", "todo_write", json!({ "todos": [{ "content": "Read", "status": "completed" }, { "content": "Fix", "status": "in_progress" }] }))],
        vec![tool("t2", "web_fetch", json!({ "url": "https://example.invalid/x" }))],
        vec![text("done")],
    ]);
    let approver = Arc::new(Answer(Decision::Deny { reason: None }, Mutex::default()));
    let mut agent = Agent::new(model, Registry::standard(), Policy::AutoEdit, approver.clone(), "s".into());
    let mut results = Vec::new();
    agent
        .turn(&mut session, "plan it", &Cancel::default(), &mut |e| {
            if let AgentEvent::ToolResult { content, .. } = e {
                results.push(content);
            }
        })
        .await
        .unwrap();
    assert_eq!(*approver.1.lock().unwrap(), vec!["web_fetch: https://example.invalid/x"]);
    assert_eq!(results[0], "Todo list updated:\n[x] Read\n[>] Fix");
    assert!(results[1].starts_with("The user declined"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn plan_mode_reads_asks_for_fetches_and_refuses_changes() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![
        vec![tool("r1", "read", json!({ "path": "notes.txt" }))],
        vec![tool("w1", "write", json!({ "path": "new.txt", "content": "x" }))],
        vec![tool("s1", "shell", json!({ "command": "echo hi" }))],
        vec![tool("f1", "web_fetch", json!({ "url": "https://example.invalid/" }))],
        vec![text("Plan: 1. bump the version in notes.txt")],
        // the user starts the plan: the next turn may change things
        vec![tool("w2", "write", json!({ "path": "new.txt", "content": "x" }))],
        vec![text("done")],
    ]);
    let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Plan, approver.clone(), "base prompt".into());
    let mut results = Vec::new();
    agent
        .turn(&mut session, "plan the version bump", &Cancel::default(), &mut |e| {
            if let AgentEvent::ToolResult { content, is_error, .. } = e {
                results.push((content, is_error));
            }
        })
        .await
        .unwrap();
    assert!(!results[0].1, "read runs: {:?}", results[0]);
    for (i, what) in [(1, "write"), (2, "shell")] {
        assert!(results[i].1 && results[i].0.contains("plan mode is on"), "{what} refused: {:?}", results[i]);
    }
    assert!(!dir.join("proj/new.txt").exists());
    // a fetch only reads, so it asks (and runs once allowed; this one fails to resolve)
    assert_eq!(*approver.1.lock().unwrap(), vec!["web_fetch: https://example.invalid/"]);
    assert!(model.systems.lock().unwrap().iter().all(|s| s.starts_with("base prompt") && s.contains("# Plan mode")));

    agent.policy = Policy::Ask;
    agent.turn(&mut session, "go ahead", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert!(dir.join("proj/new.txt").exists());
    assert!(!model.systems.lock().unwrap().last().unwrap().contains("# Plan mode"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_same_call_over_and_over_is_refused_after_three() {
    let (dir, mut session) = project().await;
    let notes = || json!({ "path": "notes.txt" });
    let model = Scripted::new(vec![
        vec![tool("r1", "read", notes())],
        vec![tool("r2", "read", notes())],
        vec![tool("r3", "read", notes())],
        vec![tool("r4", "read", notes())],
        // a different call in between starts the count again
        vec![tool("g1", "glob", json!({ "pattern": "*.txt" }))],
        vec![tool("r5", "read", notes())],
        vec![text("done")],
    ]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut results = Vec::new();
    agent
        .turn(&mut session, "read the notes", &Cancel::default(), &mut |e| {
            if let AgentEvent::ToolResult { content, is_error, .. } = e {
                results.push((content, is_error));
            }
        })
        .await
        .unwrap();
    assert_eq!(results.iter().map(|r| r.1).collect::<Vec<_>>(), [false, false, false, true, false, false]);
    assert!(results[3].0.contains("3 times in a row"), "{}", results[3].0);
    let _ = std::fs::remove_dir_all(dir);
}

/// A tool with more output than fits in one result.
struct Big;

#[async_trait]
impl Tool for Big {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "big".into(), description: "Prints a lot.".into(), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "big".into()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::ok(format!("START\n{}END", "a line of output\n".repeat(5_000)))
    }
}

#[tokio::test]
async fn long_output_is_cut_for_the_model_and_kept_whole_in_a_file() {
    let (dir, mut session) = project().await;
    std::fs::write(dir.join("proj/long.txt"), "a long line in the file\n".repeat(3_000)).unwrap();
    let model = Scripted::new(vec![vec![tool("b1", "big", json!({}))], vec![tool("r1", "read", json!({ "path": "long.txt" }))], vec![text("done")]]);
    let mut tools = Registry::standard();
    tools.add(Big);
    let mut agent = Agent::new(model, tools, Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut results = Vec::new();
    agent
        .turn(&mut session, "run it", &Cancel::default(), &mut |e| {
            if let AgentEvent::ToolResult { content, .. } = e {
                results.push(content);
            }
        })
        .await
        .unwrap();
    let saved = session.path().with_extension("out").join("b1.txt");
    let whole = format!("START\n{}END", "a line of output\n".repeat(5_000));
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), whole);
    let big = &results[0];
    assert!(big.len() < 31_000 && big.starts_with("START") && big.contains("characters, about"), "{}", &big[..200]);
    assert!(big.ends_with(&format!("[The whole output ({} characters) is saved in {}. Read parts of it with `read` (offset / limit) or search it with `grep`.]", whole.len(), saved.display())), "{}", &big[big.len() - 300..]);
    // read's own output is cut but not saved again: its file is there to page through
    assert!(results[1].contains("characters, about") && !results[1].contains("is saved in"));
    assert!(!session.path().with_extension("out").join("r1.txt").exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// A Claude Code hook: `sh` everywhere but Windows, where Codex's `commandWindows` runs in PowerShell.
fn hook(sh: &str, ps: &str) -> Value {
    json!({ "type": "command", "command": sh, "commandWindows": ps })
}

#[tokio::test]
async fn hooks_written_for_claude_code_block_add_context_answer_approvals_and_keep_the_agent_going() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    let settings = json!({ "hooks": {
        "SessionStart": [{ "hooks": [hook("echo 'session context'", "Write-Output 'session context'")] }],
        "UserPromptSubmit": [{ "hooks": [hook(
            "if grep -q password; then echo 'no passwords in prompts' >&2; exit 2; fi; echo 'branch: main'",
            "$i = [Console]::In.ReadToEnd(); if ($i -match 'password') { [Console]::Error.WriteLine('no passwords in prompts'); exit 2 }; Write-Output 'branch: main'",
        )] }],
        "PreToolUse": [{ "matcher": "Bash", "hooks": [hook(
            "if grep -q 'rm -rf'; then echo 'no rm here' >&2; exit 2; fi",
            "$i = [Console]::In.ReadToEnd(); if ($i -match 'rm -rf') { [Console]::Error.WriteLine('no rm here'); exit 2 }",
        )] }],
        "PostToolUse": [{ "matcher": "Edit|Write", "hooks": [hook(
            r#"cat > post.json; echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"formatter ran"}}'"#,
            r#"[IO.File]::WriteAllText((Join-Path (Get-Location) 'post.json'), [Console]::In.ReadToEnd()); Write-Output '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"formatter ran"}}'"#,
        )] }],
        "PermissionRequest": [{ "matcher": "WebFetch", "hooks": [hook(
            r#"echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"no web"}}}'"#,
            r#"Write-Output '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"no web"}}}'"#,
        )] }],
        "Stop": [{ "hooks": [hook(
            r#"if grep -q '"stop_hook_active":false'; then echo '{"decision":"block","reason":"run the tests first"}'; fi"#,
            r#"$i = [Console]::In.ReadToEnd(); if ($i -match '"stop_hook_active":false') { Write-Output '{"decision":"block","reason":"run the tests first"}' }"#,
        )] }],
    }});
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let found = devcat_harness::hooks::load(&proj, None);
    assert_eq!(found.files, vec![(".claude/settings.json".to_string(), 6)]);

    let model = Scripted::new(vec![
        vec![tool("t1", "shell", json!({ "command": "rm -rf build" }))],
        vec![tool("t2", "write", json!({ "path": "notes2.txt", "content": "x" }))],
        vec![tool("t3", "web_fetch", json!({ "url": "https://example.invalid/" }))],
        vec![text("Done.")],
        // the Stop hook sent it back once
        vec![text("Tests pass.")],
    ]);
    let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, approver.clone(), "s".into());
    agent.hooks = Arc::new(found.hooks);
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "please clean up", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    let results: Vec<(String, bool)> = events.iter().filter_map(|e| match e {
        AgentEvent::ToolResult { content, is_error, .. } => Some((content.clone(), *is_error)),
        _ => None,
    }).collect();
    // PreToolUse blocked the command, with the hook's reason; nobody was asked
    assert_eq!(results[0], ("Not run: a PreToolUse hook blocked it. no rm here".to_string(), true));
    // the write asked as usual, ran, and the PostToolUse hook's context came with its result
    assert!(!results[1].1 && results[1].0.ends_with("<hook-context>\nformatter ran\n</hook-context>"), "{:?}", results[1]);
    // PermissionRequest answered the fetch instead of the user
    assert_eq!(results[2], ("Not run: a PermissionRequest hook declined it. no web".to_string(), true));
    assert_eq!(*approver.1.lock().unwrap(), vec!["write: notes2.txt"]);
    assert!(session.events().iter().any(|e| matches!(e, SessionEvent::Approval { tool, allowed: false, by, .. } if tool == "web_fetch" && by == "hook")));
    // the hook got the call the way Claude Code describes it
    let post: Value = serde_json::from_str(std::fs::read_to_string(proj.join("post.json")).unwrap().trim_start_matches('\u{feff}')).unwrap();
    assert_eq!((post["hook_event_name"].as_str(), post["tool_name"].as_str(), post["tool_use_id"].as_str()), (Some("PostToolUse"), Some("Write"), Some("t2")));
    assert_eq!(post["tool_input"]["file_path"].as_str(), Some(proj.join("notes2.txt").to_str().unwrap()));
    assert_eq!((post["session_id"].as_str(), post["permission_mode"].as_str()), (Some(session.id.as_str()), Some("default")));
    // SessionStart and UserPromptSubmit added context to the message: the model got it
    let first = &session.messages()[0];
    let texts: Vec<&str> = first.content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect();
    assert_eq!(texts, ["please clean up", "<hook-context>\nsession context\n</hook-context>", "<hook-context>\nbranch: main\n</hook-context>"]);
    // the Stop hook sent the agent back to work once, and the chat says why
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Hook { event, message } if event == "Stop" && message == "Stop hook feedback:\nrun the tests first")));
    assert!(session.events().iter().any(|e| matches!(e, SessionEvent::User { hook: Some(h), text, .. } if h == "Stop" && text.ends_with("run the tests first"))));
    assert_eq!(session.messages().last().unwrap().text(), "Tests pass.");
    assert_eq!(session.title(), "please clean up");

    // UserPromptSubmit can block a message: it isn't recorded, and the turn says why
    let before = session.events().len();
    let blocked = agent.turn(&mut session, "my password is hunter2", &Cancel::default(), &mut |_| {}).await;
    assert!(matches!(&blocked, Err(Error::Hook(m)) if m == "A hook blocked this message: no passwords in prompts"), "{blocked:?}");
    assert_eq!(session.events().len(), before);
    let _ = std::fs::remove_dir_all(dir);
}

/// Answers from one script per conversation, picked by its first user message (the user's task or a
/// helper's prompt), so helpers running at the same time each get their own. Records each request's
/// first message, system prompt and tool names. With a `gate`, helpers' first steps wait for each
/// other there.
struct Router {
    scripts: Mutex<HashMap<String, Vec<Vec<Block>>>>,
    seen: Mutex<Vec<(String, String, Vec<String>)>>,
    gate: Option<tokio::sync::Barrier>,
}

impl Router {
    fn new(scripts: Vec<(&str, Vec<Vec<Block>>)>, gate: Option<usize>) -> Arc<Router> {
        let scripts = scripts.into_iter().map(|(k, s)| (k.to_string(), s.into_iter().rev().collect())).collect();
        Arc::new(Router { scripts: Mutex::new(scripts), seen: Mutex::default(), gate: gate.map(tokio::sync::Barrier::new) })
    }

    /// The requests of the conversation that started with `first`: (system prompt, tool names).
    fn requests(&self, first: &str) -> Vec<(String, Vec<String>)> {
        self.seen.lock().unwrap().iter().filter(|s| s.0 == first).map(|s| (s.1.clone(), s.2.clone())).collect()
    }
}

#[async_trait]
impl Model for Router {
    fn name(&self) -> String {
        "router".into()
    }
    async fn step(&self, req: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        let first = req.messages[0].text();
        self.seen.lock().unwrap().push((first.clone(), req.system.to_string(), req.tools.iter().map(|t| t.name.clone()).collect()));
        if let Some(gate) = &self.gate
            && req.system.contains("# You're a helper")
            && req.messages.len() == 1
        {
            gate.wait().await;
        }
        let content = self.scripts.lock().unwrap().get_mut(&first).and_then(|s| s.pop()).unwrap_or_else(|| panic!("no step left for {first:?}"));
        Ok(Reply { content, usage: Usage { input_tokens: 100, output_tokens: 10, cache_read_tokens: 0 }, truncated: false })
    }
}

fn helper_call(id: &str, description: &str, prompt: &str) -> Block {
    tool(id, "subagent", json!({ "description": description, "prompt": prompt }))
}

#[tokio::test]
async fn helpers_run_at_the_same_time_only_look_and_count_for_the_chat() {
    let (dir, mut session) = project().await;
    let model = Router::new(
        vec![
            ("find the version and the tests", vec![
                vec![text("Asking two helpers."), helper_call("h1", "Find the version", "Where is the version set?"), helper_call("h2", "Find the tests", "Are there tests?")],
                vec![text("The version is 1, and there are no tests.")],
            ]),
            ("Where is the version set?", vec![vec![tool("h1r", "read", json!({ "path": "notes.txt" }))], vec![text("In notes.txt: version = 1.")]]),
            ("Are there tests?", vec![
                // a tool a helper doesn't have, and a command
                vec![tool("h2w", "write", json!({ "path": "x.txt", "content": "x" })), tool("h2c", "git", json!({ "args": ["commit", "-m", "x"] }))],
                vec![tool("h2g", "glob", json!({ "pattern": "**/*test*" }))],
                vec![text("No tests.")],
            ]),
        ],
        // each helper's first step waits for the other's: one after the other, this would hang
        Some(2),
    );
    let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, approver.clone(), "base prompt".into());
    let mut events = Vec::new();
    let (cancel, mut on) = (Cancel::default(), |e| events.push(e));
    let turn = agent.turn(&mut session, "find the version and the tests", &cancel, &mut on);
    let end = tokio::time::timeout(Duration::from_secs(10), turn).await.expect("the helpers ran one after the other").unwrap();
    assert_eq!(end, TurnEnd::Answered);

    let results: Vec<(String, String, bool)> = events.iter().filter_map(|e| match e {
        AgentEvent::ToolResult { id, content, is_error } => Some((id.clone(), content.clone(), *is_error)),
        _ => None,
    }).collect();
    assert_eq!(results.len(), 2, "one result per helper: {results:?}");
    assert!(results.contains(&("h1".into(), "In notes.txt: version = 1.".into(), false)));
    assert!(results.contains(&("h2".into(), "No tests.".into(), false)));
    // what the helpers did shows on their cards, and their tokens count
    let did: Vec<(String, String)> = events.iter().filter_map(|e| match e {
        AgentEvent::HelperTool { id, name, .. } => Some((id.clone(), name.clone())),
        _ => None,
    }).collect();
    assert_eq!(did.len(), 2);
    assert!(did.contains(&("h1".into(), "read".into())) && did.contains(&("h2".into(), "glob".into())));
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::HelperStep { .. })).count(), 5);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { id, name, summary, .. } if id == "h1" && name == "subagent" && summary == "Find the version")));

    // nothing was written or asked
    assert!(!dir.join("proj/x.txt").exists());
    assert!(approver.1.lock().unwrap().is_empty());
    let h2 = Session::open(&session.path().with_extension("agents").join("h2.jsonl")).await.unwrap();
    let refused: Vec<String> = h2.events().iter().filter_map(|e| match e {
        SessionEvent::ToolResults { results, .. } => Some(results.clone()),
        _ => None,
    }).flatten().filter_map(|b| match b {
        Block::ToolResult { content, is_error: true, .. } => Some(content),
        _ => None,
    }).collect();
    assert_eq!(refused.len(), 2);
    assert!(refused[0].contains("no tool named `write`") && refused[1].contains("a helper only looks"), "{refused:?}");

    // helpers get the user's rules and their own section, and only the tools that look
    for (system, tools) in model.requests("Are there tests?") {
        assert!(system.starts_with("base prompt") && system.contains("# You're a helper"));
        for hidden in ["write", "edit", "shell", "todo_write", "ask_user", "subagent"] {
            assert!(!tools.contains(&hidden.to_string()), "a helper has {hidden}");
        }
        assert!(["read", "glob", "grep", "git", "web_fetch"].iter().all(|t| tools.contains(&t.to_string())));
    }
    let (main_system, main_tools) = model.requests("find the version and the tests").remove(0);
    assert!(!main_system.contains("# You're a helper") && main_tools.contains(&"subagent".to_string()));

    // the chat's tokens include the helpers' (2 + 2 + 3 steps), also after a resume; the
    // conversation has their answers in call order
    assert_eq!(session.usage().input_tokens, 700);
    let resumed = Session::open(session.path()).await.unwrap();
    assert_eq!(resumed.usage().input_tokens, 700);
    let m = resumed.messages();
    assert!(matches!(&m[2].content[..], [Block::ToolResult { tool_use_id: a, .. }, Block::ToolResult { tool_use_id: b, .. }] if a == "h1" && b == "h2"));
    // each call's time is kept for the trace, in call order
    assert!(resumed.events().iter().all(|e| match e {
        SessionEvent::ToolResults { results, took_ms, .. } => took_ms.len() == results.len(),
        _ => true,
    }));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_helper_never_asks_and_runs_in_plan_mode() {
    let (dir, mut session) = project().await;
    let model = Router::new(
        vec![
            ("plan the docs update", vec![vec![helper_call("h1", "Read the docs", "What do the docs at https://example.invalid/docs say?")], vec![text("Plan: …")]]),
            ("What do the docs at https://example.invalid/docs say?", vec![vec![tool("f1", "web_fetch", json!({ "url": "https://example.invalid/docs" }))], vec![text("I couldn't read them.")]]),
        ],
        None,
    );
    let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Plan, approver.clone(), "base prompt".into());
    agent.turn(&mut session, "plan the docs update", &Cancel::default(), &mut |_| {}).await.unwrap();
    // in plan mode a fetch asks; a helper can't, so it's refused and nobody was asked
    assert!(approver.1.lock().unwrap().is_empty());
    let h1 = Session::open(&session.path().with_extension("agents").join("h1.jsonl")).await.unwrap();
    assert!(h1.messages()[2].content.iter().any(|b| matches!(b, Block::ToolResult { content, is_error: true, .. } if content.contains("a helper can't ask"))));
    // the helper's prompt has no plan-mode section (it's about the user's plan)
    assert!(model.requests("What do the docs at https://example.invalid/docs say?").iter().all(|(s, _)| !s.contains("# Plan mode")));
    assert!(model.requests("plan the docs update").iter().all(|(s, _)| s.contains("# Plan mode")));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stopping_stops_the_helpers_too() {
    let (dir, mut session) = project().await;
    let model = Router::new(
        vec![("look around", vec![vec![helper_call("h1", "One", "first question"), helper_call("h2", "Two", "second question")]])],
        // three never meet: the helpers wait until they're stopped
        Some(3),
    );
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let cancel = Cancel::default();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop.cancel();
    });
    let mut results = Vec::new();
    let res = agent
        .turn(&mut session, "look around", &cancel, &mut |e| {
            if let AgentEvent::ToolResult { id, content, .. } = e {
                results.push((id, content));
            }
        })
        .await;
    assert!(matches!(res, Err(Error::Cancelled)));
    results.sort();
    assert_eq!(results, [("h1".to_string(), "Stopped by the user.".to_string()), ("h2".to_string(), "Stopped by the user.".to_string())]);
    // the turn is recorded as stopped, with both results
    let resumed = Session::open(session.path()).await.unwrap();
    assert!(matches!(resumed.events().last(), Some(SessionEvent::Interrupted { .. })));
    assert_eq!(resumed.messages()[2].content.len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}
