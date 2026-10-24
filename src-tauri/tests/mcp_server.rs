//! `devcat mcp` end to end: the real program, started the way Claude Code starts it, spoken to
//! with the DevCat Agent's MCP client. A project's connections show up in its code folder only,
//! and over MCP data changes only where the project allows them.
use devcat_harness::mcp::{McpTool, Server, ServerConfig};
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn tmp(what: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("devcat-mcp-{what}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start(data: &Path, args: &[&str]) -> Vec<McpTool> {
    let mut a = vec!["mcp".to_string()];
    a.extend(args.iter().map(|s| s.to_string()));
    a.extend(["--data-dir".to_string(), data.to_string_lossy().to_string()]);
    // a fresh test build reading the macOS keychain would wait for the user to allow it
    let env = vec![("DEVCAT_TEST_MEMORY_SECRETS".to_string(), "1".to_string())];
    let cfg = ServerConfig { name: "devcat".into(), command: env!("CARGO_BIN_EXE_devcat").into(), args: a, env, source: "test".into(), ..Default::default() };
    McpTool::all(&Server::start(&cfg).await.expect("devcat mcp starts"))
}

async fn call(tools: &[McpTool], name: &str, input: Value) -> ToolOutput {
    let t = tools.iter().find(|t| t.spec().name == format!("mcp__devcat__{name}")).unwrap_or_else(|| panic!("no tool {name}"));
    t.run(input, &ToolCtx { cwd: std::env::temp_dir() }).await
}

#[tokio::test]
async fn serves_the_folders_connections() {
    let (data, repo, elsewhere) = (tmp("data"), tmp("repo"), tmp("elsewhere"));
    for db in ["shop.db", "notes.db"] {
        std::fs::write(data.join(db), b"").unwrap();
    }
    let projects = json!([
        { "id": "shop", "name": "Shop", "order": 0, "folders": [repo.to_string_lossy()], "mcpWrites": true },
        { "id": "misc", "name": "Misc", "order": 1 }
    ]);
    let connections = json!([
        { "id": "c1", "name": "shop-db", "kind": "sqlite", "filePath": data.join("shop.db").to_string_lossy(), "projectId": "shop" },
        { "id": "c2", "name": "notes", "kind": "sqlite", "filePath": data.join("notes.db").to_string_lossy(), "projectId": "misc" }
    ]);
    std::fs::write(data.join("projects.json"), projects.to_string()).unwrap();
    std::fs::write(data.join("connections.json"), connections.to_string()).unwrap();

    // in the project's folder: its connections and the shared ones
    let tools = start(&data, &["--folder", &repo.to_string_lossy()]).await;
    let names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
    assert_eq!(names, ["mcp__devcat__devcat_connections", "mcp__devcat__db_schema", "mcp__devcat__db_query"]);
    let list = call(&tools, "devcat_connections", json!({})).await;
    assert!(list.content.contains("Project Shop") && list.content.contains("shop-db") && list.content.contains("notes"), "{}", list.content);

    let made = call(&tools, "db_query", json!({ "connection": "shop-db", "query": "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL); INSERT INTO orders (total) VALUES (9.5), (12);" })).await;
    assert!(!made.is_error, "the project allows writes: {}", made.content);
    let read = call(&tools, "db_query", json!({ "connection": "shop-db", "query": "SELECT count(*) AS n FROM orders" })).await;
    assert!(!read.is_error && read.content.starts_with("n\n2\n"), "{}", read.content);
    let schema = call(&tools, "db_schema", json!({ "connection": "shop-db" })).await;
    assert!(schema.content.contains("orders(id INTEGER PK, total REAL)"), "{}", schema.content);

    let refused = call(&tools, "db_query", json!({ "connection": "notes", "query": "CREATE TABLE x (a INTEGER)" })).await;
    assert!(refused.is_error && refused.content.contains("Allow writes from MCP"), "{}", refused.content);
    let reads = call(&tools, "db_query", json!({ "connection": "notes", "query": "SELECT 1 AS one" })).await;
    assert!(!reads.is_error, "reads are fine: {}", reads.content);

    // anywhere else the project's connections aren't there
    let tools = start(&data, &["--folder", &elsewhere.to_string_lossy()]).await;
    let list = call(&tools, "devcat_connections", json!({})).await;
    assert!(!list.content.contains("shop-db") && list.content.contains("notes"), "{}", list.content);
    let hidden = call(&tools, "db_query", json!({ "connection": "shop-db", "query": "SELECT 1" })).await;
    assert!(hidden.is_error && hidden.content.contains("No DevCat connection"), "{}", hidden.content);

    // one project by id, whatever the folder
    let tools = start(&data, &["--project", "shop"]).await;
    assert!(call(&tools, "devcat_connections", json!({})).await.content.contains("shop-db"));

    for d in [data, repo, elsewhere] {
        let _ = std::fs::remove_dir_all(d);
    }
}
