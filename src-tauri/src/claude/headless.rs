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
    /// Told the program's process id once it started (the ticket worker notes it, to stop it
    /// after a crash: `tickets::recover`).
    pub spawned: Option<Arc<dyn Fn(u32) + Send + Sync>>,
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
        own_group: true,
    };
    let run_id = spawn_run(sink, &job.cwd, spec).await?;
    if let (Some(f), Some(pid)) = (&job.spawned, super::runner::run_pid(&run_id)) {
        f(pid);
    }
    write_to(&run_id, &user_message(&job.prompt, &job.images)).await?;

    let deadline = tokio::time::Instant::now() + job.timeout;
    // Stop and the time limit are checked on every message and at least every 250 ms, so a run
    // that streams messages all the time can't put them off.
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut result: Option<Value> = None;
    loop {
        if cancelled() || tokio::time::Instant::now() >= deadline {
            let timed_out = !cancelled();
            let _ = super::runner::claude_stop(run_id.clone()).await;
            return Err(AppError::msg(if timed_out { "Claude took too long and was stopped" } else { "stopped" }));
        }
        let next = tokio::select! {
            m = rx.recv() => m,
            _ = tick.tick() => continue,
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

    /// When a run ended, counted from the stand-in's first message: when DevCat closed its stdin
    /// (the stop began) and when `run` returned.
    #[cfg(unix)]
    struct Ended {
        result: AppResult<Outcome>,
        stopping: Duration,
        returned: Duration,
    }

    /// Runs a stand-in for Claude Code that sends a tool call every 100 ms and never answers (nor
    /// exits when its stdin closes), with `timeout`; `stop_after` its first message, the run is
    /// stopped.
    #[cfg(unix)]
    async fn chatty(timeout: Duration, stop_after: Option<Duration>) -> Ended {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;
        let out = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("devcat-headless-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&out).unwrap();
        let script = out.join("claude");
        // fd 3: a job in the background reads /dev/null as its stdin, not ours
        std::fs::write(&script, format!(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"type\":\"user\"'*) break ;;\n  esac\ndone\nexec 3<&0\n(cat <&3 > /dev/null; touch '{0}/eof') &\nwhile true; do\n  printf '%s\\n' '{{\"type\":\"assistant\",\"session_id\":\"chatty-1\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{{\"file_path\":\"a.txt\"}}}}]}}}}'\n  sleep 0.1\ndone\n",
            out.display()
        )).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let job = Job {
            cwd: out.to_string_lossy().into_owned(),
            prompt: "Work forever.".into(),
            model: None,
            allow_commands: false,
            schema: None,
            max_turns: 1,
            program: Some(script),
            env: vec![],
            timeout,
            images: vec![],
            mcp: None,
            session: Default::default(),
            resume: None,
            spawned: None,
        };
        let first: Arc<parking_lot::Mutex<Option<Instant>>> = Default::default();
        let f = first.clone();
        let activity: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_| {
            f.lock().get_or_insert_with(Instant::now);
        });
        let f = first.clone();
        let cancelled = move || stop_after.is_some_and(|after| f.lock().is_some_and(|t| t.elapsed() >= after));
        let eof = out.join("eof");
        let closed = tokio::spawn(async move {
            for _ in 0..1000 {
                if eof.exists() {
                    return Some(Instant::now());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            None
        });
        let result = tokio::time::timeout(Duration::from_secs(20), run(job, activity, cancelled)).await.expect("still running 20 s later");
        let returned = Instant::now();
        let closed = closed.await.unwrap().expect("its stdin was never closed");
        let first = first.lock().expect("no message from the stand-in");
        let _ = std::fs::remove_dir_all(&out);
        Ended { result, stopping: closed - first, returned: returned - first }
    }

    /// A run that sends messages all the time is still stopped by its time limit, as soon as it's
    /// up: the stop begins within a moment (stdin closed) and the run ends once the program is
    /// gone (`STOP_GRACE`, then killed). The limit counts from the prompt, a little before the
    /// first message (a new script can take a second to start on macOS).
    #[cfg(unix)]
    #[tokio::test]
    async fn time_limit_while_messages_stream() {
        let e = chatty(Duration::from_secs(3), None).await;
        assert!(e.result.as_ref().is_err_and(|err| err.to_string().contains("took too long")), "{:?}", e.result.as_ref().err());
        assert!(e.stopping < Duration::from_millis(3800), "the stop began {:?} after the first message (limit 3 s)", e.stopping);
        assert!(e.returned < Duration::from_secs(6), "ended {:?} after the first message", e.returned);
    }

    /// Stop acts within a moment while messages stream.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_while_messages_stream() {
        let e = chatty(Duration::from_secs(60), Some(Duration::from_secs(1))).await;
        assert!(e.result.as_ref().is_err_and(|err| err.to_string() == "stopped"), "{:?}", e.result.as_ref().err());
        assert!(e.stopping < Duration::from_millis(1800), "the stop began {:?} after the first message (stopped at 1 s)", e.stopping);
        assert!(e.returned < Duration::from_secs(6), "ended {:?} after the first message", e.returned);
    }
}
