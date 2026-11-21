//! Run a command: PowerShell on Windows, bash (or sh) elsewhere. Each call is a fresh process in
//! the project folder; output is stdout and stderr together, then the exit code.
use super::{Tool, ToolCtx, ToolOutput, str_arg};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// How long the command may run: `timeout_secs`, at most [`MAX_TIMEOUT`]. Models often write
/// `timeout` instead (other agents' shell tools call it that, some in milliseconds); ignoring it
/// stops a build the model gave five minutes after two.
pub fn limit_of(input: &Value) -> Duration {
    let secs = input.get("timeout_secs").and_then(Value::as_u64).or_else(|| {
        // nobody means more than an hour: that's milliseconds
        input.get("timeout").and_then(Value::as_u64).map(|t| if t > 3_600 { t / 1_000 } else { t })
    });
    secs.filter(|s| *s > 0).map(Duration::from_secs).unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT)
}

pub struct Shell;

fn shell_name() -> &'static str {
    if cfg!(windows) { "PowerShell" } else { "bash" }
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: format!(
                "Run a {} command in the project folder and return its output and exit code. Each call is a new process (`cd` doesn't carry over). Not interactive: don't start editors, pagers or servers that never exit. Default timeout 120 s, at most 600 s.",
                shell_name()
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 600 }
                },
                "required": ["command"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        input.get("command").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    fn timeout(&self, input: &Value) -> Duration {
        // the pipeline's limit; a little over the command's own, so its message wins
        limit_of(input) + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let command = match str_arg(&input, "command") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let limit = limit_of(&input);
        // the sandbox's guard lives until the command has started
        let (cmd, _guard) = match command_for(command, ctx) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("Not run: the sandbox is on and couldn't be used. {e}")),
        };
        let (text, code) = match run_process(cmd, ctx, limit, shell_name()).await {
            Ok(r) => r,
            Err(e) => return e,
        };
        match code {
            // 0xC0000142 / 0xC0000017: Windows couldn't start the process (out of memory
            // or desktop heap). The command itself was never run.
            Some(c @ (-1073741502 | -1073741801)) if text.is_empty() => ToolOutput::err(format!(
                "{} didn't start (exit code {c:#X}): Windows is low on memory. The command was not run; try it again, or ask the user to free some memory.",
                shell_name()
            )),
            _ => exit_output(text, code),
        }
    }
}

/// Run `cmd` in the project folder with no stdin: stdout and stderr together (the pipeline cuts long
/// output), and the exit code. `what` names the program in errors.
pub(super) async fn run_process(mut cmd: tokio::process::Command, ctx: &ToolCtx, limit: Duration, what: &str) -> Result<(String, Option<i32>), ToolOutput> {
    cmd.current_dir(&ctx.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    // Its own session: no terminal to take over (see #2), and the whole group can be killed.
    unsafe {
        cmd.pre_exec(|| {
            libc_setsid();
            Ok(())
        });
    }
    // DevCat has no console of its own: without this, Windows opens one per command and it flashes
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| ToolOutput::err(format!("Couldn't start {what}: {e}")))?;
    // On a timeout or a stop (this future is dropped), what the command started goes with it.
    // Killing only the shell would leave a build or an install running: it keeps its locks, and
    // the next command waits on them until it times out too.
    let tree = crate::hooks::Tree::of(&child);
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let read = async {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let _ = tokio::join!(out.read_to_end(&mut o), err.read_to_end(&mut e));
        let status = child.wait().await;
        (o, e, status)
    };
    let (o, e, status) = tokio::time::timeout(limit, read).await.map_err(|_| ToolOutput::err(format!("Timed out after {} s and was stopped, with everything it started.", limit.as_secs())))?;
    // it ended by itself: what it left running on purpose stays
    tree.done();
    let mut text = String::from_utf8_lossy(&o).into_owned();
    let e = String::from_utf8_lossy(&e);
    if !e.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&e);
    }
    Ok((text.trim_end().to_string(), status.ok().and_then(|s| s.code())))
}

/// The output of a finished process for the model: an error unless it exited with 0.
pub fn exit_output(text: String, code: Option<i32>) -> ToolOutput {
    match code {
        Some(0) => ToolOutput::ok(if text.is_empty() { "(no output)".into() } else { text }),
        Some(c) => ToolOutput::err(format!("{text}\n(exit code {c})").trim_start().to_string()),
        None => ToolOutput::err(format!("{text}\n(killed by a signal)").trim_start().to_string()),
    }
}

fn command_for(command: &str, ctx: &ToolCtx) -> Result<(tokio::process::Command, crate::sandbox::Guard), String> {
    if cfg!(windows) {
        // UTF-8 output instead of the console code page
        let script = format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ProgressPreference = 'SilentlyContinue'; {command}");
        let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script].map(String::from);
        crate::sandbox::command("powershell.exe", &args, &ctx.cwd, ctx.sandbox, true)
    } else {
        let sh = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        // the repository is git's: no hooks or config planted for a later git call
        crate::sandbox::command(sh, &["-c".to_string(), command.to_string()], &ctx.cwd, ctx.sandbox, true)
    }
}

#[cfg(unix)]
pub(crate) fn libc_setsid() {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    // SAFETY: async-signal-safe; can't fail in a freshly forked child.
    unsafe {
        setsid();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_reports_exit_code_and_times_out() {
        let ctx = ToolCtx::at(std::env::temp_dir());
        let ok = Shell.run(json!({ "command": "echo hello" }), &ctx).await;
        assert_eq!((ok.content.trim(), ok.is_error), ("hello", false));

        let fail = Shell.run(json!({ "command": if cfg!(windows) { "Write-Error 'boom'; exit 3" } else { "echo boom >&2; exit 3" } }), &ctx).await;
        assert!(fail.is_error && fail.content.contains("boom") && fail.content.contains("exit code 3"), "{}", fail.content);

        let slow = Shell.run(json!({ "command": if cfg!(windows) { "Start-Sleep -Seconds 5" } else { "sleep 5" }, "timeout_secs": 1 }), &ctx).await;
        assert!(slow.is_error && slow.content.contains("Timed out"), "{}", slow.content);
    }

    #[test]
    fn the_time_limit_is_read_however_the_model_writes_it() {
        let secs = |input: Value| limit_of(&input).as_secs();
        assert_eq!(secs(json!({ "command": "x" })), 120);
        assert_eq!(secs(json!({ "command": "x", "timeout_secs": 300 })), 300);
        // `timeout`, in seconds or (as some agents' tools have it) milliseconds
        assert_eq!(secs(json!({ "command": "x", "timeout": 300 })), 300);
        assert_eq!(secs(json!({ "command": "x", "timeout": 300_000 })), 300);
        assert_eq!(secs(json!({ "command": "x", "timeout_secs": 30, "timeout": 300 })), 30);
        // never more than ten minutes, and nonsense is the default
        assert_eq!(secs(json!({ "command": "x", "timeout_secs": 9_000 })), 600);
        assert_eq!(secs(json!({ "command": "x", "timeout": 0 })), 120);
        assert_eq!(secs(json!({ "command": "x", "timeout": "soon" })), 120);
    }

    /// A command that times out takes what it started with it: a build left running would keep
    /// its locks and make the next command wait.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timeout_stops_what_the_command_started_too() {
        let dir = std::env::temp_dir().join(format!("devcat-harness-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx::at(&dir);
        // a child of the shell that would write a file after the timeout, if it were still alive
        let slow = Shell.run(json!({ "command": "(sleep 3; echo late > late.txt) & wait", "timeout_secs": 1 }), &ctx).await;
        assert!(slow.is_error && slow.content.contains("Timed out"), "{}", slow.content);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!dir.join("late.txt").exists(), "the command's child outlived the timeout");

        // a command that ends by itself leaves what it started in the background alone
        let ok = Shell.run(json!({ "command": "(sleep 1; echo kept > kept.txt) > /dev/null 2>&1 &" }), &ctx).await;
        assert!(!ok.is_error, "{}", ok.content);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(dir.join("kept.txt").exists(), "a background process was killed with a command that ended normally");
        let _ = std::fs::remove_dir_all(dir);
    }
}
