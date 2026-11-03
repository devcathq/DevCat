//! Tauri commands for the Git tools view. Every user-supplied ref / name is validated
//! (no leading '-', resolved or checked with `check-ref-format`) before it reaches git.
use super::parse::*;
use super::*;
use crate::claude::FileVersions;
use crate::claude::git::{
    git_error, git_exec, git_ok, git_raw, has_head, invalidate_cache, read_blob, require_root, resolve_rel, GitOut, Repo, DIFF_CAP,
    VERSION_CAP,
};
use crate::error::{AppError, AppResult};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOG_DEFAULT: u32 = 200;
const LOG_MAX: u32 = 2000;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(180);

// ---------- helpers ----------

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Rejects empty values and anything git could parse as an option.
fn check_arg<'a>(what: &str, s: &'a str) -> AppResult<&'a str> {
    let s = s.trim();
    if s.is_empty() {
        return Err(AppError::msg(format!("Empty {what}")));
    }
    if s.starts_with('-') || s.contains(['\0', '\n', '\r']) {
        return Err(AppError::msg(format!("Invalid {what}: {s}")));
    }
    Ok(s)
}

/// Full sha of the commit `rev` points at.
async fn resolve_commit(root: &Repo, rev: &str) -> AppResult<String> {
    let rev = check_arg("revision", rev)?;
    let out = git_raw(root, ["rev-parse", "--verify", "-q", "--end-of-options", &format!("{rev}^{{commit}}")]).await?;
    if out.code != 0 {
        return Err(AppError::msg(format!("Unknown revision: {rev}")));
    }
    Ok(text(&out.stdout).trim().to_string())
}

async fn ref_exists(root: &Repo, full_ref: &str) -> AppResult<bool> {
    Ok(git_raw(root, ["show-ref", "--verify", "-q", full_ref]).await?.code == 0)
}

async fn check_branch_name(root: &Repo, name: &str) -> AppResult<String> {
    let name = check_arg("branch name", name)?;
    let out = git_raw(root, ["check-ref-format", "--branch", name]).await?;
    // `--branch` expands @{-N}; require the name to be literal
    if out.code != 0 || text(&out.stdout).trim() != name {
        return Err(AppError::msg(format!("Invalid branch name: {name}")));
    }
    Ok(name.to_string())
}

async fn check_tag_name(root: &Repo, name: &str) -> AppResult<String> {
    let name = check_arg("tag name", name)?;
    if git_raw(root, ["check-ref-format", &format!("refs/tags/{name}")]).await?.code != 0 {
        return Err(AppError::msg(format!("Invalid tag name: {name}")));
    }
    Ok(name.to_string())
}

async fn remote_names(root: &Repo) -> AppResult<Vec<String>> {
    Ok(text(&git_ok(root, ["remote"]).await?).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Splits "origin/feat/x" into ("origin", "feat/x") using the configured remotes (names may contain '/').
async fn split_remote_branch(root: &Repo, name: &str) -> AppResult<Option<(String, String)>> {
    let mut remotes = remote_names(root).await?;
    remotes.sort_by_key(|r| std::cmp::Reverse(r.len()));
    Ok(remotes.into_iter().find_map(|r| {
        let rest = name.strip_prefix(&r)?.strip_prefix('/')?;
        (!rest.is_empty()).then(|| (r.clone(), rest.to_string()))
    }))
}

async fn check_remote(root: &Repo, remote: &str) -> AppResult<String> {
    let remote = check_arg("remote", remote)?;
    if !remote_names(root).await?.iter().any(|r| r == remote) {
        return Err(AppError::msg(format!("Unknown remote: {remote}")));
    }
    Ok(remote.to_string())
}

/// git's combined stdout + stderr, hint lines dropped.
fn combined(out: &GitOut) -> String {
    let mut s = text(&out.stdout);
    if !out.stderr.trim().is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&out.stderr);
    }
    s.lines().filter(|l| !l.trim_start().starts_with("hint:")).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Runs a mutating git command (never opens an editor); returns combined output, error text on failure.
async fn run<I, S>(root: &Repo, args: I, timeout: Option<Duration>, extra_env: &[(&str, String)]) -> AppResult<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    invalidate_cache();
    let mut env = vec![("GIT_EDITOR", "true".to_string()), ("GIT_MERGE_AUTOEDIT", "no".to_string())];
    env.extend(extra_env.iter().cloned());
    let out = git_exec(root, args, &env, timeout).await?;
    let msg = combined(&out);
    if out.code != 0 {
        return Err(if msg.is_empty() { git_error(&out) } else { AppError::msg(msg) });
    }
    Ok(msg)
}

async fn run_local<I, S>(root: &Repo, args: I) -> AppResult<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(root, args, None, &[]).await
}

/// Network ops: batch-mode SSH (unless the user configured their own) and an overall timeout.
async fn run_network(root: &Repo, args: Vec<String>) -> AppResult<String> {
    let mut env = Vec::new();
    // (on an SSH host git runs without this computer's environment)
    let own_ssh = matches!(root, Repo::Local(_)) && (std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some());
    if !own_ssh {
        let configured = git_raw(root, ["config", "--get", "core.sshCommand"]).await?;
        if configured.code != 0 || text(&configured.stdout).trim().is_empty() {
            env.push(("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=15".to_string()));
        }
    }
    run(root, args, Some(NETWORK_TIMEOUT), &env).await
}

/// The operation in progress in the repository, if any (see `operation`).
async fn operation_of(root: &Repo) -> AppResult<Option<String>> {
    match root {
        Repo::Local(dir) => {
            let p = PathBuf::from(text(&git_ok(root, ["rev-parse", "--git-dir"]).await?).trim());
            let git_dir = if p.is_absolute() { p } else { dir.join(p) };
            Ok(operation(&git_dir).map(String::from))
        }
        Repo::Remote(t) => crate::claude::remote::git_operation(t).await,
    }
}

/// In-progress operation: "merge" | "rebase" | "am" | "cherry-pick" | "revert".
fn operation(git_dir: &Path) -> Option<&'static str> {
    if git_dir.join("rebase-merge").is_dir() {
        Some("rebase")
    } else if git_dir.join("rebase-apply").is_dir() {
        Some(if git_dir.join("rebase-apply/applying").exists() { "am" } else { "rebase" })
    } else if git_dir.join("MERGE_HEAD").is_file() {
        Some("merge")
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        Some("cherry-pick")
    } else if git_dir.join("REVERT_HEAD").is_file() {
        Some("revert")
    } else {
        None
    }
}

async fn parents_of(root: &Repo, sha: &str) -> AppResult<Vec<String>> {
    let out = text(&git_ok(root, ["rev-list", "--parents", "-n", "1", sha]).await?);
    Ok(out.split_whitespace().skip(1).map(String::from).collect())
}

fn is_hex_prefix(s: &str) -> bool {
    s.len() >= 4 && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------- discovery ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_repos(root: String, max_depth: Option<u32>) -> AppResult<Vec<GitRepo>> {
    if let Some(t) = crate::claude::remote::Target::parse(&root) {
        return scan::scan_remote(&t, max_depth.unwrap_or(scan::DEFAULT_DEPTH)).await;
    }
    scan::scan(Path::new(&root), max_depth.unwrap_or(scan::DEFAULT_DEPTH)).await
}

// ---------- history ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_log(cwd: String, query: GitLogQuery) -> AppResult<Vec<GitCommit>> {
    let root = require_root(&cwd).await?;
    let limit = query.limit.unwrap_or(LOG_DEFAULT).clamp(1, LOG_MAX);
    let mut args: Vec<String> =
        vec!["log".into(), LOG_FORMAT.into(), "--decorate=full".into(), "--date-order".into(), format!("--skip={}", query.skip), format!("-n{limit}")];
    if let Some(a) = query.author.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        args.push(format!("--author={a}"));
    }
    let search = query.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = search {
        args.extend(["-i".into(), "-F".into(), format!("--grep={s}")]);
    }
    let rev = match query.rev.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) if !query.all => Some(check_arg("revision", r)?.to_string()),
        _ => None,
    };
    if query.all {
        args.extend(["--exclude=refs/stash".into(), "--all".into()]);
    } else if rev.is_none() && !has_head(&root).await? {
        return Ok(vec![]);
    }
    let path = match query.path.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(resolve_rel(&root, p)?),
        None => None,
    };
    let is_file = match path.as_deref() {
        Some(p) if p != "." => root.is_file(p).await?,
        _ => false,
    };
    if is_file {
        args.push("--follow".into());
    }
    args.push("--end-of-options".into());
    if let Some(r) = &rev {
        args.push(r.clone());
    }
    args.push("--".into());
    if let Some(p) = path {
        args.push(p);
    }
    let out = git_raw(&root, &args).await?;
    if out.code != 0 {
        // --all in a repo without any commits
        if query.all && !has_head(&root).await? && out.stderr.contains("does not have any commits") {
            return Ok(vec![]);
        }
        return Err(git_error(&out));
    }
    let mut commits = parse_log(&text(&out.stdout));
    if let Some(s) = search.filter(|s| is_hex_prefix(s) && query.skip == 0)
        && let Ok(sha) = resolve_commit(&root, s).await
        && !commits.iter().any(|c| c.sha == sha)
    {
        let one = git_ok(&root, ["log", "-1", LOG_FORMAT, "--decorate=full", &sha, "--"]).await?;
        let mut found = parse_log(&text(&one));
        found.append(&mut commits);
        commits = found;
    }
    Ok(commits)
}

/// Files changed by `sha` vs its first parent (vs the empty tree for root commits).
async fn commit_files(root: &Repo, sha: &str, parents: &[String]) -> AppResult<Vec<GitCommitFile>> {
    let base: Vec<String> = match parents.first() {
        Some(p) => vec![p.clone(), sha.to_string()],
        None => vec!["--root".into(), sha.to_string()],
    };
    let mut ns = vec!["diff-tree".to_string(), "-r".into(), "-M".into(), "--no-commit-id".into(), "--name-status".into(), "-z".into()];
    ns.extend(base.iter().cloned());
    let mut num = vec!["diff-tree".to_string(), "-r".into(), "-M".into(), "--no-commit-id".into(), "--numstat".into(), "-z".into()];
    num.extend(base);
    let (ns, num) = tokio::try_join!(git_ok(root, &ns), git_ok(root, &num))?;
    let mut files = parse_name_status_z(&ns);
    merge_numstat(&mut files, &parse_numstat_z(&num));
    Ok(files)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_show(cwd: String, sha: String) -> AppResult<GitCommitDetail> {
    let root = require_root(&cwd).await?;
    let sha = resolve_commit(&root, &sha).await?;
    let out = git_ok(&root, ["show", "-s", "--decorate=full", DETAIL_FORMAT, &sha, "--"]).await?;
    let mut detail = parse_detail(&text(&out)).ok_or_else(|| AppError::msg("Unexpected git show output"))?;
    detail.files = commit_files(&root, &sha, &detail.commit.parents).await?;
    Ok(detail)
}

fn to_versions(original: Vec<u8>, modified: Vec<u8>) -> FileVersions {
    if original.contains(&0) || modified.contains(&0) {
        return FileVersions { original: String::new(), modified: String::new(), binary: true };
    }
    FileVersions { original: text(&original), modified: text(&modified), binary: false }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_commit_file_versions(cwd: String, sha: String, path: String, old_path: Option<String>) -> AppResult<FileVersions> {
    let root = require_root(&cwd).await?;
    let sha = resolve_commit(&root, &sha).await?;
    let rel = resolve_rel(&root, &path)?;
    let old_rel = match old_path.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => resolve_rel(&root, p)?,
        None => rel.clone(),
    };
    let original = match parents_of(&root, &sha).await?.first() {
        Some(parent) => read_blob(&root, &format!("{parent}:{old_rel}")).await?,
        None => None,
    };
    let modified = read_blob(&root, &format!("{sha}:{rel}")).await?;
    Ok(to_versions(original.unwrap_or_default(), modified.unwrap_or_default()))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_commit_diff(cwd: String, sha: String) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let sha = resolve_commit(&root, &sha).await?;
    let parents = parents_of(&root, &sha).await?;
    let mut out = match parents.first() {
        Some(p) if parents.len() > 1 => git_ok(&root, ["diff", "-M", "--no-color", "--no-ext-diff", p.as_str(), &sha, "--"]).await?,
        _ => git_ok(&root, ["show", "--format=", "--patch", "-M", "--no-color", "--no-ext-diff", &sha, "--"]).await?,
    };
    if out.len() > DIFF_CAP {
        out.truncate(DIFF_CAP);
        out.extend_from_slice(b"\n# diff truncated\n");
    }
    Ok(text(&out))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_blame(cwd: String, path: String, rev: Option<String>) -> AppResult<GitBlame> {
    let root = require_root(&cwd).await?;
    let rel = resolve_rel(&root, &path)?;
    if rel == "." {
        return Err(AppError::msg("Not a file"));
    }
    let rev = match rev.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => Some(resolve_commit(&root, r).await?),
        None => None,
    };
    let raw = match &rev {
        Some(sha) => read_blob(&root, &format!("{sha}:{rel}")).await?.ok_or_else(|| AppError::msg(format!("{rel} does not exist at {sha}")))?,
        None => match &root {
            Repo::Local(dir) => {
                let p = dir.join(&rel);
                let meta = std::fs::metadata(&p).map_err(|e| AppError::msg(format!("Cannot read {rel}: {e}")))?;
                if !meta.is_file() {
                    return Err(AppError::msg("Not a file"));
                }
                if meta.len() > VERSION_CAP {
                    return Err(AppError::msg(format!("File too large to blame ({} MB)", meta.len() / (1024 * 1024))));
                }
                std::fs::read(&p)?
            }
            Repo::Remote(_) => root
                .read_worktree(&rel, VERSION_CAP)
                .await
                .map_err(|e| AppError::msg(e.to_string().replace("to diff", "to blame")))?
                .ok_or_else(|| AppError::msg("Not a file"))?,
        },
    };
    if raw.contains(&0) {
        return Err(AppError::msg("Cannot blame a binary file"));
    }
    let mut args = vec!["blame", "--porcelain"];
    if let Some(sha) = &rev {
        args.push(sha);
    }
    args.extend(["--", rel.as_str()]);
    let out = git_ok(&root, args).await?;
    Ok(parse_blame(&text(&out)))
}

// ---------- refs ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_refs(cwd: String) -> AppResult<GitRefs> {
    let root = require_root(&cwd).await?;
    let (refs, remotes, current, head, dir) = tokio::join!(
        git_ok(&root, ["for-each-ref", "--sort=-creatordate", REF_FORMAT, "refs/heads", "refs/remotes", "refs/tags"]),
        git_ok(&root, ["remote", "-v"]),
        git_raw(&root, ["symbolic-ref", "-q", "--short", "HEAD"]),
        git_raw(&root, ["rev-parse", "--verify", "-q", "HEAD"]),
        operation_of(&root),
    );
    let (mut local, mut remote, mut tags) = (vec![], vec![], vec![]);
    for line in text(&refs?).lines() {
        match parse_ref_line(line) {
            Some(RefLine::Local(b)) => local.push(b),
            Some(RefLine::Remote(b)) => remote.push(b),
            Some(RefLine::Tag(t)) => tags.push(t),
            None => {}
        }
    }
    let mut remote_list: Vec<GitRemote> = Vec::new();
    for line in text(&remotes?).lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url), Some("(fetch)")) = (parts.next(), parts.next(), parts.next()) else { continue };
        remote_list.push(GitRemote { name: name.into(), url: url.into() });
    }
    let current = current.ok().filter(|o| o.code == 0).map(|o| text(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
    let head_sha = head.ok().filter(|o| o.code == 0).map(|o| text(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
    Ok(GitRefs {
        current,
        head_sha,
        local,
        remote,
        tags,
        remotes: remote_list,
        operation: dir.ok().flatten(),
    })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_checkout(cwd: String, target: String, create: bool, start_point: Option<String>) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    if create {
        let name = check_branch_name(&root, &target).await?;
        let mut args = vec!["checkout".to_string(), "-b".into(), name];
        if let Some(sp) = start_point.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            resolve_commit(&root, sp).await?;
            args.push(sp.to_string());
        }
        run_local(&root, args).await?;
        return Ok(());
    }
    let target = check_arg("branch", &target)?;
    if ref_exists(&root, &format!("refs/heads/{target}")).await? {
        run_local(&root, ["checkout", target, "--"]).await?;
    } else if ref_exists(&root, &format!("refs/remotes/{target}")).await?
        && let Some((_, branch)) = split_remote_branch(&root, target).await?
    {
        if ref_exists(&root, &format!("refs/heads/{branch}")).await? {
            run_local(&root, ["checkout", branch.as_str(), "--"]).await?;
        } else {
            let branch = check_branch_name(&root, &branch).await?;
            run_local(&root, ["checkout", "-b", branch.as_str(), "--track", target]).await?;
        }
    } else {
        let sha = resolve_commit(&root, target).await?;
        run_local(&root, ["checkout", "--detach", sha.as_str()]).await?;
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_branch_delete(cwd: String, name: String, force: bool, remote: bool) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let name = check_arg("branch", &name)?;
    if remote {
        let (remote, branch) =
            split_remote_branch(&root, name).await?.ok_or_else(|| AppError::msg(format!("Not a remote branch: {name}")))?;
        let branch = check_arg("branch", &branch)?.to_string();
        run_network(&root, vec!["push".into(), remote, "--delete".into(), branch]).await?;
    } else {
        run_local(&root, ["branch", if force { "-D" } else { "-d" }, name]).await?;
    }
    Ok(())
}

/// Create a branch at `start_point` (default HEAD) without switching to it.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_branch_create(cwd: String, name: String, start_point: Option<String>) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let name = check_branch_name(&root, &name).await?;
    let start = match start_point.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(sp) => resolve_commit(&root, check_arg("start point", sp)?).await?,
        None => resolve_commit(&root, "HEAD").await?,
    };
    run_local(&root, ["branch", name.as_str(), start.as_str()]).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_branch_rename(cwd: String, from: String, to: String) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let from = check_arg("branch", &from)?;
    let to = check_branch_name(&root, &to).await?;
    if !ref_exists(&root, &format!("refs/heads/{from}")).await? {
        return Err(AppError::msg(format!("No such branch: {from}")));
    }
    run_local(&root, ["branch", "-m", from, to.as_str()]).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_merge(cwd: String, branch: String, no_ff: bool) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let branch = check_arg("branch", &branch)?;
    resolve_commit(&root, branch).await?;
    let mut args = vec!["merge", "--no-edit"];
    if no_ff {
        args.push("--no-ff");
    }
    args.push(branch);
    run_local(&root, args).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_abort(cwd: String) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let op = operation_of(&root).await?.ok_or_else(|| AppError::msg("No merge, rebase, cherry-pick or revert in progress"))?;
    // only the operations `operation` names (what a host printed becomes a git subcommand)
    let op = ["rebase", "am", "merge", "cherry-pick", "revert"].into_iter().find(|o| *o == op).ok_or_else(|| AppError::msg(format!("Unknown operation: {op}")))?;
    run_local(&root, [op, "--abort"]).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_tag_create(cwd: String, name: String, sha: Option<String>, message: Option<String>) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let name = check_tag_name(&root, &name).await?;
    let mut args = vec!["tag".to_string()];
    if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
        args.extend(["-a".into(), "-m".into(), m]);
    }
    args.push(name);
    if let Some(s) = sha.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        args.push(resolve_commit(&root, s).await?);
    }
    run_local(&root, args).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_tag_delete(cwd: String, name: String) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let name = check_arg("tag", &name)?;
    run_local(&root, ["tag", "-d", name]).await?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_commit_action(cwd: String, sha: String, action: CommitAction) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let sha = resolve_commit(&root, &sha).await?;
    let merge = parents_of(&root, &sha).await?.len() > 1;
    let mut args: Vec<&str> = match action {
        CommitAction::Checkout => vec!["checkout", "--detach"],
        CommitAction::CherryPick => vec!["cherry-pick"],
        CommitAction::Revert => vec!["revert", "--no-edit"],
        CommitAction::ResetSoft => vec!["reset", "--soft"],
        CommitAction::ResetMixed => vec!["reset", "--mixed"],
        CommitAction::ResetHard => vec!["reset", "--hard"],
    };
    if merge && matches!(action, CommitAction::CherryPick | CommitAction::Revert) {
        args.extend(["-m", "1"]);
    }
    args.push(&sha);
    run_local(&root, args).await
}

// ---------- stash ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_stash_list(cwd: String) -> AppResult<Vec<GitStash>> {
    let root = require_root(&cwd).await?;
    if !ref_exists(&root, "refs/stash").await? {
        return Ok(vec![]);
    }
    Ok(parse_stash(&text(&git_ok(&root, ["stash", "list", STASH_FORMAT]).await?)))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_stash_save(cwd: String, message: Option<String>, include_untracked: bool) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    let before = git_raw(&root, ["rev-parse", "-q", "--verify", "refs/stash"]).await?;
    let mut args = vec!["stash".to_string(), "push".into()];
    if include_untracked {
        args.push("--include-untracked".into());
    }
    if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
        args.extend(["-m".into(), m]);
    }
    let out = run_local(&root, args).await?;
    let after = git_raw(&root, ["rev-parse", "-q", "--verify", "refs/stash"]).await?;
    if after.code != 0 || after.stdout == before.stdout {
        return Err(AppError::msg(if out.is_empty() { "No local changes to save".to_string() } else { out }));
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_stash_apply(cwd: String, index: u32, pop: bool) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    run_local(&root, ["stash", if pop { "pop" } else { "apply" }, &format!("stash@{{{index}}}")]).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_stash_drop(cwd: String, index: u32) -> AppResult<()> {
    let root = require_root(&cwd).await?;
    run_local(&root, ["stash", "drop", &format!("stash@{{{index}}}")]).await?;
    Ok(())
}

// ---------- remotes ----------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_fetch(cwd: String, opts: RemoteOpts) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let mut args = vec!["fetch".to_string(), "--prune".into()];
    match opts.remote.as_deref().filter(|r| !r.trim().is_empty()) {
        Some(r) => args.push(check_remote(&root, r).await?),
        None => args.push("--all".into()),
    }
    run_network(&root, args).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_pull(cwd: String, opts: RemoteOpts) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let mut args = vec!["pull".to_string(), if opts.rebase { "--rebase" } else { "--no-rebase" }.into()];
    if let Some(r) = opts.remote.as_deref().filter(|r| !r.trim().is_empty()) {
        args.push(check_remote(&root, r).await?);
    }
    run_network(&root, args).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn git_push(cwd: String, opts: RemoteOpts) -> AppResult<String> {
    let root = require_root(&cwd).await?;
    let mut args = vec!["push".to_string()];
    if opts.force {
        args.push("--force-with-lease".into());
    }
    if opts.tags {
        args.push("--follow-tags".into());
    }
    let remote = match opts.remote.as_deref().filter(|r| !r.trim().is_empty()) {
        Some(r) => Some(check_remote(&root, r).await?),
        None => None,
    };
    let has_upstream = git_raw(&root, ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).await?.code == 0;
    if opts.set_upstream && !has_upstream {
        let remote = match remote {
            Some(r) => r,
            None => {
                let names = remote_names(&root).await?;
                names.iter().find(|r| *r == "origin").or(names.first()).cloned().ok_or_else(|| AppError::msg("No remote configured"))?
            }
        };
        args.extend(["-u".into(), remote, "HEAD".into()]);
    } else if let Some(r) = remote {
        args.extend([r, "HEAD".into()]);
    }
    run_network(&root, args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::git::tests::{OverSsh, TempRepo};

    fn commit(repo: &TempRepo, file: &str, content: &str, msg: &str) -> String {
        repo.write(file, content);
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", msg]);
        repo.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn by_rel<'a>(repos: &'a [GitRepo], rel: &str) -> &'a GitRepo {
        repos.iter().find(|r| r.rel == rel).unwrap_or_else(|| panic!("no repo {rel} in {repos:#?}"))
    }

    #[tokio::test]
    async fn discovers_nested_repos() {
        discovers_nested_repos_body().await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovers_nested_repos_over_ssh() {
        let _ssh = OverSsh::on();
        discovers_nested_repos_body().await
    }

    async fn discovers_nested_repos_body() {
        let top = TempRepo::new("scan");
        commit(&top, "readme.md", "top\n", "init");
        top.write("dirty.txt", "x\n");
        // nested repo two levels down, with its own nested repo
        let sub = top.dir.join("apps/web");
        std::fs::create_dir_all(&sub).unwrap();
        top.git(&["-C", sub.to_str().unwrap(), "init", "-q", "-b", "dev"]);
        let inner = sub.join("libs/inner");
        std::fs::create_dir_all(&inner).unwrap();
        top.git(&["-C", inner.to_str().unwrap(), "init", "-q"]);
        // submodule-like `.git` file pointing at a real git dir
        let gitdir = top.dir.join("apps/web/.git");
        let modlike = top.dir.join("mod");
        std::fs::create_dir_all(&modlike).unwrap();
        std::fs::write(modlike.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        // skipped: node_modules, hidden dirs
        for skipped in ["node_modules/pkg", ".cache/thing"] {
            let d = top.dir.join(skipped);
            std::fs::create_dir_all(&d).unwrap();
            top.git(&["-C", d.to_str().unwrap(), "init", "-q"]);
        }

        let repos = git_repos(format!("{}/", top.cwd()), None).await.unwrap();
        let rels: Vec<&str> = repos.iter().map(|r| r.rel.as_str()).collect();
        assert_eq!(rels, vec![".", "apps/web", "apps/web/libs/inner", "mod"], "{repos:#?}");
        let root = by_rel(&repos, ".");
        assert_eq!(root.parent, None);
        assert_eq!(root.branch.as_deref(), Some("main"));
        assert!(root.head_sha.is_some());
        assert!(root.changes >= 2, "dirty.txt + untracked nested dirs: {root:?}");
        let web = by_rel(&repos, "apps/web");
        assert_eq!(web.parent.as_deref(), Some(top.cwd().as_str()));
        assert_eq!(web.branch.as_deref(), Some("dev"));
        assert_eq!(web.head_sha, None);
        assert!(!web.is_submodule);
        assert_eq!(by_rel(&repos, "apps/web/libs/inner").parent.as_deref(), Some(web.path.as_str()));
        let m = by_rel(&repos, "mod");
        assert!(m.is_submodule);
        assert_eq!(m.name, "mod");

        // depth limit
        let shallow = git_repos(top.cwd(), Some(1)).await.unwrap();
        assert_eq!(shallow.iter().map(|r| r.rel.as_str()).collect::<Vec<_>>(), vec![".", "mod"]);

        // scanning from inside a repo reports the enclosing repo
        let deep = top.dir.join("docs/guide");
        std::fs::create_dir_all(&deep).unwrap();
        let inside = git_repos(format!("{}/docs/guide", top.cwd()), None).await.unwrap();
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].rel, "../..");
        assert_eq!(inside[0].path, top.cwd());

        // a broken submodule reports an error instead of failing the scan
        let broken = top.dir.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(".git"), "gitdir: /nonexistent/devcat\n").unwrap();
        let repos = git_repos(top.cwd(), None).await.unwrap();
        assert!(by_rel(&repos, "broken").error.is_some());
        assert!(git_repos(format!("{}/missing", top.cwd()), None).await.is_err());
    }

    #[tokio::test]
    async fn history_show_blame() {
        history_show_blame_body().await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn history_show_blame_over_ssh() {
        let _ssh = OverSsh::on();
        history_show_blame_body().await
    }

    async fn history_show_blame_body() {
        let repo = TempRepo::new("hist");
        let empty = git_log(repo.cwd(), GitLogQuery::default()).await.unwrap();
        assert!(empty.is_empty());
        assert!(git_log(repo.cwd(), GitLogQuery { all: true, ..Default::default() }).await.unwrap().is_empty());

        let c1 = commit(&repo, "a.txt", "one\ntwo\n", "first commit");
        repo.write("bin.dat", "\0\x01");
        let c2 = commit(&repo, "a.txt", "one\nTWO\nthree\n", "second: Fix bug");
        repo.git(&["mv", "a.txt", "b.txt"]);
        let c3 = commit(&repo, "other.txt", "o\n", "rename a to b");
        repo.git(&["tag", "v1"]);

        let log = git_log(repo.cwd(), GitLogQuery::default()).await.unwrap();
        assert_eq!(log.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), vec![c3.as_str(), c2.as_str(), c1.as_str()]);
        assert!(log[0].refs.iter().any(|r| r.name == "main" && r.kind == "head"));
        assert!(log[0].refs.iter().any(|r| r.name == "v1" && r.kind == "tag"));
        assert_eq!(log[1].parents, vec![c1.clone()]);
        assert_eq!(log[2].author, "DevCat Test");

        // path + follow, search, sha search, paging
        let follow = git_log(repo.cwd(), GitLogQuery { path: Some("b.txt".into()), ..Default::default() }).await.unwrap();
        assert_eq!(follow.len(), 3);
        let found = git_log(repo.cwd(), GitLogQuery { search: Some("fix BUG".into()), ..Default::default() }).await.unwrap();
        assert_eq!(found.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), vec![c2.as_str()]);
        let by_sha = git_log(repo.cwd(), GitLogQuery { search: Some(c1[..8].to_string()), ..Default::default() }).await.unwrap();
        assert_eq!(by_sha[0].sha, c1);
        let page = git_log(repo.cwd(), GitLogQuery { skip: 1, limit: Some(1), ..Default::default() }).await.unwrap();
        assert_eq!(page[0].sha, c2);
        let ranged = git_log(repo.cwd(), GitLogQuery { rev: Some(format!("{c1}..{c3}")), ..Default::default() }).await.unwrap();
        assert_eq!(ranged.len(), 2);
        assert!(git_log(repo.cwd(), GitLogQuery { rev: Some("--output=/tmp/x".into()), ..Default::default() }).await.is_err());

        // show
        let d = git_show(repo.cwd(), c2.clone()).await.unwrap();
        assert_eq!(d.commit.subject, "second: Fix bug");
        let a = d.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.status.as_str(), a.additions, a.deletions), ("M", Some(2), Some(1)));
        assert!(d.files.iter().find(|f| f.path == "bin.dat").unwrap().binary);
        let root_commit = git_show(repo.cwd(), c1.clone()).await.unwrap();
        assert_eq!(root_commit.files.len(), 1);
        assert_eq!(root_commit.files[0].status, "A");
        let ren = git_show(repo.cwd(), c3[..10].to_string()).await.unwrap();
        let b = ren.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!((b.status.as_str(), b.old_path.as_deref()), ("R", Some("a.txt")));

        // file versions + diff
        let v = git_commit_file_versions(repo.cwd(), c2.clone(), "a.txt".into(), None).await.unwrap();
        assert_eq!((v.original.as_str(), v.modified.as_str()), ("one\ntwo\n", "one\nTWO\nthree\n"));
        let v = git_commit_file_versions(repo.cwd(), c1.clone(), "a.txt".into(), None).await.unwrap();
        assert_eq!((v.original.as_str(), v.modified.as_str()), ("", "one\ntwo\n"));
        let v = git_commit_file_versions(repo.cwd(), c3.clone(), "b.txt".into(), Some("a.txt".into())).await.unwrap();
        assert_eq!(v.original, v.modified);
        assert!(git_commit_file_versions(repo.cwd(), c2.clone(), "bin.dat".into(), None).await.unwrap().binary);
        let diff = git_commit_diff(repo.cwd(), c2.clone()).await.unwrap();
        assert!(diff.contains("+TWO") && diff.contains("-two"));

        // blame: working tree (with an uncommitted line) and at a revision
        repo.write("b.txt", "one\nTWO\nthree\nlocal\n");
        let blame = git_blame(repo.cwd(), "b.txt".into(), None).await.unwrap();
        assert_eq!(blame.content, "one\nTWO\nthree\nlocal\n");
        assert_eq!(blame.lines.len(), 4);
        assert_eq!(blame.lines[0].sha, c1);
        assert_eq!(blame.lines[1].sha, c2);
        assert!(blame.commits[&blame.lines[3].sha].uncommitted);
        assert_eq!(blame.commits[&c1].filename, "a.txt");
        let old = git_blame(repo.cwd(), "a.txt".into(), Some(c1.clone())).await.unwrap();
        assert_eq!(old.content, "one\ntwo\n");
        assert!(old.lines.iter().all(|l| l.sha == c1));
        assert!(git_blame(repo.cwd(), "bin.dat".into(), None).await.is_err());
        assert!(git_blame(repo.cwd(), "b.txt".into(), Some("-x".into())).await.is_err());
    }

    #[tokio::test]
    async fn refs_branches_tags_stash() {
        refs_branches_tags_stash_body().await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refs_branches_tags_stash_over_ssh() {
        let _ssh = OverSsh::on();
        refs_branches_tags_stash_body().await
    }

    async fn refs_branches_tags_stash_body() {
        let repo = TempRepo::new("refs");
        repo.git(&["config", "tag.gpgsign", "false"]);
        let c1 = commit(&repo, "f.txt", "1\n", "one");

        git_checkout(repo.cwd(), "feat".into(), true, None).await.unwrap();
        let c2 = commit(&repo, "f.txt", "2\n", "two");
        let refs = git_refs(repo.cwd()).await.unwrap();
        assert_eq!(refs.current.as_deref(), Some("feat"));
        assert_eq!(refs.head_sha.as_deref(), Some(c2.as_str()));
        assert_eq!(refs.local.len(), 2);
        assert!(refs.local.iter().find(|b| b.name == "feat").unwrap().current);
        assert_eq!(refs.operation, None);

        // invalid names / option injection
        assert!(git_checkout(repo.cwd(), "bad..name".into(), true, None).await.is_err());
        assert!(git_checkout(repo.cwd(), "-f".into(), false, None).await.is_err());
        assert!(git_tag_create(repo.cwd(), "--force".into(), None, None).await.is_err());

        // tags
        git_tag_create(repo.cwd(), "v1".into(), Some(c1.clone()), None).await.unwrap();
        git_tag_create(repo.cwd(), "v2".into(), None, Some("release two".into())).await.unwrap();
        let refs = git_refs(repo.cwd()).await.unwrap();
        let v1 = refs.tags.iter().find(|t| t.name == "v1").unwrap();
        assert_eq!((v1.sha.as_str(), v1.annotated), (c1.as_str(), false));
        let v2 = refs.tags.iter().find(|t| t.name == "v2").unwrap();
        assert_eq!((v2.sha.as_str(), v2.annotated, v2.subject.as_str()), (c2.as_str(), true, "release two"));
        git_tag_delete(repo.cwd(), "v1".into()).await.unwrap();
        assert!(git_refs(repo.cwd()).await.unwrap().tags.iter().all(|t| t.name != "v1"));

        // checkout back, merge, rename, delete
        git_checkout(repo.cwd(), "main".into(), false, None).await.unwrap();
        let out = git_merge(repo.cwd(), "feat".into(), true).await.unwrap();
        assert!(out.contains("Merge made") || out.contains("merge"), "{out}");
        let merge_sha = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
        assert_eq!(git_show(repo.cwd(), merge_sha.clone()).await.unwrap().commit.parents.len(), 2);
        assert!(git_commit_diff(repo.cwd(), merge_sha).await.unwrap().contains("+2"));
        git_branch_rename(repo.cwd(), "feat".into(), "feature".into()).await.unwrap();
        git_branch_delete(repo.cwd(), "feature".into(), false, false).await.unwrap();
        assert_eq!(git_refs(repo.cwd()).await.unwrap().local.len(), 1);

        // create without switching
        git_branch_create(repo.cwd(), "later".into(), Some(c1.clone())).await.unwrap();
        let refs = git_refs(repo.cwd()).await.unwrap();
        assert_eq!(refs.current.as_deref(), Some("main"));
        assert_eq!(refs.local.iter().find(|b| b.name == "later").map(|b| b.sha.as_str()), Some(c1.as_str()));
        assert!(git_branch_create(repo.cwd(), "-x".into(), None).await.is_err());
        git_branch_delete(repo.cwd(), "later".into(), true, false).await.unwrap();

        // detached checkout via commit action, then back
        git_commit_action(repo.cwd(), c1.clone(), CommitAction::Checkout).await.unwrap();
        let refs = git_refs(repo.cwd()).await.unwrap();
        assert_eq!((refs.current, refs.head_sha.as_deref()), (None, Some(c1.as_str())));
        git_checkout(repo.cwd(), "main".into(), false, None).await.unwrap();

        // conflict → operation → abort
        git_checkout(repo.cwd(), "side".into(), true, Some(c1.clone())).await.unwrap();
        commit(&repo, "f.txt", "side\n", "side change");
        git_checkout(repo.cwd(), "main".into(), false, None).await.unwrap();
        assert!(git_merge(repo.cwd(), "side".into(), false).await.is_err());
        assert_eq!(git_refs(repo.cwd()).await.unwrap().operation.as_deref(), Some("merge"));
        git_abort(repo.cwd()).await.unwrap();
        assert_eq!(git_refs(repo.cwd()).await.unwrap().operation, None);
        assert!(git_abort(repo.cwd()).await.is_err());

        // revert + reset
        let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
        git_commit_action(repo.cwd(), c2.clone(), CommitAction::Revert).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("f.txt")).unwrap(), "1\n");
        git_commit_action(repo.cwd(), head.clone(), CommitAction::ResetHard).await.unwrap();
        assert_eq!(repo.git(&["rev-parse", "HEAD"]).trim(), head);
        assert_eq!(std::fs::read_to_string(repo.dir.join("f.txt")).unwrap(), "2\n");

        // stash
        assert!(git_stash_list(repo.cwd()).await.unwrap().is_empty());
        assert!(git_stash_save(repo.cwd(), None, false).await.is_err());
        repo.write("f.txt", "wip\n");
        repo.write("new.txt", "untracked\n");
        git_stash_save(repo.cwd(), Some("my wip".into()), true).await.unwrap();
        assert!(!repo.dir.join("new.txt").exists());
        let stashes = git_stash_list(repo.cwd()).await.unwrap();
        assert_eq!(stashes.len(), 1);
        assert_eq!((stashes[0].index, stashes[0].name.as_str()), (0, "stash@{0}"));
        assert!(stashes[0].message.contains("my wip"));
        git_stash_apply(repo.cwd(), 0, false).await.unwrap();
        assert_eq!(std::fs::read_to_string(repo.dir.join("f.txt")).unwrap(), "wip\n");
        assert!(repo.dir.join("new.txt").exists());
        git_stash_drop(repo.cwd(), 0).await.unwrap();
        assert!(git_stash_list(repo.cwd()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_remote_roundtrip() {
        local_remote_roundtrip_body().await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_remote_roundtrip_over_ssh() {
        let _ssh = OverSsh::on();
        local_remote_roundtrip_body().await
    }

    async fn local_remote_roundtrip_body() {
        // "remote" is a bare repo on disk — no network
        let bare = TempRepo::new("bare");
        bare.git(&["config", "core.bare", "true"]);
        let repo = TempRepo::new("clone");
        commit(&repo, "f.txt", "1\n", "one");
        repo.git(&["remote", "add", "origin", &bare.dir.join(".git").to_string_lossy()]);

        let out = git_push(repo.cwd(), RemoteOpts { remote: None, rebase: false, set_upstream: true, force: false, tags: false }).await.unwrap();
        assert!(out.contains("main"), "{out}");
        let refs = git_refs(repo.cwd()).await.unwrap();
        assert_eq!(refs.local[0].upstream.as_deref(), Some("origin/main"));
        assert!(refs.remote.iter().any(|b| b.name == "origin/main"));
        assert_eq!(refs.remotes[0].name, "origin");

        // remote-only branch → tracking checkout
        repo.git(&["push", "-q", "origin", "main:topic"]);
        repo.git(&["branch", "-q", "--set-upstream-to=origin/main", "main"]);
        git_fetch(repo.cwd(), RemoteOpts { remote: None, rebase: false, set_upstream: false, force: false, tags: false }).await.unwrap();
        git_checkout(repo.cwd(), "origin/topic".into(), false, None).await.unwrap();
        let refs = git_refs(repo.cwd()).await.unwrap();
        assert_eq!(refs.current.as_deref(), Some("topic"));
        assert_eq!(refs.local.iter().find(|b| b.name == "topic").unwrap().upstream.as_deref(), Some("origin/topic"));
        git_pull(repo.cwd(), RemoteOpts { remote: None, rebase: true, set_upstream: false, force: false, tags: false }).await.unwrap();

        git_checkout(repo.cwd(), "main".into(), false, None).await.unwrap();
        git_branch_delete(repo.cwd(), "origin/topic".into(), false, true).await.unwrap();
        assert!(git_refs(repo.cwd()).await.unwrap().remote.iter().all(|b| b.name != "origin/topic"));
        assert!(git_fetch(repo.cwd(), RemoteOpts { remote: Some("nope".into()), rebase: false, set_upstream: false, force: false, tags: false }).await.is_err());
    }
}
