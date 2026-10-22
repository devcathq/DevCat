//! The agent loop end to end with a scripted model: tool calls, approvals, cancellation, resume.
use async_trait::async_trait;
use devcat_harness::approval::{ApprovalRequest, DenyAll};
use devcat_harness::llm::{Delta, Reply, Request};
use devcat_harness::tools::Registry;
use devcat_harness::{Agent, AgentEvent, Approver, Block, Cancel, Decision, Error, Model, Policy, Role, Session, SessionEvent, TurnEnd, Usage};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Answers each step from a script and records what it was sent.
struct Scripted {
    steps: Mutex<Vec<Vec<Block>>>,
    seen: Mutex<Vec<usize>>, // messages per request
}

impl Scripted {
    fn new(steps: Vec<Vec<Block>>) -> Arc<Scripted> {
        Arc::new(Scripted { steps: Mutex::new(steps.into_iter().rev().collect()), seen: Mutex::default() })
    }
}

#[async_trait]
impl Model for Scripted {
    fn name(&self) -> String {
        "scripted".into()
    }
    async fn step(&self, req: &Request<'_>, on: &mut (dyn FnMut(Delta) + Send)) -> devcat_harness::Result<Reply> {
        self.seen.lock().unwrap().push(req.messages.len());
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
