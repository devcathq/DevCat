//! Claude workspace: unattended runs for other features (ticket fixing). One prompt, edits allowed,
//! no window: what Claude does is reported as one-line activity, and the final `result` (with
//! `structured_output` when a schema is given) is returned instead of streamed.
use super::runner::{Sink, Spawn, spawn_run, user_message, write_to};
use super::*;
use crate::error::{AppError, AppResult};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

pub struct Job {
    pub cwd: String,
    pub prompt: String,
    pub model: Option<String>,
    /// Shell commands (builds, tests) run without asking; otherwise only file edits do.
    pub allow_commands: bool,
    /// JSON schema the final answer must follow (`--json-schema`).
    pub schema: Option<Value>,
    pub max_turns: u32,
    pub program: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// Images sent with the prompt: (media type, base64).
    pub images: Vec<(String, String)>,
    /// `--mcp-config`: DevCat's MCP server (shared memory, connections), whose tools the run may
    /// use without asking (see `mcp_sync::run_config`).
    pub mcp: Option<Value>,
    /// Set to Claude's session id as soon as it's known (kept when the run fails or is stopped,
    /// so the chat can still be opened).
    pub session: Arc<parking_lot::Mutex<Option<String>>>,
    /// Go on with this Claude session instead of starting one.
    pub resume: Option<String>,
}

pub struct Outcome {
    pub structured: Option<Value>,
    pub text: String,
    pub cost_usd: Option<f64>,
}

/// Run `job` to its end. `activity` gets a line per tool call; `cancelled` is polled.
pub async fn run(job: Job, activity: Arc<dyn Fn(String) + Send + Sync>, cancelled: impl Fn() -> bool) -> AppResult<Outcome> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(&'static str, Value)>();
    let sink: Sink = Arc::new(move |e, v| drop(tx.send((e, v))));
    let mut extra: Vec<String> = vec!["--max-turns".into(), job.max_turns.to_string()];
    if !job.allow_commands {
        // acceptEdits without a prompt tool: anything else that would ask is refused.
        extra.extend(["--disallowedTools".into(), "Bash".into()]);
    }
    if let Some(m) = &job.mcp {
        extra.extend(["--mcp-config".into(), m.to_string()]);
        if !job.allow_commands {
            extra.extend(["--allowedTools".into(), format!("mcp__{}", crate::mcp_sync::NAME)]);
        }
    }
    if let Some(s) = &job.schema {
        extra.extend(["--json-schema".into(), s.to_string()]);
    }
    let mode = if job.allow_commands { "bypassPermissions" } else { "acceptEdits" };
    let spec = Spawn {
        opts: StartOptions { model: job.model.clone(), permission_mode: Some(mode.into()), resume: job.resume.clone(), ..Default::default() },
        prompt_tool: false,
        extra_args: extra,
        close_after_result: true,
        program: job.program.clone(),
        env: job.env.clone(),
    };
    let run_id = spawn_run(sink, &job.cwd, spec).await?;
    write_to(&run_id, &user_message(&job.prompt, &job.images)).await?;

    let deadline = tokio::time::Instant::now() + job.timeout;
    let mut result: Option<Value> = None;
    loop {
        let next = tokio::select! {
            m = rx.recv() => m,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if cancelled() || tokio::time::Instant::now() > deadline {
                    let timed_out = !cancelled();
                    let _ = super::runner::claude_stop(run_id.clone()).await;
                    return Err(AppError::msg(if timed_out { "Claude took too long and was stopped" } else { "stopped" }));
                }
                continue;
            }
        };
        let Some((event, v)) = next else { break };
        match event {
            "claude-msg" => {
                let m = &v["msg"];
                if let Some(id) = m["session_id"].as_str().filter(|s| !s.is_empty()) {
                    *job.session.lock() = Some(id.to_string());
                }
                match m["type"].as_str() {
                    Some("assistant") => {
                        for b in m["message"]["content"].as_array().into_iter().flatten() {
                            if b["type"] == "tool_use" {
                                activity(tool_line(b["name"].as_str().unwrap_or("tool"), &b["input"]));
                            }
                        }
                    }
                    Some("result") => result = Some(m.clone()),
                    _ => {}
                }
            }
            "claude-exit" => {
                if result.is_none() {
                    return Err(AppError::msg(v["error"].as_str().unwrap_or("Claude exited without an answer").to_string()));
                }
                break;
            }
            _ => {}
        }
    }
    let r = result.ok_or_else(|| AppError::msg("Claude exited without an answer"))?;
    if r["is_error"] == json!(true) && r["structured_output"].is_null() {
        let why = r["result"].as_str().or(r["subtype"].as_str()).unwrap_or("error").to_string();
        return Err(AppError::msg(format!("Claude stopped: {why}")));
    }
    Ok(Outcome {
        structured: r.get("structured_output").filter(|v| !v.is_null()).cloned(),
        text: r["result"].as_str().unwrap_or_default().to_string(),
        cost_usd: r["total_cost_usd"].as_f64(),
    })
}

/// "Edit src/app.rs", "Bash cargo test"… for the activity log.
pub fn tool_line(name: &str, input: &Value) -> String {
    let arg = ["file_path", "path", "command", "pattern", "url", "description"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    let arg: String = arg.lines().next().unwrap_or("").chars().take(120).collect();
    if arg.is_empty() { name.to_string() } else { format!("{name} {arg}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_lines() {
        assert_eq!(tool_line("Edit", &json!({ "file_path": "src/a.rs", "old_string": "x" })), "Edit src/a.rs");
        assert_eq!(tool_line("Bash", &json!({ "command": "cargo test\n--quiet" })), "Bash cargo test");
        assert_eq!(tool_line("TodoWrite", &json!({})), "TodoWrite");
    }
}
