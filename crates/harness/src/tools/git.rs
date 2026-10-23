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
        // one that runs without asking mustn't start a program the repository's config names
        let a = if read_only(&a) {
            match harmless(&a, ctx).await {
                Ok(a) => a,
                Err(e) => return e,
            }
        } else {
            a
        };
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["--no-pager", "-c", "color.ui=false", "-c", "core.quotePath=false"]).args(&a);
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
        // -v shows the staged diff through textconv, and status has no --no-textconv
        "status" => !rest.iter().any(|s| s == "--verbose" || (s.starts_with('-') && !s.starts_with("--") && s.contains('v'))),
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
        "reflog" => rest.first().is_none_or(|s| s == "show"),
        "config" => has(&["--get", "--get-all", "--get-regexp", "-l", "--list"]) && !has(&["--add", "--unset", "--unset-all", "--replace-all", "--rename-section", "--remove-section", "-e", "--edit"]),
        _ => false,
    }
}

/// Options that make even a reading command write a file or start a program: of the model's
/// choosing, or one the repository's config or .gitattributes names (external diff, textconv,
/// gpg, git inside a submodule with that submodule's config).
fn writes_a_file(s: &str) -> bool {
    ["--output", "--open-files-in-pager", "-O", "--submodule", "--ignore-submodules"].iter().any(|p| s.starts_with(p)) || ["--ext-diff", "--textconv", "--show-signature"].contains(&s)
}

/// What the gpg settings point at for a reading command: no such program, so a signature shows as
/// unchecked instead of running whatever the config names.
const NO_GPG: &str = "devcat-agent-no-gpg";

/// The standard Git LFS filters, kept: without them `status` and `diff` see LFS files as changed.
const LFS: &[&str] = &["git-lfs clean -- %f", "git-lfs smudge -- %f", "git-lfs filter-process"];

/// `git <a>` (a reading command) made so that git starts no program the repository's config or
/// .gitattributes names: the fsmonitor hook (status, diff), external diff drivers and textconv
/// (diff, log, show, blame), clean / smudge filters (status and diff hash the work tree through
/// them), gpg (signatures in log and show), and git inside submodules, which reads their own
/// config. The config is only text an in-project edit can change, so it can't decide what runs
/// without asking. Fails rather than run when it can't make sure.
async fn harmless(a: &[String], ctx: &ToolCtx) -> Result<Vec<String>, ToolOutput> {
    let mut set = vec!["core.fsmonitor=false".to_string(), "log.showSignature=false".into(), "diff.submodule=short".into(), "status.submoduleSummary=false".into()];
    set.extend(["gpg.program", "gpg.ssh.program", "gpg.x509.program"].map(|k| format!("{k}={NO_GPG}")));
    for (key, value) in filters(ctx).await? {
        if LFS.contains(&value.as_str()) {
            continue;
        }
        // `-c` splits at the first `=`, so such a filter can't be turned off
        if key.contains('=') {
            return Err(ToolOutput::err(format!("The repository's config has a filter (`{key}`) that git would run for this, and it can't be turned off. Run it with `shell` instead, which asks the user.")));
        }
        // an empty command turns a filter off
        set.push(format!("{key}="));
    }
    let mut out: Vec<String> = set.into_iter().flat_map(|s| ["-c".to_string(), s]).collect();
    // the options go right after the subcommand, before any `--` and paths
    let at = if a[0] == "stash" { a.len().min(2) } else { 1 };
    out.extend(a[..at].iter().cloned());
    match a[0].as_str() {
        "diff" => out.extend(["--no-ext-diff", "--no-textconv", "--ignore-submodules=dirty"].map(String::from)),
        "log" | "show" | "stash" => out.extend(["--no-ext-diff", "--no-textconv"].map(String::from)),
        "status" => out.push("--ignore-submodules=dirty".into()),
        "blame" => out.push("--no-textconv".into()),
        _ => {}
    }
    out.extend(a[at..].iter().cloned());
    Ok(out)
}

/// The clean / smudge / process filter commands in the repository's config, with their values.
async fn filters(ctx: &ToolCtx) -> Result<Vec<(String, String)>, ToolOutput> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["config", "-z", "--get-regexp", r"^filter\..*\.(clean|smudge|process)$"]).current_dir(&ctx.cwd).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
        // 1: there are none
        Ok(Ok(out)) if out.status.success() || out.status.code() == Some(1) => out,
        Ok(Ok(out)) => return Err(ToolOutput::err(format!("Couldn't read the repository's config: {}", String::from_utf8_lossy(&out.stderr).trim()))),
        Ok(Err(e)) => return Err(ToolOutput::err(format!("Couldn't run git: {e}"))),
        Err(_) => return Err(ToolOutput::err("Reading the repository's config took too long.")),
    };
    // `-z`: "key\nvalue" entries, each ended by NUL
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter_map(|e| e.split_once('\n')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
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
        for a in [&["commit", "-m", "x"][..], &["checkout", "main"], &["reset", "--hard"], &["push"], &["fetch"], &["branch", "new-name"], &["branch", "-D", "old"], &["branch", "-rd", "origin/x"], &["tag", "v2"], &["tag", "-l", "-d", "v1"], &["remote", "add", "x", "url"], &["stash"], &["stash", "pop"], &["reflog", "expire"], &["reflog", "-n", "5"], &["config", "user.name", "me"], &["diff", "--output=/tmp/x"], &["grep", "-Ovim", "x"], &["st"],
            // programs the repository's config names
            &["diff", "--ext-diff"], &["log", "-p", "--textconv"], &["grep", "--textconv", "x"], &["log", "--show-signature"], &["status", "-v"], &["status", "-sv"], &["status", "--verbose"],
            &["diff", "--submodule=diff"], &["status", "--ignore-submodules=none"]] {
            assert!(!ro(a), "{a:?} should ask");
        }
    }

    #[test]
    fn args_must_start_with_the_subcommand() {
        assert!(args(&json!({ "args": ["-C", "/elsewhere", "status"] })).is_err());
        assert!(args(&json!({ "args": [] })).is_err());
        assert!(args(&json!({ "args": "status" })).is_err());
        assert_eq!(Git.summary(&json!({ "args": ["commit", "-m", "fix it"] })), "git commit -m \"fix it\"");
        let ctx = ToolCtx { cwd: std::env::temp_dir() };
        assert_eq!(Git.kind(&json!({ "args": ["-c", "x=y", "status"] }), &ctx), Kind::Exec);
    }

    #[tokio::test]
    async fn runs_git_in_the_project() {
        let dir = std::env::temp_dir().join(format!("devcat-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx { cwd: dir.clone() };
        let outside = Git.run(json!({ "args": ["status"] }), &ctx).await;
        assert!(outside.is_error && outside.content.to_lowercase().contains("not a git repository"), "{outside:?}");
        assert!(!Git.run(json!({ "args": ["init", "-q"] }), &ctx).await.is_error);
        std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
        let status = Git.run(json!({ "args": ["status", "--short"] }), &ctx).await;
        assert_eq!((status.content.as_str(), status.is_error), ("?? a.txt", false));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// git in `dir`, outside the tool.
    fn plain(dir: &std::path::Path, a: &[&str]) -> std::process::Output {
        std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).current_dir(dir).output().unwrap()
    }

    #[tokio::test]
    async fn reading_runs_no_program_the_repository_names() {
        let dir = std::env::temp_dir().join(format!("devcat-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        plain(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
        plain(&dir, &["add", "."]);
        plain(&dir, &["commit", "-qm", "a"]);
        // what an in-project edit of .git/config and .gitattributes can plant; each leaves a file
        for (key, value) in [
            ("core.fsmonitor", "echo ran > fsmonitor-ran; false #"),
            ("diff.external", "echo ran > external-ran; false #"),
            ("diff.evil.textconv", "echo ran > textconv-ran; cat"),
            ("filter.evil.clean", "echo ran > clean-ran; cat"),
        ] {
            plain(&dir, &["config", key, value]);
        }
        std::fs::write(dir.join(".gitattributes"), "*.txt diff=evil filter=evil\n").unwrap();
        std::fs::write(dir.join("a.txt"), "hi\nthere\n").unwrap();
        let ran = || ["fsmonitor", "external", "textconv", "clean"].into_iter().filter(|p| dir.join(format!("{p}-ran")).exists()).collect::<Vec<_>>();
        // they do run, outside the tool
        plain(&dir, &["status", "--short"]);
        plain(&dir, &["diff"]);
        plain(&dir, &["log", "-p", "-1", "--textconv"]);
        assert_eq!(ran(), ["fsmonitor", "external", "textconv", "clean"]);
        for p in ["fsmonitor", "external", "textconv", "clean"] {
            std::fs::remove_file(dir.join(format!("{p}-ran"))).unwrap();
        }

        let ctx = ToolCtx { cwd: dir.clone() };
        for a in [&["status", "--short"][..], &["diff"], &["diff", "--stat"], &["log", "-p", "-1"], &["show", "HEAD"], &["blame", "a.txt"], &["stash", "list"]] {
            let out = Git.run(json!({ "args": a }), &ctx).await;
            assert!(!out.is_error, "{a:?}: {out:?}");
        }
        let diff = Git.run(json!({ "args": ["diff"] }), &ctx).await;
        assert!(diff.content.contains("+there"), "{diff:?}");
        assert_eq!(ran(), Vec::<&str>::new(), "a reading command started a program the config names");

        // a filter `-c` can't name
        plain(&dir, &["config", "filter.a=b.clean", "echo ran > clean-ran; cat"]);
        let out = Git.run(json!({ "args": ["status"] }), &ctx).await;
        assert!(out.is_error && out.content.contains("filter.a=b.clean"), "{out:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
