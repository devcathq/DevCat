//! Run git in the project folder. Commands that only read the repository (status, diff, log, …)
//! run without asking; anything else changes it or talks to a remote, so it asks like `shell`.
//! Arguments come as a list (no shell quoting), and git never waits for a pager, an editor or a
//! password prompt.
use super::shell::{exit_output, run_process};
use super::{Tool, ToolCtx, ToolOutput};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Subcommands that only read the repository, whatever their options (see [`writes_a_file`]).
const READS: &[&str] = &["status", "diff", "log", "show", "blame", "grep", "ls-files", "rev-parse", "describe", "shortlog", "merge-base"];

pub struct Git;

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git".into(),
            description: "Run git in the project folder. `args` are the arguments after `git`, one per item: [\"status\", \"--short\"], [\"diff\", \"--stat\", \"HEAD~3\"], [\"log\", \"--oneline\", \"-20\"], [\"show\", \"HEAD:src/main.rs\"]. Commands that only read the repository (status, diff, log, show, blame, grep, ls-files, rev-parse, describe, shortlog, merge-base, reflog, and listing branches, tags, remotes, stashes or config) run without asking; anything else (commit, checkout, reset, merge, stash, fetch, pull, push, …) asks the user first. There's no pager, color or editor: give commit messages with -m. Prefer this over `shell` for git.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "args": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                },
                "required": ["args"]
            }),
        }
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        match args(input) {
            Ok(a) if read_only(&a) => Kind::Read,
            _ => Kind::Exec,
        }
    }
    fn summary(&self, input: &Value) -> String {
        let a = args(input).unwrap_or_default();
        let quoted: Vec<String> = a.iter().map(|s| if s.is_empty() || s.contains(char::is_whitespace) { format!("\"{s}\"") } else { s.clone() }).collect();
        format!("git {}", quoted.join(" "))
    }
    fn timeout(&self, _: &Value) -> Duration {
        // the pipeline's limit; a little over git's own, so its message wins
        TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let a = match args(&input) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let args: Vec<String> = ["--no-pager", "-c", "color.ui=false", "-c", "core.quotePath=false"].iter().map(|s| s.to_string()).chain(a).collect();
        // the sandbox's guard lives until git has started
        let (mut cmd, _guard) = match crate::sandbox::command("git", &args, &ctx.cwd, ctx.sandbox, false) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("Not run: the sandbox is on and couldn't be used. {e}")),
        };
        // no prompt can wait for an answer that never comes: credentials, editors (commit without -m,
        // rebase -i), and no optional locks, so reading doesn't get in the way of the user's own git
        cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "devcat-agent-no-editor").env("GIT_SEQUENCE_EDITOR", "devcat-agent-no-editor").env("GIT_OPTIONAL_LOCKS", "0");
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        match run_process(cmd, ctx, TIMEOUT, "git").await {
            Ok((text, code)) => exit_output(text, code),
            Err(e) => e,
        }
    }
}

/// `args` as strings, starting with the subcommand.
fn args(input: &Value) -> Result<Vec<String>, ToolOutput> {
    let list = input.get("args").and_then(Value::as_array).ok_or_else(|| ToolOutput::err("Missing required parameter `args`: a list such as [\"status\", \"--short\"]."))?;
    let a: Vec<String> = list.iter().map(|v| v.as_str().map(String::from)).collect::<Option<_>>().ok_or_else(|| ToolOutput::err("`args` must be a list of strings."))?;
    match a.first() {
        None => Err(ToolOutput::err("`args` is empty: start it with the subcommand, such as \"status\".")),
        Some(s) if s.starts_with('-') => Err(ToolOutput::err("Start `args` with the subcommand (status, diff, log, …): options before it, like -C or -c, aren't allowed.")),
        Some(_) => Ok(a),
    }
}

/// Whether `git <args>` only reads the repository.
fn read_only(a: &[String]) -> bool {
    let (sub, rest) = (a[0].as_str(), &a[1..]);
    if rest.iter().any(|s| writes_a_file(s)) {
        return false;
    }
    let has = |names: &[&str]| rest.iter().any(|s| names.contains(&s.as_str()));
    match sub {
        s if READS.contains(&s) => true,
        // listing only: no name to create, nothing to delete, move, copy or track
        "branch" => {
            let changes = rest.iter().any(|s| {
                ["-d", "-D", "--delete", "-m", "-M", "--move", "-c", "-C", "--copy", "-u", "--unset-upstream", "--edit-description", "-f", "--force", "-t", "--track", "--no-track", "--create-reflog"].contains(&s.as_str())
                    || s.starts_with("--set-upstream-to")
                    || (s.starts_with('-') && !s.starts_with("--") && s.len() > 2 && !s[1..].chars().all(|c| "arvl".contains(c)))
            });
            !changes && (rest.iter().all(|s| s.starts_with('-')) || has(&["-l", "--list"]))
        }
        "tag" => rest.is_empty() || (has(&["-l", "--list"]) && !has(&["-d", "--delete", "-a", "--annotate", "-s", "--sign", "-f", "--force", "-m", "--message", "-F", "--file", "-u", "--local-user"])),
        "remote" => rest.is_empty() || rest == ["-v"] || rest == ["--verbose"] || rest.first().is_some_and(|s| s == "get-url"),
        "stash" => rest.first().is_some_and(|s| s == "list" || s == "show"),
        "reflog" => rest.first().is_none_or(|s| s == "show" || s.starts_with('-')),
        "config" => has(&["--get", "--get-all", "--get-regexp", "-l", "--list"]) && !has(&["--add", "--unset", "--unset-all", "--replace-all", "--rename-section", "--remove-section", "-e", "--edit"]),
        _ => false,
    }
}

/// Options that make even a reading command write a file or start a program of the model's choosing.
fn writes_a_file(s: &str) -> bool {
    s.starts_with("--output") || s.starts_with("--open-files-in-pager") || s.starts_with("-O")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(a: &[&str]) -> bool {
        read_only(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn reading_commands_dont_ask_and_the_rest_do() {
        for a in [&["status", "--short"][..], &["diff", "--stat", "HEAD~3"], &["log", "--oneline", "-20"], &["show", "HEAD:src/main.rs"], &["blame", "-L", "1,20", "a.rs"], &["branch"], &["branch", "-av"], &["branch", "--list", "feat/*"], &["tag"], &["tag", "-l", "v1.*"], &["remote", "-v"], &["stash", "list"], &["reflog"], &["config", "--get", "user.name"]] {
            assert!(ro(a), "{a:?} should only read");
        }
        for a in [&["commit", "-m", "x"][..], &["checkout", "main"], &["reset", "--hard"], &["push"], &["fetch"], &["branch", "new-name"], &["branch", "-D", "old"], &["branch", "-rd", "origin/x"], &["tag", "v2"], &["tag", "-l", "-d", "v1"], &["remote", "add", "x", "url"], &["stash"], &["stash", "pop"], &["reflog", "expire"], &["config", "user.name", "me"], &["diff", "--output=/tmp/x"], &["grep", "-Ovim", "x"], &["st"]] {
            assert!(!ro(a), "{a:?} should ask");
        }
    }

    #[test]
    fn args_must_start_with_the_subcommand() {
        assert!(args(&json!({ "args": ["-C", "/elsewhere", "status"] })).is_err());
        assert!(args(&json!({ "args": [] })).is_err());
        assert!(args(&json!({ "args": "status" })).is_err());
        assert_eq!(Git.summary(&json!({ "args": ["commit", "-m", "fix it"] })), "git commit -m \"fix it\"");
        let ctx = ToolCtx::at(std::env::temp_dir());
        assert_eq!(Git.kind(&json!({ "args": ["-c", "x=y", "status"] }), &ctx), Kind::Exec);
    }

    #[tokio::test]
    async fn runs_git_in_the_project() {
        let dir = std::env::temp_dir().join(format!("devcat-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx::at(dir.clone());
        let outside = Git.run(json!({ "args": ["status"] }), &ctx).await;
        assert!(outside.is_error && outside.content.to_lowercase().contains("not a git repository"), "{outside:?}");
        assert!(!Git.run(json!({ "args": ["init", "-q"] }), &ctx).await.is_error);
        std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
        let status = Git.run(json!({ "args": ["status", "--short"] }), &ctx).await;
        assert_eq!((status.content.as_str(), status.is_error), ("?? a.txt", false));
        let _ = std::fs::remove_dir_all(dir);
    }
}
