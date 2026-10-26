//! The memory tools: built into the DevCat Agent and served by `devcat mcp` to Claude Code, Codex,
//! Cursor and Gemini, so every agent reads and writes the same memory (see `mod.rs`).
use super::{Place, place_for};
use crate::agent_tools::Scope;
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::sync::Arc;

/// Who calls the tools and where.
pub struct Ctx {
    /// The place the agent works in (none: only the memories for every project).
    pub here: Option<Place>,
    /// The agent, as memories record it (`codex`, `devcat-agent`, …).
    pub origin: String,
}

/// The memory tools for an agent working in `scope`.
pub fn for_scope(scope: &Scope, origin: &str) -> Vec<Arc<dyn Tool>> {
    let ctx = Arc::new(Ctx { here: place_for(scope), origin: origin.to_string() });
    vec![
        Arc::new(Search(ctx.clone())),
        Arc::new(ReadMemory(ctx.clone())),
        Arc::new(Save(ctx.clone())),
        Arc::new(Forget(ctx.clone())),
        Arc::new(HandoffWrite(ctx.clone())),
        Arc::new(HandoffTake(ctx)),
    ]
}

impl Ctx {
    /// This place, then every project.
    fn places(&self) -> Vec<Place> {
        self.here.iter().cloned().chain([Place::Global]).collect()
    }

    /// Where a save goes: `scope: "global"` (or a `user` memory) is for every project.
    fn target(&self, input: &Value, kind: &str) -> Result<Place, ToolOutput> {
        match input.get("scope").and_then(Value::as_str).map(str::trim) {
            Some("global") => Ok(Place::Global),
            Some("project") => self.here.clone().ok_or_else(|| ToolOutput::err("This folder isn't a project or repository DevCat knows; save it with scope \"global\" instead, or work from the project's folder.")),
            _ if kind == "user" => Ok(Place::Global),
            _ => Ok(self.here.clone().unwrap_or(Place::Global)),
        }
    }
}

fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).map(str::trim).unwrap_or("")
}

fn scope_word(p: &Place) -> &'static str {
    if *p == Place::Global { "every project" } else { "this project" }
}

// ---------- memory_search

struct Search(Arc<Ctx>);

#[async_trait]
impl Tool for Search {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_search".into(),
            description: "Search the memory the user's coding agents share (DevCat): preferences, corrections, decisions, project facts and pointers saved by any agent (Claude Code, Codex, Gemini, …). Searches this project's memories and the ones for every project; every word must match. An empty query lists them all. Use `memory_read` for a memory's whole text.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for (empty: list everything)" },
                    "limit": { "type": "integer", "description": "Most results (default 20)" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        text(input, "query").to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let limit = input.get("limit").and_then(Value::as_u64).map(|n| n.clamp(1, 100) as usize).unwrap_or(20);
        let hits = super::search(&self.0.places(), text(&input, "query"), limit);
        if hits.is_empty() {
            return ToolOutput::ok("No memories match.");
        }
        let lines: Vec<String> = hits
            .iter()
            .map(|m| {
                let about = if m.description.is_empty() { super::first_line(&m.body) } else { m.description.clone() };
                let by = if m.origin.is_empty() { String::new() } else { format!(", by {}", m.origin) };
                format!("- {} [{}, {}{by}, {}]: {about}", m.name, m.kind, scope_word(&m.place), m.updated)
            })
            .collect();
        ToolOutput::ok(lines.join("\n"))
    }
}

// ---------- memory_read

struct ReadMemory(Arc<Ctx>);

#[async_trait]
impl Tool for ReadMemory {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_read".into(),
            description: "Read one shared memory in full, by name (as `memory_search` or the brief lists it).".into(),
            input_schema: json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        text(input, "name").to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let name = text(&input, "name");
        match self.0.places().iter().find_map(|p| super::read(p, name)) {
            Some(m) => ToolOutput::ok(format!(
                "{} ({}, for {}; saved by {} on {})\n{}\n\n{}",
                m.name,
                m.kind,
                scope_word(&m.place),
                if m.origin.is_empty() { "an agent" } else { &m.origin },
                m.updated,
                m.description,
                m.body
            )),
            None => ToolOutput::err(format!("No memory called \"{name}\" here; `memory_search` lists them.")),
        }
    }
}

// ---------- memory_save

struct Save(Arc<Ctx>);

#[async_trait]
impl Tool for Save {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_save".into(),
            description: "Save something to the memory the user's coding agents share, so the next session, in any agent, knows it. Save when the user asks you to remember something, or you learn something durable: who the user is and how they like to work (type user), a correction or confirmed approach with why (feedback), a project fact, decision or constraint the code doesn't show (project), or where to find something (reference). One memory per topic: saving an existing name replaces it, so read and update rather than duplicate. Never save secrets.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short kebab-case name, e.g. `prefers-pnpm`" },
                    "description": { "type": "string", "description": "One line: what it says, to decide later whether it's relevant" },
                    "type": { "type": "string", "enum": super::KINDS },
                    "body": { "type": "string", "description": "The memory (markdown). For feedback and project: the fact, then **Why:** and **How to apply:**" },
                    "scope": { "type": "string", "enum": ["project", "global"], "description": "project (default: this project or repository) or global (every project; the default for type user)" }
                },
                "required": ["name", "description", "type", "body"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Edit
    }
    fn summary(&self, input: &Value) -> String {
        text(input, "name").to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let kind = text(&input, "type").to_lowercase();
        let place = match self.0.target(&input, &kind) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let existed = super::read(&place, text(&input, "name")).is_some();
        let new = super::NewMemory {
            name: text(&input, "name").into(),
            description: text(&input, "description").into(),
            kind,
            body: text(&input, "body").into(),
            origin: self.0.origin.clone(),
            source: None,
        };
        match super::save(&place, new) {
            Ok(m) => {
                if place == Place::Global {
                    super::rules::refresh();
                }
                ToolOutput::ok(format!("{} \"{}\" for {}. Every agent of the user's sees it from their next session.", if existed { "Updated" } else { "Saved" }, m.name, scope_word(&place)))
            }
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

// ---------- memory_forget

struct Forget(Arc<Ctx>);

#[async_trait]
impl Tool for Forget {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_forget".into(),
            description: "Remove a shared memory that's wrong or no longer true, by name. Only when the user asks, or the memory is clearly outdated.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "scope": { "type": "string", "enum": ["project", "global"], "description": "Where it is (default: this project, then every project)" }
                },
                "required": ["name"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Edit
    }
    fn summary(&self, input: &Value) -> String {
        text(input, "name").to_string()
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let name = text(&input, "name");
        let places: Vec<Place> = match text(&input, "scope") {
            "global" => vec![Place::Global],
            "project" => self.0.here.iter().cloned().collect(),
            _ => self.0.places(),
        };
        for p in &places {
            match super::forget(p, name) {
                Ok(true) => {
                    if *p == Place::Global {
                        super::rules::refresh();
                    }
                    return ToolOutput::ok(format!("Forgot \"{name}\" ({}).", scope_word(p)));
                }
                Ok(false) => {}
                Err(e) => return ToolOutput::err(e.to_string()),
            }
        }
        ToolOutput::err(format!("No memory called \"{name}\" here."))
    }
}

// ---------- handoffs

struct HandoffWrite(Arc<Ctx>);

#[async_trait]
impl Tool for HandoffWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "handoff_write".into(),
            description: "Leave a handoff for the next agent working in this project (the user may switch to another model or tool): what the task is, what's done, what's left, what failed and why, and the files or commands that matter. Write it when you stop before the task is done, or when the user says they're switching.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "summary": { "type": "string", "description": "The handoff (markdown)" } },
                "required": ["summary"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Edit
    }
    fn summary(&self, input: &Value) -> String {
        super::first_line(text(input, "summary"))
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(place) = self.0.here.clone() else {
            return ToolOutput::err("Handoffs belong to a project: this folder isn't one DevCat knows.");
        };
        match super::handoff_write(&place, &self.0.origin, text(&input, "summary")) {
            Ok(_) => ToolOutput::ok("Handoff left: the next agent in this project sees it when it starts."),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

struct HandoffTake(Arc<Ctx>);

#[async_trait]
impl Tool for HandoffTake {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "handoff_take".into(),
            description: "Take the newest open handoff another agent left in this project, to continue its work. Each handoff is given out once.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, _: &Value) -> String {
        String::new()
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        let Some(place) = self.0.here.clone() else { return ToolOutput::ok("No open handoff (this folder isn't a project DevCat knows).") };
        match super::handoff_take(&place) {
            Ok(Some(h)) => ToolOutput::ok(format!("Handoff from {} ({}):\n\n{}", if h.origin.is_empty() { "another agent" } else { &h.origin }, h.created, h.body)),
            Ok(None) => ToolOutput::ok("No open handoff in this project."),
            Err(e) => ToolOutput::err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolCtx {
        ToolCtx { cwd: std::env::temp_dir() }
    }

    fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        tools.iter().find(|t| t.spec().name == name).cloned().unwrap()
    }

    #[tokio::test]
    async fn one_agent_saves_another_finds_it() {
        super::super::tests::data();
        let dir = std::env::temp_dir().join(format!("devcat-memory-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let scope = Scope::folder(&dir.join("src"));
        let claude = for_scope(&scope, "claude-code");
        let codex = for_scope(&scope, "codex");

        let saved = tool(&claude, "memory_save")
            .run(json!({ "name": "test-db", "description": "Tests need the docker postgres", "type": "project", "body": "Run `docker compose up db` first." }), &ctx())
            .await;
        assert!(!saved.is_error, "{}", saved.content);
        let found = tool(&codex, "memory_search").run(json!({ "query": "docker postgres" }), &ctx()).await;
        assert!(found.content.contains("test-db [project, this project, by claude-code"), "{}", found.content);
        let read = tool(&codex, "memory_read").run(json!({ "name": "test-db" }), &ctx()).await;
        assert!(read.content.contains("docker compose up db"), "{}", read.content);

        // a subfolder of the same repository is the same place
        let elsewhere = for_scope(&Scope::folder(&dir), "gemini");
        assert!(tool(&elsewhere, "memory_search").run(json!({ "query": "test-db" }), &ctx()).await.content.contains("test-db"));

        tool(&claude, "handoff_write").run(json!({ "summary": "Half the migration is done" }), &ctx()).await;
        let taken = tool(&codex, "handoff_take").run(json!({}), &ctx()).await;
        assert!(taken.content.contains("from claude-code") && taken.content.contains("Half the migration"), "{}", taken.content);

        assert!(!tool(&codex, "memory_forget").run(json!({ "name": "test-db" }), &ctx()).await.is_error);
        assert!(tool(&codex, "memory_read").run(json!({ "name": "test-db" }), &ctx()).await.is_error);
        let bad = tool(&codex, "memory_save").run(json!({ "name": "x", "description": "d", "type": "secret", "body": "b" }), &ctx()).await;
        assert!(bad.is_error);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn user_memories_go_to_every_project() {
        super::super::tests::data();
        let none = for_scope(&Scope::default(), "codex");
        let out = tool(&none, "memory_save")
            .run(json!({ "name": "tool-test-user-role", "description": "Backend engineer", "type": "user", "body": "Writes Rust and TypeScript" }), &ctx())
            .await;
        assert!(out.content.contains("for every project"), "{}", out.content);
        assert!(super::super::read(&Place::Global, "tool-test-user-role").is_some());
        assert!(tool(&none, "handoff_write").run(json!({ "summary": "x" }), &ctx()).await.is_error, "no project: no handoff");
        super::super::forget(&Place::Global, "tool-test-user-role").unwrap();
    }
}
