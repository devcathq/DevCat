//! Try the harness from a terminal (development only):
//!
//! ```sh
//! cargo run --example chat -- --base http://127.0.0.1:11434/v1 --model qwen3:8b --cwd /some/project "list the files"
//! ```
//! Options: `--protocol openai-chat|anthropic` (default openai-chat), `--key <api key>` (or
//! `DEVCAT_API_KEY`), `--policy ask|auto-edit|bypass` (default ask: you answer y/n here),
//! `--resume <session.jsonl>`. Sessions go to `<cwd>/.devcat-sessions/`.
use async_trait::async_trait;
use devcat_harness::approval::ApprovalRequest;
use devcat_harness::prompt::{Environment, system_prompt};
use devcat_harness::tools::Registry;
use devcat_harness::{Agent, AgentEvent, Approver, Cancel, Decision, Llm, LlmConfig, Policy, Protocol, Session};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

struct Terminal;

#[async_trait]
impl Approver for Terminal {
    async fn decide(&self, req: &ApprovalRequest) -> (Decision, String) {
        let q = format!("\n? allow {} `{}`? [y]es / [a]lways / [n]o: ", req.tool, req.summary);
        let answer = tokio::task::spawn_blocking(move || {
            print!("{q}");
            let _ = std::io::stdout().flush();
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            s.trim().to_lowercase()
        })
        .await
        .unwrap_or_default();
        let d = match answer.as_str() {
            "y" | "yes" => Decision::Allow,
            "a" | "always" => Decision::AllowForSession,
            _ => Decision::Deny { reason: None },
        };
        (d, "terminal".into())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (mut base, mut model, mut key, mut cwd, mut resume) = (String::new(), String::new(), std::env::var("DEVCAT_API_KEY").ok(), std::env::current_dir().unwrap(), None);
    let (mut protocol, mut policy, mut prompt) = (Protocol::OpenaiChat, Policy::Ask, Vec::new());
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("missing value");
        match a.as_str() {
            "--base" => base = val(),
            "--model" => model = val(),
            "--key" => key = Some(val()),
            "--cwd" => cwd = PathBuf::from(val()),
            "--resume" => resume = Some(PathBuf::from(val())),
            "--protocol" => protocol = serde_json::from_value(serde_json::Value::String(val())).expect("openai-chat or anthropic"),
            "--policy" => policy = serde_json::from_value(serde_json::Value::String(val())).expect("ask, auto-edit or bypass"),
            _ => prompt.push(a),
        }
    }
    let llm = Llm::new(LlmConfig { protocol, base_url: base, api_key: key, model, max_tokens: 8192 });
    let mut session = match resume {
        Some(p) => Session::open(&p).await.expect("open session"),
        None => Session::create(&cwd.join(".devcat-sessions"), &cwd.to_string_lossy()).await.expect("create session"),
    };
    let date = format!("{:?}", std::time::SystemTime::now());
    let system = system_prompt(&Environment { cwd: &cwd, os: std::env::consts::OS, shell: if cfg!(windows) { "PowerShell" } else { "bash" }, date: &date });
    let mut agent = Agent::new(Arc::new(llm), Registry::standard(), policy, Arc::new(Terminal), system);
    println!("session {}", session.path().display());
    let end = agent
        .turn(&mut session, &prompt.join(" "), &Cancel::default(), &mut |e| match e {
            AgentEvent::Text { text } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::Thinking { .. } => {}
            AgentEvent::ToolCall { name, summary, .. } => println!("\n→ {name} {summary}"),
            AgentEvent::ToolResult { content, is_error, .. } => {
                let first: String = content.lines().take(6).collect::<Vec<_>>().join("\n  ");
                println!("  {}{first}", if is_error { "✗ " } else { "" });
            }
            AgentEvent::Step { usage, .. } => eprintln!("\n[{} in / {} out tokens]", usage.input_tokens, usage.output_tokens),
            AgentEvent::ToolStart { .. } => {}
            AgentEvent::Compacting { tokens_before } => eprintln!("
[compacting {tokens_before} tokens of conversation…]"),
            AgentEvent::Compacted { summary, .. } => eprintln!("[compacted to {} characters]", summary.len()),
        })
        .await;
    println!("\n== {end:?}; total {:?}", session.usage());
}
