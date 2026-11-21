//! The agent loop end to end with a scripted model: tool calls, approvals, cancellation, resume.
use async_trait::async_trait;
use devcat_harness::approval::{ApprovalRequest, DenyAll, Kind};
use devcat_harness::llm::{Delta, Reply, Request};
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Registry, Tool, ToolCtx, ToolOutput};
use devcat_harness::{Agent, AgentEvent, Approver, Block, Cancel, Decision, Error, Inbox, Model, Policy, Role, Session, SessionEvent, TurnEnd, Usage};
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

/// A tool that can't be undone: it runs only when the user types `target` (its `confirm`).
struct Drop(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Tool for Drop {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "drop".into(), description: "drop".into(), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        input["target"].as_str().unwrap_or("").to_string()
    }
    fn confirm(&self, input: &Value) -> Option<String> {
        input["target"].as_str().map(String::from)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        self.0.lock().unwrap().push(input["target"].as_str().unwrap_or("").to_string());
        ToolOutput::ok("dropped")
    }
}

#[tokio::test]
async fn a_call_that_cant_be_undone_runs_only_with_its_name_typed() {
    let run = |decision: Decision, policy: Policy| async move {
        let (dir, mut session) = project().await;
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let mut tools = Registry::default();
        tools.add(Drop(dropped.clone()));
        // twice: an "always allow" never covers the second one
        let model = Scripted::new(vec![vec![tool("a", "drop", json!({ "target": "shop" }))], vec![tool("b", "drop", json!({ "target": "shop" }))], vec![text("done")]]);
        let approver = Arc::new(Answer(decision, Mutex::default()));
        let mut agent = Agent::new(model, tools, policy, approver.clone(), "s".into());
        agent.turn(&mut session, "drop it", &Cancel::default(), &mut |_| {}).await.unwrap();
        let _ = std::fs::remove_dir_all(dir);
        let asked = approver.1.lock().unwrap().len();
        let n = dropped.lock().unwrap().len();
        (asked, n)
    };
    // even bypass asks, and a plain yes isn't enough
    assert_eq!(run(Decision::Allow, Policy::Bypass).await, (2, 0));
    assert_eq!(run(Decision::AllowForSession, Policy::Ask).await, (2, 0));
    assert_eq!(run(Decision::Confirm { text: "shopp".into() }, Policy::Ask).await, (2, 0));
    assert_eq!(run(Decision::Confirm { text: " shop ".into() }, Policy::Bypass).await, (2, 2));
    // plan mode doesn't run it at all
    assert_eq!(run(Decision::Confirm { text: "shop".into() }, Policy::Plan).await, (0, 0));
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
    // the summary, with what the user wrote kept word for word
    assert!(session.messages()[0].text().contains("word for word") && session.messages()[0].text().contains("1. hello"));
    assert!(session.context_tokens() < 60);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn compaction_carries_an_open_checklist_over() {
    let (dir, mut session) = project().await;
    let plan = json!({ "todos": [
        { "content": "Read the code", "status": "completed" },
        { "content": "Fix the bug", "status": "in_progress" },
        { "content": "Run the tests", "status": "pending" },
    ] });
    let done = json!({ "todos": [{ "content": "Read the code", "status": "completed" }] });
    let model = Scripted::new(vec![
        vec![tool("t1", "todo_write", plan)],
        vec![text("working")],
        vec![text("Summary: fixing the bug.")],
        vec![tool("t2", "todo_write", done)],
        vec![text("all done")],
        vec![text("Summary: finished.")],
    ]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "system".into());
    agent.turn(&mut session, "fix it", &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.unwrap();
    let summary = session.messages()[0].text();
    assert!(summary.contains("Summary: fixing the bug."));
    assert!(summary.contains("[x] Read the code
[>] Fix the bug
[ ] Run the tests"), "{summary}");
    // a finished list isn't carried over
    agent.turn(&mut session, "go on", &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.unwrap();
    assert!(!session.messages()[0].text().contains("checklist"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn in_the_sandbox_commands_still_ask() {
    for sandbox in [false, true] {
        let (dir, mut session) = project().await;
        let model = Scripted::new(vec![vec![tool("s1", "shell", json!({ "command": "echo hi" }))], vec![text("done")]]);
        let approver = Arc::new(Answer(Decision::Allow, Mutex::default()));
        let mut agent = Agent::new(model, Registry::standard(), Policy::AutoEdit, approver.clone(), "s".into());
        agent.sandbox = sandbox;
        let mut results = Vec::new();
        agent
            .turn(&mut session, "say hi", &Cancel::default(), &mut |e| {
                if let AgentEvent::ToolResult { content, is_error, .. } = e {
                    results.push((content, is_error));
                }
            })
            .await
            .unwrap();
        // the sandbox limits writes, not reads or the network: Auto-edit asks for commands either way
        assert_eq!(approver.1.lock().unwrap().len(), 1, "sandbox {sandbox}");
        match devcat_harness::sandbox::available() {
            Ok(()) => assert!(!results[0].1 && results[0].0.contains("hi"), "{results:?}"),
            // no sandbox on this system: the command isn't run unsandboxed
            Err(_) if sandbox => assert!(results[0].1 && results[0].0.starts_with("Not run: the sandbox is on"), "{results:?}"),
            Err(_) => assert!(!results[0].1, "{results:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }
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

#[tokio::test]
async fn the_step_limit_is_a_setting_and_zero_means_none() {
    // distinct calls, so the repeat guard stays out of it
    let glob = |i: usize| vec![tool(&format!("g{i}"), "glob", json!({ "pattern": format!("*{i}") }))];
    let (dir, mut session) = project().await;
    let mut agent = Agent::new(Scripted::new((0..3).map(glob).collect()), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.max_steps = 3;
    let end = agent.turn(&mut session, "look around", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!(end, TurnEnd::StepLimit);
    let _ = std::fs::remove_dir_all(dir);

    // 0: past any limit and on to the answer
    let (dir, mut session) = project().await;
    let mut steps: Vec<Vec<Block>> = (0..70).map(glob).collect();
    steps.push(vec![text("done")]);
    let mut agent = Agent::new(Scripted::new(steps), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.max_steps = 0;
    let end = agent.turn(&mut session, "look around", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
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
    // read's own output stops on a whole line and says where to go on; nothing is saved: its
    // file is there to page through
    assert!(results[1].contains("use offset") && !results[1].contains("is saved in") && results[1].len() < 31_000, "{}", &results[1][results[1].len() - 200..]);
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
        // the message itself, without context hooks added after it
        let first = match req.messages[0].content.first() {
            Some(Block::Text { text }) => text.clone(),
            _ => String::new(),
        };
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

#[tokio::test]
async fn prompt_hooks_are_for_the_users_messages_not_a_helpers_prompt() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    let settings = json!({ "hooks": { "UserPromptSubmit": [{ "hooks": [hook("echo 'from the hook'", "Write-Output 'from the hook'")] }] } });
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let model = Router::new(
        vec![
            ("look it up", vec![vec![helper_call("h1", "Look", "what is in notes.txt?")], vec![text("done")]]),
            ("what is in notes.txt?", vec![vec![text("version = 1")]]),
        ],
        None,
    );
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.hooks = Arc::new(devcat_harness::hooks::load(&proj, None).hooks);
    agent.turn(&mut session, "look it up", &Cancel::default(), &mut |_| {}).await.unwrap();
    let context = |s: &Session| s.events().iter().find_map(|e| if let SessionEvent::User { context, .. } = e { Some(context.clone()) } else { None }).unwrap();
    assert_eq!(context(&session), vec!["from the hook"]);
    let helper = Session::open(&session.path().with_extension("agents").join("h1.jsonl")).await.unwrap();
    assert!(context(&helper).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_hook_that_stops_the_agent_keeps_what_already_ran() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    let settings = json!({ "hooks": {
        "PostToolUse": [{ "matcher": "Read", "hooks": [hook(
            r#"echo '{"continue":false,"stopReason":"enough reading"}'"#,
            r#"Write-Output '{"continue":false,"stopReason":"enough reading"}'"#,
        )] }],
    }});
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let model = Scripted::new(vec![vec![tool("t1", "read", json!({ "path": "notes.txt" })), tool("t2", "glob", json!({ "pattern": "*" }))]]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.hooks = Arc::new(devcat_harness::hooks::load(&proj, None).hooks);
    let end = agent.turn(&mut session, "look", &Cancel::default(), &mut |_| {}).await;
    assert!(matches!(&end, Err(Error::Hook(m)) if m == "A PostToolUse hook stopped the agent: enough reading"), "{end:?}");
    // the read ran: the model gets its result; the glob after it didn't
    let results: Vec<(String, String)> = session.messages().iter().flat_map(|m| m.content.clone()).filter_map(|b| match b {
        Block::ToolResult { tool_use_id, content, .. } => Some((tool_use_id, content)),
        _ => None,
    }).collect();
    assert!(results[0].0 == "t1" && results[0].1.contains("version = 1"), "{results:?}");
    assert_eq!(results[1], ("t2".to_string(), "Not run: a hook stopped the agent.".to_string()));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stop_doesnt_wait_for_a_hook() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    let settings = json!({ "hooks": { "PreToolUse": [{ "hooks": [hook("sleep 30", "Start-Sleep -Seconds 30")] }] } });
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let model = Scripted::new(vec![vec![tool("t1", "read", json!({ "path": "notes.txt" }))]]);
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.hooks = Arc::new(devcat_harness::hooks::load(&proj, None).hooks);
    let cancel = Cancel::default();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        stopper.cancel();
    });
    let started = std::time::Instant::now();
    let end = agent.turn(&mut session, "look", &cancel, &mut |_| {}).await;
    assert!(matches!(end, Err(Error::Cancelled)), "{end:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "waited {:?} for the hook", started.elapsed());
    let _ = std::fs::remove_dir_all(dir);
}

/// Plays a script, and plays the user too: before step `n` returns, it sends `say[n]` (if any)
/// through the agent's inbox, as if typed while the agent worked. Records each request's last message.
struct Typist {
    steps: Mutex<Vec<Vec<Block>>>,
    say: Vec<Option<&'static str>>,
    inbox: Inbox,
    n: Mutex<usize>,
    last: Mutex<Vec<String>>,
}

#[async_trait]
impl Model for Typist {
    fn name(&self) -> String {
        "typist".into()
    }
    async fn step(&self, req: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        let n = {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            *n - 1
        };
        let last = req.messages.last().unwrap();
        self.last.lock().unwrap().push(last.content.iter().map(|b| match b {
            Block::Text { text } => text.clone(),
            Block::ToolResult { tool_use_id, .. } => format!("result {tool_use_id}"),
            _ => String::new(),
        }).collect::<Vec<_>>().join(" + "));
        if let Some(Some(s)) = self.say.get(n) {
            self.inbox.push(s.to_string(), vec![]);
        }
        let content = self.steps.lock().unwrap().remove(0);
        Ok(Reply { content, usage: Usage::default(), truncated: false })
    }
}

#[tokio::test]
async fn what_the_user_sends_while_it_works_goes_in_at_the_next_step() {
    let (dir, mut session) = project().await;
    let inbox = Inbox::default();
    let model = Arc::new(Typist {
        steps: Mutex::new(vec![
            vec![tool("t1", "read", json!({ "path": "notes.txt" }))],
            vec![text("Version 1.")],
            vec![text("And README is missing.")],
        ]),
        // during the read, and again while it answers
        say: vec![Some("also check the README"), Some("one more thing: be brief")],
        inbox: inbox.clone(),
        n: Mutex::new(0),
        last: Mutex::new(vec![]),
    });
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.inbox = inbox.clone();
    let mut said = Vec::new();
    let end = agent
        .turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| {
            if let AgentEvent::UserMessage { text, blocked: false } = e {
                said.push(text);
            }
        })
        .await
        .unwrap();
    assert_eq!(end, TurnEnd::Answered);
    // the first went in with the tool's result; the one sent while it answered kept it going
    assert_eq!(*model.last.lock().unwrap(), ["what's the version?", "result t1 + also check the README", "one more thing: be brief"]);
    assert_eq!(said, ["also check the README", "one more thing: be brief"]);
    assert!(inbox.is_empty());
    let users: Vec<&str> = session.events().iter().filter_map(|e| if let SessionEvent::User { text, .. } = e { Some(text.as_str()) } else { None }).collect();
    assert_eq!(users, ["what's the version?", "also check the README", "one more thing: be brief"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_message_sent_while_it_works_goes_past_the_prompt_hooks_too() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    let settings = json!({ "hooks": { "UserPromptSubmit": [{ "hooks": [hook(
        "if grep -q password; then echo 'no passwords in prompts' >&2; exit 2; fi; echo 'branch: main'",
        "$i = [Console]::In.ReadToEnd(); if ($i -match 'password') { [Console]::Error.WriteLine('no passwords in prompts'); exit 2 }; Write-Output 'branch: main'",
    )] }] } });
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let inbox = Inbox::default();
    let model = Arc::new(Typist {
        steps: Mutex::new(vec![
            vec![tool("t1", "read", json!({ "path": "notes.txt" }))],
            vec![tool("t2", "read", json!({ "path": "notes.txt" }))],
            vec![text("Version 1.")],
        ]),
        say: vec![Some("my password is hunter2"), Some("also check the README")],
        inbox: inbox.clone(),
        n: Mutex::new(0),
        last: Mutex::new(vec![]),
    });
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.hooks = Arc::new(devcat_harness::hooks::load(&proj, None).hooks);
    agent.inbox = inbox.clone();
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    // the hook refused the first one: the chat drops it and says why; the second went in
    let sent: Vec<(&str, bool)> = events.iter().filter_map(|e| if let AgentEvent::UserMessage { text, blocked } = e { Some((text.as_str(), *blocked)) } else { None }).collect();
    assert_eq!(sent, [("my password is hunter2", true), ("also check the README", false)]);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Hook { event, message } if event == "UserPromptSubmit" && message == "A hook blocked a message you sent: no passwords in prompts")));
    // the model never saw it, and the one it let through came with the hook's context
    assert!(!model.last.lock().unwrap().iter().any(|m| m.contains("hunter2")));
    let users: Vec<(&str, Vec<String>)> = session.events().iter().filter_map(|e| if let SessionEvent::User { text, context, .. } = e { Some((text.as_str(), context.clone())) } else { None }).collect();
    assert_eq!(users, [("what's the version?", vec!["branch: main".to_string()]), ("also check the README", vec!["branch: main".to_string()])]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A tool that shows the model a picture.
struct Picture;

#[async_trait]
impl Tool for Picture {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "picture".into(), description: "Show a picture.".into(), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "picture".into()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::image("cat.png (shown below)", "image/png", "AAAA".into())
    }
}

#[tokio::test]
async fn a_tools_image_goes_to_the_model_after_the_tool_results() {
    let (dir, mut session) = project().await;
    let model = Scripted::new(vec![vec![tool("p", "picture", json!({})), tool("r", "read", json!({ "path": "notes.txt" }))], vec![text("A cat.")]]);
    let mut tools = Registry::standard();
    tools.add(Picture);
    let mut agent = Agent::new(model, tools, Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.turn(&mut session, "what's in the picture?", &Cancel::default(), &mut |_| {}).await.unwrap();
    let msgs = session.messages();
    let results = &msgs[2];
    assert_eq!(results.role, Role::User);
    assert!(matches!(&results.content[0], Block::ToolResult { tool_use_id, content, .. } if tool_use_id == "p" && content.contains("cat.png")));
    assert!(matches!(&results.content[1], Block::ToolResult { tool_use_id, .. } if tool_use_id == "r"));
    assert_eq!(results.content[2], Block::Image { media_type: "image/png".into(), data: "AAAA".into() });
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------ long turns: limits, summaries, flaky providers

/// One scripted step with everything a provider can do to it.
struct Say {
    content: Vec<Block>,
    /// What the provider counted as input (0: it doesn't count).
    input: u64,
    truncated: bool,
    error: Option<String>,
}

fn say(content: Vec<Block>) -> Say {
    Say { content, input: 100, truncated: false, error: None }
}

fn fail(error: &str) -> Say {
    Say { content: vec![], input: 0, truncated: false, error: Some(error.into()) }
}

/// Plays [`Say`] steps and keeps every request's messages.
struct Script {
    steps: Mutex<std::collections::VecDeque<Say>>,
    requests: Mutex<Vec<Vec<devcat_harness::Message>>>,
    /// Refuses a request with more characters than this, the way a provider refuses one that
    /// doesn't fit its context window (without using up a scripted step).
    fits: Option<usize>,
}

impl Script {
    fn new(steps: Vec<Say>) -> Arc<Script> {
        Arc::new(Script { steps: Mutex::new(steps.into()), requests: Mutex::default(), fits: None })
    }
    fn tight(steps: Vec<Say>, fits: usize) -> Arc<Script> {
        Arc::new(Script { steps: Mutex::new(steps.into()), requests: Mutex::default(), fits: Some(fits) })
    }
    fn asked(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    /// Scripted steps the model wasn't asked for.
    fn left(&self) -> usize {
        self.steps.lock().unwrap().len()
    }
    /// The text of request `n`'s last message.
    fn last_text(&self, n: usize) -> String {
        self.requests.lock().unwrap()[n].last().unwrap().text()
    }
}

const TOO_LONG: &str = "400 This model's maximum context length is 32768 tokens. However, your messages resulted in 40211 tokens. Please reduce the length of the messages.";

fn chars(messages: &[devcat_harness::Message]) -> usize {
    messages.iter().flat_map(|m| &m.content).map(|b| match b {
        Block::Text { text } | Block::Thinking { text, .. } => text.len(),
        Block::ToolResult { content, .. } => content.len(),
        Block::ToolUse { input, .. } => input.to_string().len(),
        Block::Image { .. } => 6_000,
    }).sum()
}

#[async_trait]
impl Model for Script {
    fn name(&self) -> String {
        "script".into()
    }
    async fn step(&self, req: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        self.requests.lock().unwrap().push(req.messages.to_vec());
        if self.fits.is_some_and(|max| chars(req.messages) > max) {
            return Err(Error::Provider(TOO_LONG.into()));
        }
        let s = self.steps.lock().unwrap().pop_front().expect("the model was asked more steps than scripted");
        match s.error {
            Some(e) => Err(Error::Provider(e)),
            None => Ok(Reply { content: s.content, usage: Usage { input_tokens: s.input, output_tokens: if s.input > 0 { 10 } else { 0 }, cache_read_tokens: 0 }, truncated: s.truncated }),
        }
    }
}

fn agent_with(model: Arc<dyn Model>) -> Agent {
    Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "system".into())
}

fn notes(events: &[AgentEvent]) -> Vec<String> {
    events.iter().filter_map(|e| if let AgentEvent::Hook { event, message } = e { (event == "DevCat").then(|| message.clone()) } else { None }).collect()
}

fn compactions(events: &[AgentEvent]) -> usize {
    events.iter().filter(|e| matches!(e, AgentEvent::Compacted { .. })).count()
}

fn read_notes(id: &str) -> Say {
    say(vec![tool(id, "read", json!({ "path": "notes.txt" }))])
}

#[tokio::test]
async fn a_helper_out_of_steps_answers_with_what_it_found() {
    let (dir, mut session) = project().await;
    // 60 lookups (the helper's limit), then what it says when it's asked for its answer: the
    // call it still tries doesn't run
    let mut looking: Vec<Vec<Block>> = (0..60).map(|i| vec![tool(&format!("g{i}"), "glob", json!({ "pattern": format!("*{i}*") }))]).collect();
    looking.push(vec![text("The version is set in notes.txt (version = 1). I didn't get to the tests."), tool("late", "glob", json!({ "pattern": "*" }))]);
    let model = Router::new(
        vec![
            ("where is the version?", vec![vec![helper_call("h1", "Find the version", "Find where the version is set.")], vec![text("It's in notes.txt.")]]),
            ("Find where the version is set.", looking),
        ],
        None,
    );
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "where is the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    let result = events.iter().find_map(|e| match e {
        AgentEvent::ToolResult { id, content, is_error } if id == "h1" => Some((content.clone(), *is_error)),
        _ => None,
    }).unwrap();
    // its work comes back as an answer, marked as partial: not an error that sends the caller
    // to ask the same thing again
    assert!(!result.1, "{}", result.0);
    assert!(result.0.starts_with("The version is set in notes.txt (version = 1). I didn't get to the tests."), "{}", result.0);
    assert!(result.0.contains("ran out of steps (60)"), "{}", result.0);
    // what the helper was told at its limit is between the harness and the helper, not in the chat
    assert!(notes(&events).is_empty(), "{:?}", notes(&events));
    // 60 steps and the one that asked for the answer; it was told why, and its late call isn't kept
    assert_eq!(model.requests("Find where the version is set.").len(), 61);
    let helper = Session::open(&session.path().with_extension("agents").join("h1.jsonl")).await.unwrap();
    assert!(helper.events().iter().any(|e| matches!(e, SessionEvent::User { hook: Some(h), text, .. } if h == "DevCat" && text.contains("all 60 steps"))));
    assert!(!helper.events().iter().any(|e| matches!(e, SessionEvent::Assistant { content, .. } if content.iter().any(|b| matches!(b, Block::ToolUse { id, .. } if id == "late")))));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_helper_that_says_nothing_at_its_limit_still_hands_back_its_last_words() {
    let (dir, mut session) = project().await;
    let mut looking: Vec<Vec<Block>> = (0..60).map(|i| vec![text(if i == 59 { "So far: the version is in notes.txt." } else { "" }), tool(&format!("g{i}"), "glob", json!({ "pattern": format!("*{i}*") }))]).collect();
    // asked for its answer, it only calls a tool again
    looking.push(vec![tool("late", "glob", json!({ "pattern": "*" }))]);
    let model = Router::new(
        vec![
            ("where is the version?", vec![vec![helper_call("h1", "Find the version", "Find where the version is set.")], vec![text("Looking myself.")]]),
            ("Find where the version is set.", looking),
        ],
        None,
    );
    let mut agent = Agent::new(model, Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut results = Vec::new();
    agent.turn(&mut session, "where is the version?", &Cancel::default(), &mut |e| {
        if let AgentEvent::ToolResult { content, is_error, .. } = e {
            results.push((content, is_error));
        }
    }).await.unwrap();
    assert!(results[0].1 && results[0].0.contains("stopped after 60 steps") && results[0].0.contains("So far: the version is in notes.txt."), "{:?}", results[0]);
    assert!(results[0].0.contains("Ask a narrower question"), "{:?}", results[0]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_empty_summary_is_asked_for_again() {
    let (dir, mut session) = project().await;
    // 110 tokens a step in a 120-token window: summarized after the first step
    let model = Script::new(vec![
        read_notes("t1"),
        // asked for the summary, the model calls a tool instead (it still has them), then writes it
        say(vec![tool("oops", "read", json!({ "path": "notes.txt" }))]),
        say(vec![text("5. State now: notes.txt read, version = 1.")]),
        say(vec![text("The version is 1.")]),
    ]);
    let mut agent = agent_with(model.clone());
    agent.context_window = Some(120);
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(compactions(&events), 1);
    assert!(model.last_text(2).contains("Your last reply had no text"), "{}", model.last_text(2));
    // the tokens of both tries count
    assert!(session.events().iter().any(|e| matches!(e, SessionEvent::Compacted { usage, .. } if usage.input_tokens == 200)));
    assert_eq!(session.messages().last().unwrap().text(), "The version is 1.");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_summary_that_fails_doesnt_end_the_turn() {
    let (dir, mut session) = project().await;
    let model = Script::new(vec![
        read_notes("t1"),
        // no summary, twice: the turn goes on without one
        say(vec![]),
        say(vec![]),
        read_notes("t2"),
        // not tried again at the very next step
        say(vec![text("The version is 1.")]),
    ]);
    let mut agent = agent_with(model.clone());
    agent.context_window = Some(120);
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(compactions(&events), 0);
    // the chat hears why, once
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::CompactFailed { message } if message.contains("empty summary"))).count(), 1);
    // the conversation is whole: nothing was replaced
    assert!(session.messages()[0].text().starts_with("what's the version?"));
    assert_eq!(model.asked(), 5);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_small_window_isnt_summarized_at_every_step() {
    let (dir, mut session) = project().await;
    let heavy = |id: &str, input: u64| Say { input, ..read_notes(id) };
    // A 1000-token window (summarized above 800). What stays after a summary (the system
    // prompt, the tools, the summary itself) is over that on its own here.
    let model = Script::new(vec![
        heavy("t1", 850),
        say(vec![text("5. State now: reading notes.txt.")]),
        heavy("t2", 820),
        heavy("t3", 830),
        heavy("t4", 840),
        Say { input: 850, ..say(vec![text("The version is 1.")]) },
    ]);
    let mut agent = agent_with(model.clone());
    agent.context_window = Some(1000);
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    // once: again after every step would free nothing and make the model forget each step
    assert_eq!((compactions(&events), model.left()), (1, 0));
    // ...until the conversation grew enough since then (a tenth of the window)
    let model = Script::new(vec![
        heavy("t1", 850),
        say(vec![text("5. State now: reading notes.txt.")]),
        heavy("t2", 820),
        heavy("t3", 930),
        say(vec![text("5. State now: still reading.")]),
        say(vec![text("The version is 1.")]),
    ]);
    let (dir2, mut session) = project().await;
    let mut agent = agent_with(model.clone());
    agent.context_window = Some(1000);
    let mut events = Vec::new();
    agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!((compactions(&events), model.left()), (2, 0));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[tokio::test]
async fn a_provider_that_doesnt_count_tokens_still_gets_its_conversation_summarized() {
    let (dir, mut session) = project().await;
    std::fs::write(dir.join("proj/long.txt"), "a long line in the file\n".repeat(300)).unwrap();
    let quiet = |content: Vec<Block>| Say { input: 0, ..say(content) };
    let model = Script::new(vec![
        quiet(vec![tool("r1", "read", json!({ "path": "long.txt" }))]),
        quiet(vec![text("5. State now: long.txt read.")]),
        quiet(vec![text("It's long.")]),
    ]);
    let mut agent = agent_with(model.clone());
    // about 2000 tokens of file in a 1000-token window: without a count from the provider, the
    // characters say so
    agent.context_window = Some(1000);
    let mut events = Vec::new();
    agent.turn(&mut session, "how long is long.txt?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!((compactions(&events), model.left()), (1, 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_conversation_the_provider_refuses_as_too_long_is_summarized_and_the_step_taken_again() {
    let (dir, mut session) = project().await;
    let model = Script::new(vec![
        read_notes("t1"),
        // the window isn't known (or was guessed too large): the provider says it doesn't fit
        fail(TOO_LONG),
        say(vec![text("5. State now: notes.txt read, version = 1.")]),
        say(vec![text("The version is 1.")]),
    ]);
    let mut agent = agent_with(model.clone());
    agent.max_steps = 2;
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    // (the refused step didn't count against the step limit)
    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(compactions(&events), 1);
    let summary = session.messages()[0].text();
    assert!(summary.contains("State now") && summary.contains("1. what's the version?") && summary.contains("go on now with what is still open"), "{summary}");
    assert_eq!(session.messages().last().unwrap().text(), "The version is 1.");
    let _ = std::fs::remove_dir_all(dir);

    // still too long after the summary: summarizing again can't help, and the error says why
    let (dir, mut session) = project().await;
    let model = Script::new(vec![read_notes("t1"), fail(TOO_LONG), say(vec![text("Summary.")]), fail(TOO_LONG)]);
    let mut agent = agent_with(model.clone());
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |_| {}).await;
    assert!(matches!(&end, Err(Error::Provider(m)) if m.contains("maximum context length") && m.contains("still doesn't fit")), "{end:?}");
    assert_eq!(model.asked(), 4);
    // a first message that's too long by itself has nothing to summarize: the provider's word stands
    let model = Script::new(vec![fail(TOO_LONG)]);
    let (dir2, mut session) = project().await;
    let end = agent_with(model.clone()).turn(&mut session, "a very long message", &Cancel::default(), &mut |_| {}).await;
    assert!(matches!(&end, Err(Error::Provider(m)) if m == TOO_LONG), "{end:?}");
    assert_eq!(model.asked(), 1);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[tokio::test]
async fn a_summary_as_a_turn_starts_keeps_the_message_that_started_it() {
    let (dir, mut session) = project().await;
    // the first turn ends over the limit of a 100,000-token window: the next one starts with a summary
    let model = Script::new(vec![Say { input: 85_000, ..say(vec![text("The version is 1.")]) }, say(vec![text("5. State now: told the user the version.")]), say(vec![text("A red build badge.")])]);
    let mut agent = agent_with(model.clone());
    agent.context_window = Some(100_000);
    agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |_| {}).await.unwrap();
    let image = Block::Image { media_type: "image/png".into(), data: "AAAA".into() };
    let mut events = Vec::new();
    agent.turn_with_images(&mut session, "and what does this screenshot show?", vec![image.clone()], &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!((compactions(&events), model.left()), (1, 0));
    // the model answers the message itself, picture included: not a paraphrase of it
    let asked = model.requests.lock().unwrap()[2].clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].content.contains(&Block::Text { text: "and what does this screenshot show?".into() }) && asked[0].content.contains(&image), "{:?}", asked[0].content);
    assert!(matches!(&asked[0].content[0], Block::Text { text } if text.contains("State now")));
    // and a chat opened again says the same
    let again = Session::open(session.path()).await.unwrap();
    assert!(again.messages()[0].content.contains(&image));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_kept_message_that_doesnt_fit_is_summarized_too_before_giving_up() {
    let (dir, mut session) = project().await;
    // the provider takes 8,000 characters; the second turn's message has 9,000
    let model = Script::tight(
        vec![read_notes("t1"), say(vec![text("The version is 1.")]), say(vec![text("S1: told the user the version.")]), say(vec![text("S2: the user pasted a long log.")]), say(vec![text("The log shows a timeout.")])],
        8_000,
    );
    let mut agent = agent_with(model.clone());
    agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |_| {}).await.unwrap();
    let mut events = Vec::new();
    let end = agent.turn(&mut session, &format!("what's wrong here? {}", "log line\n".repeat(1_000)), &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    // summarized keeping the message (still too long), then with the message in the summary
    assert_eq!((end, compactions(&events), model.left()), (TurnEnd::Answered, 2, 0));
    let first = session.messages()[0].text();
    assert!(first.contains("S2: the user pasted a long log.") && first.contains("what's wrong here? log line") && first.contains("[…]"), "{}", &first[..300]);
    assert_eq!(session.messages().last().unwrap().text(), "The log shows a timeout.");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_message_refused_by_a_hook_after_the_answer_doesnt_ask_the_model_again() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    let settings = json!({ "hooks": { "UserPromptSubmit": [{ "hooks": [hook(
        "if grep -q password; then echo 'no passwords in prompts' >&2; exit 2; fi",
        "$i = [Console]::In.ReadToEnd(); if ($i -match 'password') { [Console]::Error.WriteLine('no passwords in prompts'); exit 2 }",
    )] }] } });
    std::fs::write(proj.join(".claude/settings.json"), settings.to_string()).unwrap();
    let inbox = Inbox::default();
    // sent while the model writes its answer
    let model = Arc::new(Typist { steps: Mutex::new(vec![vec![text("Version 1.")]]), say: vec![Some("my password is hunter2")], inbox: inbox.clone(), n: Mutex::new(0), last: Mutex::new(vec![]) });
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Ask, Arc::new(DenyAll), "s".into());
    agent.hooks = Arc::new(devcat_harness::hooks::load(&proj, None).hooks);
    agent.inbox = inbox;
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    // its answer stands: a request that ends with the model's own message isn't a question
    assert_eq!((end, *model.n.lock().unwrap()), (TurnEnd::Answered, 1));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::UserMessage { blocked: true, .. })));
    assert_eq!(session.messages().last().unwrap().text(), "Version 1.");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_window_too_small_for_the_work_ends_the_turn_instead_of_summarizing_forever() {
    let (dir, mut session) = project().await;
    let look = |i: usize| say(vec![tool(&format!("g{i}"), "glob", json!({ "pattern": format!("*{i}") }))]);
    let summary = || say(vec![text("5. State now: looking around.")]);
    // every step after a summary is one too many for the window
    let model = Script::new(vec![look(1), fail(TOO_LONG), summary(), look(2), fail(TOO_LONG), summary(), look(3), fail(TOO_LONG), summary(), look(4), fail(TOO_LONG)]);
    let mut events = Vec::new();
    let end = agent_with(model.clone()).turn(&mut session, "look around", &Cancel::default(), &mut |e| events.push(e)).await;
    // three summaries with a step between them, then it says what would help
    assert!(matches!(&end, Err(Error::Provider(m)) if m.contains("maximum context length") && m.contains("Use a model with a larger window")), "{end:?}");
    assert_eq!((compactions(&events), model.left()), (3, 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_conversation_too_long_to_summarize_is_cut_down_until_it_fits() {
    let (dir, mut session) = project().await;
    // the provider takes 12,000 characters; `big` returns 30,000
    let model = Script::tight(vec![say(vec![tool("b1", "big", json!({}))]), say(vec![text("5. State now: ran big, it printed lines.")]), say(vec![text("It printed a lot.")])], 12_000);
    let mut tools = Registry::standard();
    tools.add(Big);
    let mut agent = Agent::new(model.clone(), tools, Policy::Ask, Arc::new(DenyAll), "system".into());
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "run big", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(compactions(&events), 1);
    // the step, refused; the summary request, refused whole and taken with the output clipped;
    // then the step again
    let requests = model.requests.lock().unwrap();
    let sizes: Vec<usize> = requests.iter().map(|r| chars(r)).collect();
    assert_eq!(requests.len(), 5, "{sizes:?}");
    assert!(sizes[1] > 12_000 && sizes[2] > 12_000 && sizes[3] <= 12_000 && sizes[3] > 2_000, "{sizes:?}");
    // clipped, not dropped: the call still has its result, and its start and end are there
    let clipped = requests[3].iter().flat_map(|m| &m.content).find_map(|b| if let Block::ToolResult { tool_use_id, content, .. } = b { (tool_use_id == "b1").then(|| content.clone()) } else { None }).unwrap();
    assert!(clipped.starts_with("START") && clipped.contains("characters left out"), "{}", &clipped[..200]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_answer_cut_off_at_the_output_limit_goes_on() {
    let (dir, mut session) = project().await;
    let model = Script::new(vec![Say { truncated: true, ..say(vec![text("The first half")]) }, say(vec![text(" and the second.")])]);
    let mut agent = agent_with(model.clone());
    let mut events = Vec::new();
    let end = agent.turn(&mut session, "explain it", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert_eq!(model.asked(), 2);
    assert!(model.last_text(1).contains("cut off at the output limit"));
    assert_eq!(notes(&events).len(), 1);
    // told by the harness, not typed by the user: the chat's title and message count don't change
    assert!(session.events().iter().any(|e| matches!(e, SessionEvent::User { hook: Some(h), .. } if h == "DevCat")));
    assert_eq!(session.title(), "explain it");
    let _ = std::fs::remove_dir_all(dir);

    // a model that never gets to the end is stopped asking after three times
    let (dir, mut session) = project().await;
    let model = Script::new((0..4).map(|_| Say { truncated: true, ..say(vec![text("more")]) }).collect());
    let end = agent_with(model.clone()).turn(&mut session, "explain it", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!((end, model.asked()), (TurnEnd::Answered, 4));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_call_cut_off_at_the_output_limit_isnt_run_and_its_text_isnt_kept() {
    let (dir, mut session) = project().await;
    let raw = format!("{{\"path\": \"big.txt\", \"content\": \"{}", "x".repeat(20_000));
    let model = Script::new(vec![Say { truncated: true, ..say(vec![tool("w1", "write", json!({ "_unparsed_arguments": raw }))]) }, say(vec![text("I'll write it in parts.")])]);
    let mut agent = agent_with(model.clone());
    agent.policy = Policy::Bypass;
    let mut results = Vec::new();
    agent.turn(&mut session, "write big.txt", &Cancel::default(), &mut |e| {
        if let AgentEvent::ToolResult { content, is_error, .. } = e {
            results.push((content, is_error));
        }
    }).await.unwrap();
    assert!(results[0].1 && results[0].0.contains("hit the output limit") && results[0].0.contains("in parts"), "{:?}", results[0]);
    assert!(!dir.join("proj/big.txt").exists());
    // the 20,000 characters don't go back to the model with every later step
    let sent = chars(&model.requests.lock().unwrap()[1]);
    assert!(sent < 2_000, "{sent}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_empty_answer_is_asked_about_once() {
    let (dir, mut session) = project().await;
    let thinking = || Block::Thinking { text: "hm".into(), signature: None };
    let model = Script::new(vec![read_notes("t1"), say(vec![thinking()]), say(vec![text("The version is 1.")])]);
    let mut events = Vec::new();
    let end = agent_with(model.clone()).turn(&mut session, "what's the version?", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert!(model.last_text(2).contains("Your last message was empty"));
    assert_eq!(session.messages().last().unwrap().text(), "The version is 1.");
    // the empty step isn't sent back (providers refuse an empty message), so the roles still alternate
    for request in model.requests.lock().unwrap().iter() {
        assert!(request.iter().all(|m| !m.content.is_empty()));
        assert!(request.windows(2).all(|w| w[0].role != w[1].role));
    }
    let _ = std::fs::remove_dir_all(dir);

    // empty again: the turn ends, as it did before
    let (dir, mut session) = project().await;
    let model = Script::new(vec![say(vec![]), say(vec![])]);
    let end = agent_with(model.clone()).turn(&mut session, "hello?", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!((end, model.asked()), (TurnEnd::Answered, 2));
    // and the session stays usable
    let model = Script::new(vec![say(vec![text("Hi.")])]);
    agent_with(model.clone()).turn(&mut session, "hello again", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert!(model.requests.lock().unwrap()[0].iter().all(|m| !m.content.is_empty()));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_turn_that_didnt_finish_is_told_to_the_model_with_the_next_message() {
    let (dir, mut session) = project().await;
    let glob = |i: usize| say(vec![tool(&format!("g{i}"), "glob", json!({ "pattern": format!("*{i}") }))]);
    let model = Script::new(vec![glob(0), glob(1), say(vec![text("Done.")]), say(vec![text("Hello.")])]);
    let mut agent = agent_with(model.clone());
    agent.max_steps = 2;
    assert_eq!(agent.turn(&mut session, "look around", &Cancel::default(), &mut |_| {}).await.unwrap(), TurnEnd::StepLimit);
    agent.turn(&mut session, "continue", &Cancel::default(), &mut |_| {}).await.unwrap();
    // "continue" alone, after work that just ends, reads like a new task: the model is told
    // that the turn was cut short and that its work stands
    let last = model.last_text(2);
    assert!(last.contains("ended before you answered: Stopped after 2 steps") && last.contains("instead of starting over") && last.ends_with("continue"), "{last}");
    // only the message right after it says so
    agent.turn(&mut session, "thanks", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert_eq!(model.last_text(3), "thanks");
    let _ = std::fs::remove_dir_all(dir);
}

/// A tool with a bug.
struct Crash;

#[async_trait]
impl Tool for Crash {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "crash".into(), description: "Crashes.".into(), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "crash".into()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        panic!("the row had no name")
    }
}

#[tokio::test]
async fn a_tool_that_crashes_fails_its_call_not_the_turn() {
    let (dir, mut session) = project().await;
    let model = Script::new(vec![say(vec![tool("c1", "crash", json!({})), tool("r1", "read", json!({ "path": "notes.txt" }))]), say(vec![text("The tool is broken; the version is 1.")])]);
    let mut tools = Registry::standard();
    tools.add(Crash);
    let mut agent = Agent::new(model, tools, Policy::Ask, Arc::new(DenyAll), "s".into());
    let mut results = Vec::new();
    let end = agent.turn(&mut session, "try it", &Cancel::default(), &mut |e| {
        if let AgentEvent::ToolResult { content, is_error, .. } = e {
            results.push((content, is_error));
        }
    }).await.unwrap();
    assert_eq!(end, TurnEnd::Answered);
    assert!(results[0].1 && results[0].0.contains("`crash` tool crashed (the row had no name)"), "{:?}", results[0]);
    // the call after it ran
    assert!(!results[1].1 && results[1].0.contains("version = 1"), "{:?}", results[1]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn compaction_keeps_what_the_user_wrote_word_for_word() {
    let (dir, mut session) = project().await;
    let model = Script::new(vec![
        say(vec![text("OK.")]),
        say(vec![text("Summary one.")]),
        say(vec![text("Done.")]),
        say(vec![text("Summary two.")]),
    ]);
    let mut agent = agent_with(model);
    agent.turn(&mut session, "Rename the flag --dry to --dry-run, but keep --dry working.", &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.turn(&mut session, &format!("Also update the docs. {}", "x".repeat(3_000)), &Cancel::default(), &mut |_| {}).await.unwrap();
    agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.unwrap();
    let summary = session.messages()[0].text();
    // the second summary still has the first request as it was said (the log keeps it), and
    // a long message is cut, not dropped
    assert!(summary.contains("1. Rename the flag --dry to --dry-run, but keep --dry working."), "{summary}");
    assert!(summary.contains("2. Also update the docs. xxx") && summary.contains("[…]"), "{}", &summary[..400]);
    assert!(summary.len() < 3_000, "{}", summary.len());
    // a summary asked for between turns doesn't tell the model to carry on by itself
    assert!(!summary.contains("go on now with what is still open"));
    let _ = std::fs::remove_dir_all(dir);
}

/// A tool that only looks and takes `ms` to do it; `order` records when each call starts and ends.
struct Slow(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "slow_look".into(), description: "looks, slowly".into(), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input["n"].to_string()
    }
    fn concurrent(&self, _: &Value, _: &ToolCtx) -> bool {
        true
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let n = input["n"].as_u64().unwrap();
        self.0.lock().unwrap().push(format!("start {n}"));
        // the later calls finish first: the results must still come back in call order
        tokio::time::sleep(Duration::from_millis(300 - n * 50)).await;
        self.0.lock().unwrap().push(format!("end {n}"));
        ToolOutput::ok(format!("looked {n}"))
    }
}

#[tokio::test]
async fn calls_that_only_look_run_side_by_side_and_answer_in_order() {
    let (dir, mut session) = project().await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let model = Scripted::new(vec![
        vec![
            tool("a", "slow_look", json!({ "n": 1 })),
            tool("b", "slow_look", json!({ "n": 2 })),
            tool("c", "slow_look", json!({ "n": 3 })),
            // not one that only looks: it runs on its own, after them
            tool("d", "write", json!({ "path": "out.txt", "content": "x" })),
            tool("e", "read", json!({ "path": "out.txt" })),
        ],
        vec![text("Done.")],
    ]);
    let mut tools = Registry::standard();
    tools.add(Slow(order.clone()));
    let mut agent = Agent::new(model, tools, Policy::Bypass, Arc::new(DenyAll), "system".into());
    let started = std::time::Instant::now();
    let mut results = Vec::new();
    agent.turn(&mut session, "look", &Cancel::default(), &mut |e| if let AgentEvent::ToolResult { id, .. } = e { results.push(id) }).await.unwrap();
    // all three started before any ended, and it took about the longest, not the sum (750 ms)
    let o = order.lock().unwrap().clone();
    assert_eq!(&o[..3], ["start 1", "start 2", "start 3"], "{o:?}");
    assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
    assert_eq!(results, ["a", "b", "c", "d", "e"]);
    // the read after the write saw what it wrote
    let SessionEvent::ToolResults { results, took_ms, .. } = session.events().iter().rev().find(|e| matches!(e, SessionEvent::ToolResults { .. })).unwrap() else { unreachable!() };
    let texts: Vec<&str> = results.iter().map(|b| match b { Block::ToolResult { content, .. } => content.as_str(), _ => "" }).collect();
    assert!(texts[0] == "looked 1" && texts[2] == "looked 3" && texts[4].contains("\tx"), "{texts:?}");
    assert_eq!(took_ms.len(), 5);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stopping_stops_calls_that_run_side_by_side() {
    let (dir, mut session) = project().await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let model = Scripted::new(vec![vec![tool("a", "slow_look", json!({ "n": 1 })), tool("b", "slow_look", json!({ "n": 2 }))], vec![text("Done.")]]);
    let mut tools = Registry::standard();
    tools.add(Slow(order.clone()));
    let cancel = Cancel::default();
    let mut agent = Agent::new(model, tools, Policy::Bypass, Arc::new(DenyAll), "system".into());
    let c = cancel.clone();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let res = agent.turn(&mut session, "look", &cancel, &mut |_| {}).await;
    stop.await.unwrap();
    assert!(matches!(res, Err(Error::Cancelled)));
    let SessionEvent::ToolResults { results, .. } = session.events().iter().rev().find(|e| matches!(e, SessionEvent::ToolResults { .. })).unwrap() else { unreachable!() };
    assert!(results.iter().all(|b| matches!(b, Block::ToolResult { is_error: true, content, .. } if content.contains("Stopped by the user"))), "{results:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_summary_keeps_the_newest_steps_and_lists_the_files_and_the_next_one_updates_it() {
    let (dir, mut session) = project().await;
    // a 10,000-token window: the second step's 9,000 tokens make it summarize before the third
    let model = Script::new(vec![
        say(vec![tool("r1", "read", json!({ "path": "notes.txt" }))]),
        Say { input: 9_000, ..say(vec![tool("w1", "write", json!({ "path": "new.txt", "content": "hello" }))]) },
        say(vec![text("1. Request: the task.")]),
        say(vec![text("Done.")]),
        say(vec![text("1. Request: the task, done.")]),
    ]);
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Bypass, Arc::new(DenyAll), "system".into());
    agent.context_window = Some(10_000);
    let mut events = Vec::new();
    agent.turn(&mut session, "the task", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    assert_eq!(compactions(&events), 1);
    // the step after the summary sees the summary, then the newest step as it was: the write and its result
    let after = model.requests.lock().unwrap()[3].clone();
    assert_eq!(after.len(), 3, "{after:?}");
    let summary = after[0].text();
    assert!(summary.contains("1. Request: the task.") && summary.contains("Changed: new.txt") && summary.contains("Read: notes.txt"), "{summary}");
    assert!(after[1].tool_uses().any(|(id, name, _)| id == "w1" && name == "write"));
    assert!(matches!(&after[2].content[0], Block::ToolResult { tool_use_id, is_error: false, .. } if tool_use_id == "w1"));
    // the one before it, and the first message, are only in the summary
    assert!(!after.iter().any(|m| m.tool_uses().any(|(id, _, _)| id == "r1")));
    // summarized again: asked to update the summary it starts with
    agent.compact(&mut session, None, &Cancel::default(), &mut |_| {}).await.unwrap();
    assert!(model.last_text(4).contains("update of it"), "{}", model.last_text(4));
    assert!(!model.last_text(2).contains("update of it"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_tail_too_big_for_the_window_is_summarized_too() {
    let (dir, mut session) = project().await;
    // the provider takes 4,000 characters at most: the newest step alone is more
    let model = Script::tight(vec![
        say(vec![tool("r1", "read", json!({ "path": "notes.txt" }))]),
        say(vec![tool("g1", "glob", json!({ "pattern": "*" }))]),
        say(vec![tool("w1", "write", json!({ "path": "big.txt", "content": "x".repeat(5_000) }))]),
        say(vec![text("Summary one.")]),
        say(vec![text("Summary two.")]),
        say(vec![text("Done.")]),
    ], 4_000);
    let mut agent = Agent::new(model.clone(), Registry::standard(), Policy::Bypass, Arc::new(DenyAll), "system".into());
    agent.context_window = Some(1_000_000);
    let mut events = Vec::new();
    agent.turn(&mut session, "the task", &Cancel::default(), &mut |e| events.push(e)).await.unwrap();
    // refused as too long: summarized keeping the newest step, refused again, summarized without it
    assert_eq!(compactions(&events), 2, "{events:?}");
    let tails: Vec<usize> = session.events().iter().filter_map(|e| if let SessionEvent::Compacted { tail, .. } = e { Some(*tail) } else { None }).collect();
    // (the glob and the write with their results: all after the first step)
    assert_eq!(tails, [4, 0]);
    assert_eq!(model.left(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_slash_command_goes_to_the_model_and_the_log_keeps_what_was_typed() {
    let (dir, mut session) = project().await;
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join(".claude/commands")).unwrap();
    std::fs::write(proj.join(".claude/commands/bump.md"), "Bump the version in $1 by one.").unwrap();
    let (_, ask) = devcat_harness::commands::expand("/bump notes.txt", &proj, None).unwrap();
    let model = Script::new(vec![say(vec![text("Bumped.")])]);
    let mut agent = agent_with(model.clone());
    agent.turn_with(&mut session, "/bump notes.txt", vec![], vec![ask], &Cancel::default(), &mut |_| {}).await.unwrap();
    let sent = model.requests.lock().unwrap()[0][0].text();
    assert!(sent.starts_with("/bump notes.txt") && sent.contains("Bump the version in notes.txt by one."), "{sent}");
    assert_eq!(session.title(), "/bump notes.txt");
    let _ = std::fs::remove_dir_all(dir);
}

/// An MCP server's tool with a long definition: enough of them are left out of the requests.
struct Heavy(&'static str);

#[async_trait]
impl Tool for Heavy {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.0.into(), description: format!("{} {}", self.0.replace('_', " "), "x".repeat(40_000)), input_schema: json!({ "type": "object" }) }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        String::new()
    }
    fn deferrable(&self) -> bool {
        true
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::ok(format!("{} ran", self.0))
    }
}

/// Records the tool names of every request.
struct Tools(Mutex<Vec<Vec<String>>>, Mutex<Vec<Vec<Block>>>);

#[async_trait]
impl Model for Tools {
    fn name(&self) -> String {
        "tools".into()
    }
    async fn step(&self, req: &Request<'_>, _: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        self.0.lock().unwrap().push(req.tools.iter().map(|t| t.name.clone()).collect());
        Ok(Reply { content: self.1.lock().unwrap().remove(0), usage: Usage::default(), truncated: false })
    }
}

#[tokio::test]
async fn many_mcp_tools_are_found_with_tool_search_and_stay_once_found() {
    let (dir, mut session) = project().await;
    let model = Arc::new(Tools(Mutex::default(), Mutex::new(vec![
        vec![tool("s1", "tool_search", json!({ "query": "open an issue" }))],
        vec![tool("c1", "mcp__gh__open_issue", json!({}))],
        vec![text("Opened.")],
        vec![text("Still here.")],
    ])));
    let mut tools = Registry::standard();
    tools.add(Heavy("mcp__gh__open_issue"));
    tools.add(Heavy("mcp__gh__merge_pull"));
    let mut agent = Agent::new(model.clone(), tools, Policy::Bypass, Arc::new(DenyAll), "system".into());
    let mut results = Vec::new();
    agent.turn(&mut session, "open an issue", &Cancel::default(), &mut |e| if let AgentEvent::ToolResult { content, .. } = e { results.push(content) }).await.unwrap();
    assert!(results[0].starts_with("Tools now available: mcp__gh__open_issue"), "{}", results[0]);
    assert_eq!(results[1], "mcp__gh__open_issue ran");
    let seen = model.0.lock().unwrap().clone();
    let has = |n: usize, t: &str| seen[n].iter().any(|x| x == t);
    // first only the search, then the tool it found (never the other one)
    assert!(has(0, "tool_search") && !has(0, "mcp__gh__open_issue"));
    assert!(has(1, "mcp__gh__open_issue") && has(2, "mcp__gh__open_issue") && !seen.iter().flatten().any(|t| t == "mcp__gh__merge_pull"));
    // a chat opened again still has it
    let mut again = Session::open(session.path()).await.unwrap();
    agent.turn(&mut again, "and now?", &Cancel::default(), &mut |_| {}).await.unwrap();
    assert!(model.0.lock().unwrap().last().unwrap().iter().any(|t| t == "mcp__gh__open_issue"));
    let _ = std::fs::remove_dir_all(dir);
}
