//! Claude workspace: review. A one-shot, read-only run over a git diff that answers with
//! structured findings.
//!
//! Where the review lands in the stream: with `--json-schema`, the final
//! `{"type":"result","subtype":"success",…}` message carries the validated object in
//! `result.structured_output` ({summary, findings[]} per `REVIEW_SCHEMA`); `result.result`
//! holds the model's plain text. If structured output wasn't produced (older CLI, error
//! result, schema retries exhausted) `structured_output` is absent — fall back to
//! rendering `result.result` as markdown.
use super::locate;
use super::runner::{Sink, Spawn, spawn_run, user_message, write_to};
use super::*;
use crate::error::{AppError, AppResult};
use serde_json::{Value, json};

/// Inline the diff in the prompt below this size; above it Claude reads it with git itself.
const INLINE_DIFF_LIMIT: usize = 120 * 1024;
const MAX_TURNS: &str = "40";

pub(super) fn review_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["critical", "major", "minor", "nit"] },
                        "file": { "type": "string" },
                        "line": { "type": ["integer", "null"] },
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "suggestion": { "type": ["string", "null"] }
                    },
                    "required": ["severity", "file", "line", "title", "body", "suggestion"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["summary", "findings"],
        "additionalProperties": false
    })
}

const ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git status:*)",
    "Bash(git blame:*)",
    "Bash(git ls-files:*)",
];

/// Flags on top of the base run flags: read-only tools, no prompts, structured output.
/// `--strict-mcp-config` (without `--mcp-config`) and `--disable-slash-commands` keep the
/// user's MCP tool definitions and skills out of the prompt — on a heavily configured
/// machine that was ~180k tokens of context per review.
pub(super) fn review_args() -> Vec<String> {
    let mut a: Vec<String> = vec!["--strict-mcp-config".into(), "--disable-slash-commands".into()];
    a.extend(["--tools".into(), "Read,Grep,Glob,Bash".into(), "--allowedTools".into()]);
    a.push(ALLOWED_TOOLS.join(","));
    a.extend(["--max-turns".into(), MAX_TURNS.into(), "--json-schema".into(), review_schema().to_string()]);
    a
}

async fn git(cwd: &str, args: &[&str]) -> AppResult<String> {
    let mut cmd = locate::tool_command("git").await;
    cmd.args(args).current_dir(cwd).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = cmd.output().await.map_err(|e| AppError::msg(format!("git: {e}")))?;
    if !out.status.success() {
        return Err(AppError::msg(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn rev_exists(cwd: &str, rev: &str) -> bool {
    git(cwd, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).await.is_ok()
}

/// origin/HEAD's target, else main / master (local, then origin/).
async fn default_base(cwd: &str) -> AppResult<String> {
    if let Ok(r) = git(cwd, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]).await {
        let r = r.trim();
        if !r.is_empty() {
            return Ok(r.to_string());
        }
    }
    for c in ["main", "master", "origin/main", "origin/master"] {
        if rev_exists(cwd, c).await {
            return Ok(c.to_string());
        }
    }
    Err(AppError::msg("Could not determine the base branch; pass one explicitly"))
}

pub(super) struct Target {
    /// Human description, e.g. "uncommitted changes (vs HEAD)".
    pub label: String,
    /// The git command Claude can re-run to see the diff.
    pub diff_cmd: String,
    pub stat: String,
    pub diff: String,
    pub untracked: Vec<String>,
}

async fn collect(cwd: &str, scope: &str, base: Option<&str>) -> AppResult<Target> {
    git(cwd, &["rev-parse", "--show-toplevel"]).await.map_err(|_| AppError::msg(format!("Not a git repository: {cwd}")))?;
    let has_head = rev_exists(cwd, "HEAD").await;
    let (label, range): (String, Vec<String>) = match scope {
        "uncommitted" if has_head => ("uncommitted changes (working tree + index vs HEAD)".into(), vec!["HEAD".into()]),
        // No commits yet: everything staged is the change.
        "uncommitted" => ("uncommitted changes (no commits yet)".into(), vec!["--cached".into()]),
        "staged" => ("staged changes (index vs HEAD)".into(), vec!["--cached".into()]),
        "branch" => {
            let base = match base.map(str::trim).filter(|b| !b.is_empty()) {
                Some(b) => b.to_string(),
                None => default_base(cwd).await?,
            };
            if !rev_exists(cwd, &base).await {
                return Err(AppError::msg(format!("Unknown base revision: {base}")));
            }
            (format!("this branch's changes since it diverged from {base}"), vec![format!("{base}...HEAD")])
        }
        other => return Err(AppError::msg(format!("Unknown review scope: {other} (uncommitted | staged | branch)"))),
    };
    let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff"];
    args.extend(range.iter().map(String::as_str));
    let diff = git(cwd, &args).await?;
    let mut stat_args = args.clone();
    stat_args.insert(1, "--stat");
    let stat = git(cwd, &stat_args).await?;
    let untracked = if scope == "uncommitted" {
        git(cwd, &["ls-files", "--others", "--exclude-standard"]).await?.lines().map(str::to_string).collect()
    } else {
        Vec::new()
    };
    if diff.trim().is_empty() && untracked.is_empty() {
        return Err(AppError::msg(format!("Nothing to review: no {label}")));
    }
    Ok(Target { label, diff_cmd: format!("git {}", args.join(" ")), stat, diff, untracked })
}

pub(super) fn build_prompt(t: &Target) -> String {
    let mut p = format!(
        "You are reviewing {label} in this git repository. This is a read-only review: do not modify files.\n\n\
         Look for correctness bugs, security problems, data loss, race conditions, broken error handling, \
         performance regressions, and maintainability issues. Read surrounding code with Read/Grep/Glob when \
         the diff alone isn't enough to judge. Only report real, actionable issues; don't pad the list.\n\n\
         Answer with: `summary` (2–4 sentences on what the change does and its overall quality) and `findings`, \
         each with severity (critical | major | minor | nit), file (repo-relative path), line (line number in the \
         new version, or null), a short title, a body explaining the problem, and a concrete suggestion (or null).\n\n\
         Diffstat (`{cmd} --stat`):\n```\n{stat}```\n",
        label = t.label,
        cmd = t.diff_cmd,
        stat = t.stat,
    );
    if !t.diff.trim().is_empty() {
        if t.diff.len() <= INLINE_DIFF_LIMIT {
            p.push_str(&format!("\nFull diff (`{}`):\n```diff\n{}```\n", t.diff_cmd, t.diff));
        } else {
            p.push_str(&format!(
                "\nThe diff is large ({} KB), so it is not included. Inspect it file by file with `{} -- <path>`.\n",
                t.diff.len() / 1024,
                t.diff_cmd
            ));
        }
    }
    if !t.untracked.is_empty() {
        p.push_str("\nNew untracked files (not in the diff — read them):\n");
        for f in t.untracked.iter().take(200) {
            p.push_str(&format!("- {f}\n"));
        }
        if t.untracked.len() > 200 {
            p.push_str(&format!("- … and {} more\n", t.untracked.len() - 200));
        }
    }
    p
}

/// Start a one-shot code review run (read-only tools) of `scope`: "uncommitted" | "staged" | "branch" (vs `base`).
/// Streams like a chat run; the final `result` carries the review (structured findings when supported).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn claude_review(window: crate::events::Owner, cwd: String, scope: String, base: Option<String>, model: Option<String>) -> AppResult<String> {
    let target = collect(&cwd, &scope, base.as_deref()).await?;
    let spec = Spawn {
        // dontAsk: anything not in --allowedTools is denied without prompting.
        opts: StartOptions { model, permission_mode: Some("dontAsk".into()), ..Default::default() },
        prompt_tool: false,
        extra_args: review_args(),
        close_after_result: true,
        ..Default::default()
    };
    let run_id = start_with_prompt(crate::events::sink(), &cwd, spec, &build_prompt(&target)).await?;
    super::runner::set_owner(&run_id, window.label());
    Ok(run_id)
}

pub(super) async fn start_with_prompt(sink: Sink, cwd: &str, spec: Spawn, prompt: &str) -> AppResult<String> {
    let run_id = spawn_run(sink, cwd, spec).await?;
    write_to(&run_id, &user_message(prompt, &[])).await?;
    Ok(run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real review run (costs a few cents) over a tiny buggy change.
    #[tokio::test]
    #[ignore]
    async fn real_review_run() {
        use super::super::runner::tests::{channel_sink, wait_for};
        let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("devcat-review-real-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_str().unwrap();
        for args in [vec!["init", "-q", "-b", "main"], vec!["config", "user.email", "t@example.com"], vec!["config", "user.name", "t"]] {
            git(cwd, &args).await.unwrap();
        }
        std::fs::write(dir.join("calc.py"), "def average(xs):\n    return sum(xs) / len(xs)\n").unwrap();
        git(cwd, &["add", "."]).await.unwrap();
        git(cwd, &["commit", "-qm", "init"]).await.unwrap();
        std::fs::write(dir.join("calc.py"), "def average(xs):\n    return sum(xs) / (len(xs) - 1)\n").unwrap();

        let target = collect(cwd, "uncommitted", None).await.unwrap();
        let mut extra = review_args();
        extra.extend(["--max-budget-usd".into(), "0.10".into()]);
        let spec = Spawn {
            opts: StartOptions { model: Some("haiku".into()), permission_mode: Some("dontAsk".into()), ..Default::default() },
            prompt_tool: false,
            extra_args: extra,
            close_after_result: true,
            ..Default::default()
        };
        let (sink, mut rx) = channel_sink();
        let id = start_with_prompt(sink, cwd, spec, &build_prompt(&target)).await.unwrap();
        let res = wait_for(&mut rx, 180, |e, v| e == "claude-msg" && v["msg"]["type"] == "result").await;
        let m = &res["msg"];
        println!("subtype {} turns {} cost {}", m["subtype"], m["num_turns"], m["total_cost_usd"]);
        println!("permission_denials {}", m["permission_denials"]);
        println!("structured_output {}", serde_json::to_string_pretty(&m["structured_output"]).unwrap());
        println!("result text {}", m["result"]);
        let exit = wait_for(&mut rx, 15, |e, _| e == "claude-exit").await;
        println!("exit {exit} (run {id})");
        assert!(m["structured_output"]["findings"].is_array() || m["result"].is_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn args_are_read_only() {
        let a = review_args();
        let tools = &a[a.iter().position(|x| x == "--tools").unwrap() + 1];
        assert!(!tools.contains("Edit") && !tools.contains("Write"));
        let allowed = &a[a.iter().position(|x| x == "--allowedTools").unwrap() + 1];
        assert!(allowed.contains("Bash(git diff:*)") && !allowed.contains("Bash(*)"));
        let schema: Value = serde_json::from_str(&a[a.iter().position(|x| x == "--json-schema").unwrap() + 1]).unwrap();
        assert_eq!(schema["required"], json!(["summary", "findings"]));
    }

    #[test]
    fn prompt_inlines_small_diffs_only() {
        let mut t = Target {
            label: "staged changes".into(),
            diff_cmd: "git diff --cached".into(),
            stat: " a.rs | 1 +\n".into(),
            diff: "+fn x() {}\n".into(),
            untracked: vec!["new.rs".into()],
        };
        let p = build_prompt(&t);
        assert!(p.contains("```diff\n+fn x() {}") && p.contains("- new.rs"));
        t.diff = "x".repeat(INLINE_DIFF_LIMIT + 1);
        let p = build_prompt(&t);
        assert!(p.contains("not included") && !p.contains("```diff"));
    }

    #[tokio::test]
    async fn collect_scopes_in_temp_repo() {
        let dir = std::env::temp_dir().join(format!("devcat-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_str().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            git(cwd, &args).await.unwrap();
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(cwd, &["add", "a.txt"]).await.unwrap();
        git(cwd, &["commit", "-qm", "init"]).await.unwrap();

        assert!(collect(cwd, "uncommitted", None).await.is_err(), "clean tree has nothing to review");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let t = collect(cwd, "uncommitted", None).await.unwrap();
        assert!(t.diff.contains("+two") && t.untracked == vec!["b.txt".to_string()]);
        assert!(collect(cwd, "staged", None).await.is_err());

        git(cwd, &["checkout", "-qb", "feature"]).await.unwrap();
        git(cwd, &["commit", "-qam", "two"]).await.unwrap();
        let t = collect(cwd, "branch", None).await.unwrap();
        assert!(t.diff_cmd.ends_with("main...HEAD") && t.diff.contains("+two"));
        assert!(collect(cwd, "branch", Some("nope")).await.is_err());
        assert!(collect(cwd, "bogus", None).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
