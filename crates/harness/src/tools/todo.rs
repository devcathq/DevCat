//! `todo_write`: the model's checklist for multi-step work. The whole list is sent each time; the
//! chat shows the latest one, so the user sees the plan and where the agent is in it.
use super::{Tool, ToolCtx, ToolOutput};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::{Value, json};

pub const TODO_WRITE: &str = "todo_write";

pub struct TodoWrite;

#[async_trait]
impl Tool for TodoWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TODO_WRITE.into(),
            description: "Keep a checklist for work with several steps (3 or more), so the user can follow along. Send the whole list every time. Mark a step `in_progress` before you start it (one at a time) and `completed` as soon as it's done. Skip it for small tasks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "The step, in a few words" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        let todos = input.get("todos").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let done = todos.iter().filter(|t| t.get("status").and_then(Value::as_str) == Some("completed")).count();
        format!("{done}/{} done", todos.len())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(todos) = input.get("todos").and_then(Value::as_array) else {
            return ToolOutput::err("Missing required array parameter `todos`.");
        };
        let mut lines = Vec::new();
        for t in todos {
            let content = t.get("content").and_then(Value::as_str).unwrap_or("").trim();
            let mark = match t.get("status").and_then(Value::as_str) {
                Some("completed") => "[x]",
                Some("in_progress") => "[>]",
                Some("pending") => "[ ]",
                other => return ToolOutput::err(format!("Unknown status {other:?} for \"{content}\": use pending, in_progress or completed.")),
            };
            if content.is_empty() {
                return ToolOutput::err("Every step needs a `content`.");
            }
            lines.push(format!("{mark} {content}"));
        }
        ToolOutput::ok(if lines.is_empty() { "Todo list cleared.".into() } else { format!("Todo list updated:\n{}", lines.join("\n")) })
    }
}
