//! Run a command: PowerShell on Windows, bash (or sh) elsewhere. Each call is a fresh process in
//! the project folder; output is stdout and stderr together, then the exit code.
use super::{Tool, ToolCtx, ToolOutput, str_arg, truncate};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

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
        input.get("timeout_secs").and_then(Value::as_u64).map(Duration::from_secs).unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT) + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let command = match str_arg(&input, "command") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let limit = input.get("timeout_secs").and_then(Value::as_u64).map(Duration::from_secs).unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let mut cmd = command_for(command);
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
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("Couldn't start {}: {e}", shell_name())),
        };
        let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let read = async {
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let _ = tokio::join!(out.read_to_end(&mut o), err.read_to_end(&mut e));
            let status = child.wait().await;
            (o, e, status)
        };
        match tokio::time::timeout(limit, read).await {
            Ok((o, e, status)) => {
                let mut text = String::from_utf8_lossy(&o).into_owned();
                let e = String::from_utf8_lossy(&e);
                if !e.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&e);
                }
                let code = status.ok().and_then(|s| s.code());
                let text = truncate(text.trim_end());
                match code {
                    Some(0) => ToolOutput::ok(if text.is_empty() { "(no output)".into() } else { text }),
                    // 0xC0000142 / 0xC0000017: Windows couldn't start the process (out of memory
                    // or desktop heap). The command itself was never run.
                    Some(c @ (-1073741502 | -1073741801)) if text.is_empty() => ToolOutput::err(format!(
                        "{} didn't start (exit code {c:#X}): Windows is low on memory. The command was not run; try it again, or ask the user to free some memory.",
                        shell_name()
                    )),
                    Some(c) => ToolOutput::err(format!("{text}\n(exit code {c})").trim_start().to_string()),
                    None => ToolOutput::err(format!("{text}\n(killed by a signal)").trim_start().to_string()),
                }
            }
            Err(_) => ToolOutput::err(format!("Timed out after {} s and was stopped.", limit.as_secs())),
        }
    }
}

fn command_for(command: &str) -> tokio::process::Command {
    if cfg!(windows) {
        let mut c = tokio::process::Command::new("powershell.exe");
        // UTF-8 output instead of the console code page
        let script = format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ProgressPreference = 'SilentlyContinue'; {command}");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script]);
        c
    } else {
        let sh = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        let mut c = tokio::process::Command::new(sh);
        c.args(["-c", command]);
        c
    }
}

#[cfg(unix)]
fn libc_setsid() {
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
        let ctx = ToolCtx { cwd: std::env::temp_dir() };
        let ok = Shell.run(json!({ "command": "echo hello" }), &ctx).await;
        assert_eq!((ok.content.trim(), ok.is_error), ("hello", false));

        let fail = Shell.run(json!({ "command": if cfg!(windows) { "Write-Error 'boom'; exit 3" } else { "echo boom >&2; exit 3" } }), &ctx).await;
        assert!(fail.is_error && fail.content.contains("boom") && fail.content.contains("exit code 3"), "{}", fail.content);

        let slow = Shell.run(json!({ "command": if cfg!(windows) { "Start-Sleep -Seconds 5" } else { "sleep 5" }, "timeout_secs": 1 }), &ctx).await;
        assert!(slow.is_error && slow.content.contains("Timed out"), "{}", slow.content);
    }
}
