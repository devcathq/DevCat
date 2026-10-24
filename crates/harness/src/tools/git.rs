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

pub struct Git;

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git".into(),
            description: "Run git in the project folder. `args` are the arguments after `git`, one per item: [\"status\", \"--short\"], [\"diff\", \"--stat\", \"HEAD~3\"], [\"log\", \"--oneline\", \"-20\"], [\"show\", \"HEAD:src/main.rs\"]. Commands that only read the repository (status, diff, log, show, blame, grep, ls-files, rev-parse, describe, shortlog, merge-base, reflog, and listing branches, tags, remotes, stashes or config), with their usual options spelled out, run without asking; anything else (commit, checkout, reset, merge, stash, fetch, pull, push, …) asks the user first. There's no pager, color or editor: give commit messages with -m. Prefer this over `shell` for git.".into(),
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

/// Whether `git <args>` only reads the repository: a reading subcommand with only options it lists
/// below. Git accepts option abbreviations (`--verb` for `--verbose`) and short options bunched
/// together (`-iO<cmd>`), so options aren't matched by their start: each must be one of these,
/// spelled out, and short ones are taken one letter at a time. Anything else asks.
fn read_only(a: &[String]) -> bool {
    let (sub, rest) = (a[0].as_str(), &a[1..]);
    let opts = |lists: &[&[&str]]| options(rest, lists);
    let list_mode = |pos: &[String]| rest.iter().any(|s| s == "-l" || s == "--list") || pos.is_empty();
    match sub {
        "status" => opts(&[STATUS]).is_some(),
        "diff" => opts(&[DIFF, &["--cached", "--staged", "--merge-base", "--base", "--ours", "--theirs", "-#"]]).is_some(),
        "log" | "show" => opts(&[DIFF, REVS]).is_some(),
        // its own first: shortlog's -n is --numbered, log's takes a count
        "shortlog" => opts(&[SHORTLOG, REVS]).is_some(),
        "blame" => opts(&[BLAME]).is_some(),
        "grep" => opts(&[GREP]).is_some(),
        "ls-files" => opts(&[LS_FILES]).is_some(),
        "rev-parse" => opts(&[REV_PARSE]).is_some(),
        "describe" => opts(&[DESCRIBE]).is_some(),
        "merge-base" => opts(&[&["--all", "-a", "--octopus", "--independent", "--is-ancestor", "--fork-point"]]).is_some(),
        // listing only: a name without --list would create a branch or tag
        "branch" => opts(&[BRANCH]).is_some_and(|pos| list_mode(&pos)),
        "tag" => opts(&[TAG]).is_some_and(|pos| list_mode(&pos) || rest.iter().any(|s| s.starts_with("-n"))),
        "remote" => rest.is_empty() || rest == ["-v"] || rest == ["--verbose"] || (rest.first().is_some_and(|s| s == "get-url") && options(&rest[1..], &[&["--push", "--all"]]).is_some()),
        "stash" => match rest.first().map(String::as_str) {
            Some("list") => options(&rest[1..], &[DIFF, REVS]).is_some(),
            Some("show") => options(&rest[1..], &[DIFF, &["-u", "--include-untracked", "--only-untracked"]]).is_some(),
            _ => false,
        },
        "reflog" => rest.is_empty() || (rest[0] == "show" && options(&rest[1..], &[DIFF, REVS]).is_some()),
        "config" => match rest.first().map(String::as_str) {
            // git 2.46's subcommands
            Some("get" | "list") => options(&rest[1..], &[CONFIG]).is_some(),
            _ => opts(&[CONFIG, &["--get", "--get-all", "--get-regexp", "--get-urlmatch", "-l", "--list"]]).is_some() && rest.iter().any(|s| ["--get", "--get-all", "--get-regexp", "--get-urlmatch", "-l", "--list"].contains(&s.as_str())),
        },
        _ => false,
    }
}

// The options reading commands may have (see [`options`] for how they're written). None of them
// writes a file, starts a program (--output, -O, --ext-diff, --textconv, --show-signature,
// --remerge-diff, --alternate-refs) or runs git inside submodules (--submodule,
// --ignore-submodules, --recurse-submodules, status -v, describe --dirty / --broken).

const DIFF: &[&str] = &[
    "-p", "-u", "-s", "-z", "-R", "-a", "-w", "-b", "-W", "-D", "-U?", "-M?", "-C?", "-B?", "-X?", "-G=", "-S=", "-l=",
    "--patch", "--no-patch", "--unified?", "--raw", "--patch-with-raw", "--patch-with-stat", "--stat?", "--stat-width=",
    "--stat-name-width=", "--stat-graph-width=", "--stat-count=", "--numstat", "--shortstat", "--dirstat?",
    "--cumulative", "--dirstat-by-file?", "--summary", "--compact-summary", "--name-only", "--name-status", "--check",
    "--full-index", "--binary", "--abbrev?", "--no-abbrev", "--break-rewrites?", "--find-renames?", "--find-copies?",
    "--find-copies-harder", "--no-renames", "--rename-empty", "--no-rename-empty", "--irreversible-delete",
    "--diff-filter=", "--find-object=", "--pickaxe-all", "--pickaxe-regex", "--skip-to=", "--rotate-to=", "--relative?",
    "--no-relative", "--text", "--ignore-cr-at-eol", "--ignore-space-at-eol", "--ignore-space-change",
    "--ignore-all-space", "--ignore-blank-lines", "--ignore-matching-lines=", "--inter-hunk-context=",
    "--function-context", "--exit-code", "--quiet", "--minimal", "--patience", "--histogram", "--anchored=",
    "--diff-algorithm=", "--indent-heuristic", "--no-indent-heuristic", "--word-diff?", "--word-diff-regex=",
    "--color-words?", "--color?", "--no-color", "--color-moved?", "--no-color-moved", "--color-moved-ws=",
    "--no-color-moved-ws", "--ws-error-highlight=", "--src-prefix=", "--dst-prefix=", "--no-prefix",
    "--default-prefix", "--line-prefix=", "--ita-invisible-in-index", "--ita-visible-in-index", "--no-ext-diff",
    "--no-textconv", "--output-indicator-new=", "--output-indicator-old=", "--output-indicator-context=",
];

const REVS: &[&str] = &[
    "-#", "-n=", "-i", "-E", "-F", "-P", "-g", "-m", "-c", "-t", "-L=", "--max-count=", "--skip=", "--since=", "--after=",
    "--until=", "--before=", "--since-as-filter=", "--max-age=", "--min-age=", "--author=", "--committer=", "--grep=",
    "--grep-reflog=", "--all-match", "--invert-grep", "--regexp-ignore-case", "--extended-regexp", "--fixed-strings",
    "--perl-regexp", "--basic-regexp", "--remove-empty", "--merges", "--no-merges", "--min-parents=", "--max-parents=",
    "--no-min-parents", "--no-max-parents", "--first-parent", "--exclude-first-parent-only", "--not", "--all",
    "--branches?", "--tags?", "--remotes?", "--glob=", "--exclude=", "--reflog", "--single-worktree", "--ignore-missing",
    "--bisect", "--cherry-mark", "--cherry-pick", "--left-only", "--right-only", "--cherry", "--walk-reflogs", "--merge",
    "--boundary", "--simplify-by-decoration", "--show-pulls", "--full-history", "--dense", "--sparse",
    "--simplify-merges", "--ancestry-path?", "--date-order", "--author-date-order", "--topo-order", "--reverse",
    "--no-walk?", "--do-walk", "--format=", "--pretty?", "--abbrev-commit", "--no-abbrev-commit", "--oneline",
    "--encoding=", "--expand-tabs?", "--no-expand-tabs", "--notes?", "--no-notes", "--show-notes?", "--standard-notes",
    "--no-standard-notes", "--show-linear-break?", "--relative-date", "--date=", "--parents", "--children",
    "--left-right", "--graph", "--no-graph", "--cc", "--dd", "--combined-all-paths", "--no-diff-merges", "--follow",
    "--decorate?", "--no-decorate", "--decorate-refs=", "--decorate-refs-exclude=", "--clear-decorations", "--source",
    "--mailmap", "--no-mailmap", "--use-mailmap", "--no-use-mailmap", "--full-diff", "--log-size", "--count",
];

/// shortlog's own (it takes the revision options too).
const SHORTLOG: &[&str] = &["-n", "-s", "-e", "-c", "-w?", "--numbered", "--summary", "--email", "--committer", "--group=", "--format="];

const STATUS: &[&str] = &[
    "-s", "-b", "-z", "-u?", "--short", "--branch", "--show-stash", "--porcelain?", "--long", "--untracked-files?",
    "--ignored?", "--column?", "--no-column", "--ahead-behind", "--no-ahead-behind", "--renames", "--no-renames",
    "--find-renames?", "--null",
];

const BLAME: &[&str] = &[
    "-b", "-c", "-e", "-f", "-l", "-n", "-p", "-s", "-t", "-w", "-L=", "-S=", "-M?", "-C?", "--root", "--show-stats",
    "--score-debug", "--show-name", "--show-number", "--show-email", "--porcelain", "--line-porcelain", "--incremental",
    "--encoding=", "--contents=", "--date=", "--no-progress", "--reverse", "--first-parent", "--abbrev?", "--ignore-rev=",
    "--ignore-revs-file=", "--color-lines", "--color-by-age", "--minimal", "--no-textconv", "--since=",
];

const GREP: &[&str] = &[
    "-#", "-a", "-i", "-I", "-w", "-v", "-h", "-H", "-E", "-G", "-F", "-P", "-n", "-l", "-L", "-c", "-o", "-q", "-z", "-p",
    "-W", "-e=", "-f=", "-A=", "-B=", "-C=", "-m=", "--cached", "--untracked", "--no-exclude-standard",
    "--exclude-standard", "--text", "--no-textconv", "--ignore-case", "--word-regexp", "--invert-match", "--full-name",
    "--extended-regexp", "--basic-regexp", "--perl-regexp", "--fixed-strings", "--line-number", "--column",
    "--files-with-matches", "--name-only", "--files-without-match", "--null", "--only-matching", "--count", "--color?",
    "--no-color", "--break", "--heading", "--show-function", "--function-context", "--context=", "--after-context=",
    "--before-context=", "--threads=", "--max-depth=", "--max-count=", "--and", "--or", "--not", "--all-match", "--quiet",
];

const LS_FILES: &[&str] = &[
    "-c", "-d", "-m", "-o", "-i", "-s", "-u", "-k", "-z", "-t", "-v", "-f", "-x=", "-X=", "--cached", "--deleted",
    "--modified", "--others", "--ignored", "--stage", "--directory", "--no-empty-directory", "--unmerged", "--killed",
    "--resolve-undo", "--eol", "--exclude=", "--exclude-from=", "--exclude-per-directory=", "--exclude-standard",
    "--error-unmatch", "--with-tree=", "--full-name", "--abbrev?", "--debug", "--deduplicate", "--sparse", "--format=",
];

const REV_PARSE: &[&str] = &[
    "-q", "--quiet", "--verify", "--short?", "--abbrev-ref?", "--symbolic", "--symbolic-full-name", "--all",
    "--branches?", "--tags?", "--remotes?", "--glob=", "--exclude=", "--disambiguate=", "--since=", "--after=",
    "--until=", "--before=", "--default=", "--revs-only", "--no-revs", "--flags", "--no-flags", "--not", "--sq",
    "--show-toplevel", "--show-prefix", "--show-cdup", "--show-superproject-working-tree", "--git-dir",
    "--git-common-dir", "--absolute-git-dir", "--is-inside-work-tree", "--is-inside-git-dir", "--is-bare-repository",
    "--is-shallow-repository", "--shared-index-path", "--local-env-vars", "--git-path=", "--path-format=",
    "--show-object-format?", "--show-ref-format",
];

const DESCRIBE: &[&str] = &[
    "--tags", "--all", "--long", "--abbrev?", "--always", "--contains", "--exact-match", "--first-parent", "--match=",
    "--exclude=", "--candidates=", "--debug",
];

const BRANCH: &[&str] = &[
    "-a", "-r", "-v", "-l", "-i", "--all", "--remotes", "--verbose", "--list", "--ignore-case", "--show-current",
    "--contains=", "--no-contains=", "--merged=", "--no-merged=", "--points-at=", "--sort=", "--format=", "--column?",
    "--no-column", "--abbrev?", "--no-abbrev", "--omit-empty",
];

const TAG: &[&str] = &[
    "-l", "-i", "-n?", "--list", "--ignore-case", "--contains=", "--no-contains=", "--merged=", "--no-merged=",
    "--points-at=", "--sort=", "--format=", "--column?", "--no-column", "--omit-empty",
];

const CONFIG: &[&str] = &[
    "-z", "--null", "--show-origin", "--show-scope", "--name-only", "--type=", "--bool", "--int", "--bool-or-int",
    "--path", "--expiry-date", "--local", "--global", "--system", "--worktree", "--default=", "--includes",
    "--no-includes", "--all", "--regexp", "--value=", "--fixed-value",
];

/// Check `args` against option lists, and return the other arguments (revisions, paths, names).
/// None if an option isn't listed. An entry is `-p` / `--stat` (a flag), `-n=` / `--format=` (a
/// value: attached, or the next argument), `-U?` / `--stat?` (a value only when attached: `-U5`,
/// `--stat=80`), or `-#` (`-5`, a count). Short options may be bunched (`-sb`); a value-taking one
/// takes the rest of the bunch. Long ones must be spelled out. After `--` there are only paths. A
/// value from the next argument can't start with `-`: where git reads that as an option after all,
/// it's checked as one here too.
fn options(args: &[String], lists: &[&[&str]]) -> Option<Vec<String>> {
    let kind = |opt: &str| {
        lists.iter().flat_map(|l| l.iter()).find_map(|e| match e.strip_suffix('=').or_else(|| e.strip_suffix('?')) {
            Some(name) if name == opt => Some(e.as_bytes()[e.len() - 1]),
            None if *e == opt => Some(b' '),
            _ => None,
        })
    };
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.extend(it.cloned());
            break;
        }
        if !a.starts_with('-') || a == "-" {
            rest.push(a.clone());
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            match kind(&format!("--{name}"))? {
                b' ' if value.is_some() => return None,
                b'=' if value.is_none() => {
                    it.next().filter(|v| !v.starts_with('-'))?;
                }
                _ => {}
            }
            continue;
        }
        let bunch = &a[1..];
        if bunch.chars().all(|c| c.is_ascii_digit()) {
            kind("-#")?;
            continue;
        }
        for (i, c) in bunch.char_indices() {
            match kind(&format!("-{c}"))? {
                b' ' => continue,
                // a value: the rest of the bunch, or the next argument
                b'=' if i + c.len_utf8() == bunch.len() => {
                    it.next().filter(|v| !v.starts_with('-'))?;
                }
                _ => {}
            }
            break;
        }
    }
    Some(rest)
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
        for a in [&["status", "--short"][..], &["status", "-sb"], &["status", "-uno", "--porcelain=v2"], &["diff", "--stat", "HEAD~3"], &["diff", "--cached", "-U5", "--", "src/a.rs"],
            &["log", "--oneline", "-20"], &["log", "-p", "-n", "3", "--format=%h %s", "--", "-weird-path"], &["show", "HEAD:src/main.rs"], &["show", "--stat", "HEAD"],
            &["blame", "-L", "1,20", "a.rs"], &["blame", "-wL10,+5", "a.rs"], &["grep", "-in", "-e", "TODO", "--", "src"], &["grep", "-C", "2", "fn main"],
            &["ls-files", "-mo", "--exclude-standard"], &["rev-parse", "--abbrev-ref", "HEAD"], &["describe", "--tags", "--always"], &["shortlog", "-sn", "--all"],
            &["merge-base", "--is-ancestor", "a", "b"], &["branch"], &["branch", "-av"], &["branch", "--list", "feat/*"], &["branch", "--contains", "HEAD"],
            &["tag"], &["tag", "-l", "v1.*"], &["tag", "-n5"], &["remote", "-v"], &["remote", "get-url", "origin"], &["stash", "list"], &["stash", "show", "-p", "stash@{1}"],
            &["reflog"], &["reflog", "show", "-5"], &["config", "--get", "user.name"], &["config", "--list", "--show-origin"], &["config", "get", "user.email"]] {
            assert!(ro(a), "{a:?} should only read");
        }
        for a in [&["commit", "-m", "x"][..], &["checkout", "main"], &["reset", "--hard"], &["push"], &["fetch"], &["branch", "new-name"], &["branch", "-D", "old"], &["branch", "-rd", "origin/x"], &["tag", "v2"], &["tag", "-l", "-d", "v1"], &["remote", "add", "x", "url"], &["stash"], &["stash", "pop"], &["reflog", "expire"], &["reflog", "-n", "5"], &["config", "user.name", "me"], &["diff", "--output=/tmp/x"], &["grep", "-Ovim", "x"], &["st"],
            // programs the repository's config names
            &["diff", "--ext-diff"], &["log", "-p", "--textconv"], &["grep", "--textconv", "x"], &["log", "--show-signature"], &["status", "-v"], &["status", "-sv"], &["status", "--verbose"],
            &["diff", "--submodule=diff"], &["status", "--ignore-submodules=none"],
            // bunched and shortened options (git takes both)
            &["grep", "-iOtouch /tmp/x; true", "hello"], &["grep", "--open=touch /tmp/x; true", "hello"], &["status", "--verb"],
            &["status", "--ignore-sub=none"], &["diff", "--ext"], &["log", "--show-sig"], &["describe", "--dirty"], &["describe", "--broken"],
            &["branch", "--set-u=origin/x"], &["branch", "--unset-u"], &["log", "--remerge-diff"], &["diff", "--no-index", "a", "b"],
            &["config", "--file", "x", "--list"], &["remote", "show", "origin"], &["branch", "--", "new"]] {
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

    /// The ways around a reading command's rules found in review (git 2.45): each must ask.
    #[test]
    fn options_git_reads_another_way_ask() {
        let ctx = ToolCtx { cwd: std::env::temp_dir() };
        for a in [
            json!(["grep", "-iOtouch /tmp/x; true", "hello"]),
            json!(["grep", "--open=touch /tmp/x; true", "hello"]),
            json!(["status", "--verb"]),
            json!(["status", "--ignore-sub=none"]),
            json!(["describe", "--dirty"]),
            json!(["describe", "--broken"]),
            json!(["branch", "--set-u=origin/main"]),
            json!(["branch", "--unset-u"]),
            // a value that's really an option
            json!(["log", "-n", "--output=/tmp/x"]),
            json!(["grep", "-e", "--open-files-in-pager=sh", "x"]),
        ] {
            assert_eq!(Git.kind(&json!({ "args": a }), &ctx), Kind::Exec, "{a}");
        }
        assert_eq!(Git.kind(&json!({ "args": ["status", "--short"] }), &ctx), Kind::Read);
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
