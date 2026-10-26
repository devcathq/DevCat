//! The MCP client against a real (tiny, Python) stdio server: handshake, tools/list, tools/call.
use devcat_harness::mcp::{McpTool, Server, ServerConfig};
use devcat_harness::tools::{Tool, ToolCtx};
use serde_json::json;

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| std::process::Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success()))
}

#[tokio::test]
async fn lists_and_calls_tools() {
    let Some(py) = python() else {
        eprintln!("no python: skipped");
        return;
    };
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_server.py");
    let cfg = ServerConfig { name: "calc".into(), command: py.into(), args: vec![script.to_string_lossy().into()], source: "test".into(), ..Default::default() };
    let server = Server::start(&cfg).await.expect("server starts");
    let tools = McpTool::all(&server);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "mcp__calc__add");
    assert!(tools[0].spec().description.contains("Add two numbers"));
    let out = tools[0].run(json!({ "a": 2, "b": 40 }), &ToolCtx::at(std::env::temp_dir())).await;
    assert_eq!((out.content.as_str(), out.is_error), ("42", false));
}
