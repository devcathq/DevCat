//! Tickets: runs DevCat didn't finish. A run notes itself in `<data>/tickets/running/<id>.json`
//! (`InFlight`) from just before its ticket moves to the board's "working" column until the run
//! ends, however it ends. A note still there when DevCat starts is a run that DevCat quitting or
//! crashing cut short: its agent is stopped if it's still at work (`cut_short`, before
//! `worker::tidy_worktrees` removes its worktree), and its ticket goes to the board's failed
//! column with a comment (`put_back`), so it isn't left in "working" for good, where the watch
//! never takes it again. Only tickets of runs DevCat noted are touched, never one a person moved
//! to "working".
use super::trackers::Tracker;
use super::{Board, Column, RunRecord, Ticket, TrackerKind};
use crate::error::AppResult;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// What the ticket's comment and the history say about a run cut short.
const STOPPED: &str = "DevCat stopped during the run (it quit or crashed)";

/// A process, told apart from one that gets its pid later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proc {
    pub pid: u32,
    /// Its start time and command line, as `ps` shows them ("" where there's no `ps`: then it's
    /// never taken for running).
    pub id: String,
}

/// This DevCat.
static ME: Lazy<Proc> = Lazy::new(|| Proc::of(std::process::id()));

impl Proc {
    fn of(pid: u32) -> Proc {
        Proc { pid, id: identity(pid).unwrap_or_default() }
    }

    /// Still running: the same process, not another one that got its pid since.
    fn alive(&self) -> bool {
        !self.id.is_empty() && identity(self.pid).as_deref() == Some(self.id.as_str())
    }
}

/// "Thu Nov 19 19:38:00 2026 /path/to/claude -p ...": when process `pid` started and its command
/// line (None: no such process).
#[cfg(unix)]
fn identity(pid: u32) -> Option<String> {
    let o = std::process::Command::new("ps").args(["-ww", "-p", &pid.to_string(), "-o", "lstart=", "-o", "args="]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

#[cfg(not(unix))]
fn identity(_pid: u32) -> Option<String> {
    None
}

/// A run's note (see the module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flight {
    pub board: String,
    pub ticket_id: String,
    pub key: String,
    pub title: String,
    pub url: String,
    /// The column it was in before the run: where it goes back on a board without a failed column.
    pub from: String,
    /// The branch the agent works on.
    pub branch: String,
    /// Where the agent works (a worktree: its folder).
    pub cwd: String,
    pub started: i64,
    /// The DevCat running it: a note of a DevCat still running is left to it.
    pub devcat: Proc,
    /// The agent's program (Claude Code), once it started.
    #[serde(default)]
    pub agent: Option<Proc>,
}

/// A run's note while it runs; dropping it (the run ended) removes it.
pub struct InFlight(Arc<Mutex<Option<(PathBuf, Flight)>>>);

impl InFlight {
    /// `t` (still in the column it came from) is about to move to "working", worked on `branch` in
    /// `cwd`.
    pub fn start(b: &Board, t: &Ticket, branch: &str, cwd: &str) -> InFlight {
        let f = Flight {
            board: b.id.clone(),
            ticket_id: t.id.clone(),
            key: t.key.clone(),
            title: t.title.clone(),
            url: t.url.clone(),
            from: t.column.clone(),
            branch: branch.to_string(),
            cwd: cwd.to_string(),
            started: chrono::Utc::now().timestamp_millis(),
            devcat: ME.clone(),
            agent: None,
        };
        let note = super::dir().ok().map(|d| (d.join("running").join(format!("{}.json", uuid::Uuid::new_v4())), f));
        if let Some((p, f)) = &note
            && let Err(e) = super::write_json(p, f)
        {
            eprintln!("tickets: couldn't note the run of {}: {e}", f.key);
        }
        InFlight(Arc::new(Mutex::new(note)))
    }

    /// For the agent's program once it started: noted, so a crash doesn't leave it working.
    pub fn agent(&self) -> Arc<dyn Fn(u32) + Send + Sync> {
        let note = self.0.clone();
        Arc::new(move |pid| {
            if let Some((p, f)) = note.lock().as_mut() {
                f.agent = Some(Proc::of(pid));
                let _ = super::write_json(p, f);
            }
        })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some((p, _)) = self.0.lock().take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A run cut short: its note's file, the note, and whether its agent was still at work.
pub type Cut = (PathBuf, Flight, bool);

/// DevCat starting: the runs a quit or crash cut short (not those of another DevCat that's still
/// running), with their agents stopped where they were still at work.
pub async fn cut_short() -> Vec<Cut> {
    let Ok(d) = super::dir() else { return vec![] };
    let mut cut = vec![];
    for p in std::fs::read_dir(d.join("running")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
        let Some(f) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Flight>(&s).ok()) else {
            let _ = std::fs::remove_file(&p);
            continue;
        };
        if f.devcat.alive() {
            continue;
        }
        let working = f.agent.as_ref().is_some_and(Proc::alive);
        if let Some(a) = f.agent.as_ref().filter(|_| working) {
            stop(a).await;
        }
        cut.push((p, f, working));
    }
    cut
}

/// Stops the agent's program, and the commands it started (its process group, see
/// `claude::runner`): SIGTERM, and SIGKILL to what's left after 2 seconds.
#[cfg(unix)]
async fn stop(a: &Proc) {
    let pid = a.pid as libc::pid_t;
    // SAFETY: plain signals to the process checked to be the agent DevCat started (`Proc::alive`),
    // and its process group when it leads one.
    let group = unsafe { libc::getpgid(pid) } == pid;
    let signal = |s| unsafe {
        if group {
            libc::killpg(pid, s);
        } else {
            libc::kill(pid, s);
        }
    };
    signal(libc::SIGTERM);
    for _ in 0..20 {
        if !a.alive() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // the group outlives its leader, and its id isn't anyone else's while it has members
    if group || a.alive() {
        signal(libc::SIGKILL);
    }
}

#[cfg(not(unix))]
async fn stop(_a: &Proc) {}

/// The tickets of runs `cut_short`: each goes to the board's failed column (without one: back
/// where it was) with a comment, and into the board's history; then its note is removed. A ticket
/// that isn't in the board's "working" column any more (a person moved it) stays where it is.
/// When the tracker can't be reached the note stays, for the next start.
pub async fn put_back(cut: Vec<Cut>) {
    for (p, f, working) in cut {
        let done = match super::board(&f.board) {
            Ok(b) => match put_one(&b, &f, working).await {
                Ok(rec) => {
                    super::record(&b.id, rec);
                    true
                }
                Err(e) => {
                    eprintln!("tickets: {}: couldn't put {} right after DevCat stopped during its run: {e}", b.name, f.key);
                    false
                }
            },
            // the board is gone
            Err(_) => true,
        };
        if done {
            let _ = std::fs::remove_file(&p);
        }
    }
}

async fn put_one(b: &Board, f: &Flight, working: bool) -> AppResult<RunRecord> {
    let tr = Tracker::new(b, super::token(b))?;
    let t = tr.get(&f.ticket_id).await?;
    let mut rec = super::worker::error_record(&f.ticket_id, format!("{STOPPED}."));
    rec.key = f.key.clone();
    rec.title = f.title.clone();
    rec.url = f.url.clone();
    rec.branch = Some(f.branch.clone()).filter(|b| !b.is_empty());
    rec.cwd = Some(f.cwd.clone());
    rec.started = f.started;
    if working {
        rec.notes.push("Its agent was still working when DevCat started again, and was stopped.".into());
    }
    if b.working.as_ref().is_some_and(|w| w.id != t.column) {
        rec.notes.push("It had been moved since, so DevCat left it where it was.".into());
        return Ok(rec);
    }
    let target = b.failed_to.clone().or_else(|| (t.column != f.from).then(|| Column { id: f.from.clone(), name: "where it was".into() }));
    if let Some(c) = &target {
        tr.move_to(&t, &c.id).await?;
        rec.moved_to = Some(c.name.clone());
    }
    if b.comment
        && let Err(e) = tr.comment(&t, &comment(b, f)).await
    {
        rec.notes.push(format!("Couldn't comment: {e}"));
    }
    Ok(rec)
}

/// The ticket's comment: what happened, where the work is, how to try again.
fn comment(b: &Board, f: &Flight) -> String {
    let mut c = if f.branch.is_empty() { format!("{STOPPED}.") } else { format!("{STOPPED}; the branch {} keeps any committed work.", f.branch) };
    match (b.kind, &b.pick_from) {
        // the trigger label added again starts a new run (`github::ready`)
        (TrackerKind::Github, Some(pick)) => c.push_str(&format!(" Add the label `{}` again to try again.", pick.id)),
        _ => c.push_str(" To try again, fix it with the agent in DevCat's Tickets tab."),
    }
    // Jira's plain text shows markdown as is
    if b.kind == TrackerKind::Jira {
        c = c.replace('`', "");
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tickets::github::tests::{board, one_issue, tickets_data};
    use crate::tickets::mock_github::MockGithub;
    use crate::tickets::worker::tests::{clone_with_origin, stand_in};
    use serde_json::json;
    use std::time::Duration;

    fn notes_of(board: &str) -> Vec<PathBuf> {
        let dir = super::super::dir().unwrap().join("running");
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains(board))).collect()
    }

    /// A run notes itself (the ticket, where it was, its branch and worktree, the agent's process,
    /// which has a process group of its own) while it runs, and removes the note when it ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_notes_itself() {
        tickets_data();
        let gh = MockGithub::start(one_issue()).await;
        let (dir, remote) = clone_with_origin("in-flight").await;
        let b = board(&gh.url, dir.to_str().unwrap(), "");
        let running = super::super::dir().unwrap().join("running");
        let act = format!("echo '    # fixed' >> calc.py; grep -l '{}' '{}'/*.json | xargs cat > \"$OUT/flight.json\"; echo $$ > \"$OUT/pid.txt\"; ps -o pgid= -p $$ | tr -d ' ' > \"$OUT/pgid.txt\"", b.id, running.display());
        let (fixer, out) = stand_in("fixed", &act).await;
        let b = super::super::tickets_board_save(Board { provider_id: fixer, ..b }).await.unwrap();
        let rec = super::super::worker::fix_one(&b, "7", None, None, &devcat_harness::Cancel::default()).await;
        assert_eq!(rec.outcome, "fixed", "{} {:?}", rec.summary, rec.notes);
        let f: Flight = serde_json::from_str(&std::fs::read_to_string(out.join("flight.json")).unwrap()).unwrap();
        let pid: u32 = std::fs::read_to_string(out.join("pid.txt")).unwrap().trim().parse().unwrap();
        assert_eq!((f.ticket_id.as_str(), f.from.as_str(), Some(f.branch.as_str()), Some(f.cwd.as_str())), ("7", "ready-for-agent", rec.branch.as_deref(), rec.cwd.as_deref()));
        assert_eq!((f.agent.map(|a| a.pid), f.devcat.pid), (Some(pid), std::process::id()));
        assert_eq!(std::fs::read_to_string(out.join("pgid.txt")).unwrap().trim(), pid.to_string(), "its own process group");
        assert!(notes_of(&b.id).is_empty(), "the note is gone");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// DevCat starting after a crash: the ticket of a run it was in the middle of goes to the
    /// board's failed column with a comment that names the branch, gets a history entry, and its
    /// agent is stopped where it's still at work; the note is gone. A ticket someone moved since
    /// stays where it is, and a process that only got the agent's old pid is left alone.
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_cut_short() {
        tickets_data();
        let mut data = one_issue().with_issue(8, "Another", "Another bug.", &["bug", "agent-review"], &["octo"]);
        data.issues.get_mut(&7).unwrap()["labels"] = json!([{ "name": "bug" }, { "name": "agent-working" }]);
        let gh = MockGithub::start(data).await;
        let (dir, remote) = clone_with_origin("cut-short").await;
        let (fixer, _) = stand_in("fixed", "").await;
        let b = super::super::tickets_board_save(board(&gh.url, dir.to_str().unwrap(), &fixer)).await.unwrap();

        let spawn = || tokio::process::Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]).process_group(0).kill_on_drop(true).spawn().unwrap();
        let (mut agent, mut stranger) = (spawn(), spawn());
        // a DevCat that's gone (its pid is this test's now)
        let gone = Proc { pid: std::process::id(), id: "Thu Nov 19 19:38:00 2026 /Applications/DevCat.app/Contents/MacOS/devcat".into() };
        let note = |n: &str, agent: Proc| {
            let f = Flight { board: b.id.clone(), ticket_id: n.into(), key: format!("#{n}"), title: "t".into(), url: String::new(), from: "ready-for-agent".into(), branch: format!("octo/{n}-fix"),
                cwd: dir.join(".worktrees").join(n).to_string_lossy().into_owned(), started: 1, devcat: gone.clone(), agent: Some(agent) };
            let p = super::super::dir().unwrap().join("running").join(format!("{}.json", uuid::Uuid::new_v4()));
            super::super::write_json(&p, &f).unwrap();
            p
        };
        let seven = note("7", Proc::of(agent.id().unwrap()));
        let eight = note("8", Proc { pid: stranger.id().unwrap(), id: "Wed Nov 18 08:00:00 2026 claude -p".into() });

        // (other tests' runs going on now are this DevCat's: left alone)
        let cut = cut_short().await;
        assert!(tokio::time::timeout(Duration::from_secs(5), agent.wait()).await.is_ok(), "the agent still at work is stopped");
        assert!(stranger.try_wait().unwrap().is_none(), "not the agent: left alone");
        put_back(cut).await;

        assert_eq!(gh.labels_of(7), vec!["bug", "ready-for-human"], "{:?}", gh.requests());
        let comments = gh.comments_on(7);
        assert_eq!(comments.len(), 1, "{comments:?}");
        assert!(comments[0].starts_with("DevCat stopped during the run (it quit or crashed); the branch octo/7-fix keeps any committed work.") && comments[0].contains("`ready-for-agent` again"), "{}", comments[0]);
        assert_eq!((gh.labels_of(8), gh.comments_on(8).len()), (vec!["bug".to_string(), "agent-review".to_string()], 0), "someone moved it: left as it is");
        assert!(!seven.exists() && !eight.exists(), "the notes are gone");
        let h = super::super::history(&b.id);
        let r = h.iter().find(|r| r.ticket_id == "7").expect("in the history");
        assert_eq!((r.outcome.as_str(), r.branch.as_deref(), r.moved_to.as_deref()), ("error", Some("octo/7-fix"), Some("ready-for-human")));
        assert!(r.notes.iter().any(|n| n.contains("agent")), "{:?}", r.notes);
        let _ = stranger.kill().await;
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}
