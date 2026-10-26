//! Scheduled DevCat Agent jobs (issue #11): a prompt that runs on a schedule in a folder, with a
//! DevCat Agent connection and a permission mode. Each run is a new chat, the same as one the user
//! starts: it's in the folder's chat list, and it asks for approvals like any chat (the window or a
//! paired phone answers). Jobs run while DevCat is open; one that came due while it was closed runs
//! once when it opens. Kept in `<data>/agent-jobs.json`; `agent-jobs` events tell the window.
use crate::error::{AppError, AppResult};
use chrono::{Datelike, Local, TimeZone};
use devcat_harness::Policy;
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Runs a job keeps (newest first); their chats stay in the folder's list either way.
const KEEP_RUNS: usize = 20;
/// The shortest interval.
const MIN_MINUTES: u32 = 5;
/// How often the scheduler looks for jobs that are due.
const TICK: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    /// Every `minutes` (at least 5), counted from the last run.
    Every { minutes: u32 },
    /// At `time` ("HH:MM", local time) on `days` (1 = Monday … 7 = Sunday; none = every day).
    Daily { time: String, days: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRun {
    pub started_ms: u64,
    /// The run's chat (None if it couldn't start).
    pub session_id: Option<String>,
    /// How it ended: "answered", "stepLimit", "stopped", "error"; None while it runs.
    pub end: Option<String>,
    pub error: Option<String>,
    pub finished_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub provider_id: String,
    pub prompt: String,
    pub policy: Policy,
    /// Its commands run in the OS sandbox (macOS, Linux).
    #[serde(default)]
    pub sandbox: bool,
    pub schedule: Schedule,
    pub enabled: bool,
    #[serde(default)]
    pub runs: Vec<JobRun>,
    /// When it runs next (ms); None while it's off.
    #[serde(default)]
    pub next_ms: Option<u64>,
}

static FILE: OnceCell<PathBuf> = OnceCell::new();
/// Reading and writing the file (and the decisions made on what's in it) go one at a time.
static LOCK: Mutex<()> = Mutex::new(());
/// Jobs running now: job id -> the run's chat.
static RUNNING: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(Default::default);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The jobs on disk; none before the first one is saved. A file that can't be read or parsed is an
/// error, and nothing is written over it: a newer DevCat's jobs (after a downgrade), a hand edit
/// with a stray comma or a moment the antivirus holds the file mustn't erase every job.
fn read() -> AppResult<Vec<Job>> {
    let Some(f) = FILE.get() else { return Ok(Vec::new()) };
    match std::fs::read(f) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| AppError::msg(format!("Couldn't read the scheduled jobs in {} ({e}). Fix the file or move it away; DevCat doesn't write over it.", f.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(AppError::msg(format!("Couldn't read the scheduled jobs in {}: {e}", f.display()))),
    }
}

fn write(jobs: &[Job]) -> AppResult<()> {
    let f = FILE.get().ok_or_else(|| AppError::msg("Scheduled jobs aren't initialised"))?;
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(jobs)?)?;
    std::fs::rename(tmp, f)?;
    Ok(())
}

/// Change the jobs on disk; the window hears about it. Only a change is written (the scheduler
/// looks every 20 s), and nothing is when the file can't be read.
fn update<T>(f: impl FnOnce(&mut Vec<Job>) -> T) -> AppResult<T> {
    let (out, changed_any) = {
        let _g = LOCK.lock();
        let mut jobs = read()?;
        let before = jobs.clone();
        let out = f(&mut jobs);
        let changed_any = jobs != before;
        if changed_any {
            write(&jobs)?;
        }
        (out, changed_any)
    };
    if changed_any {
        changed();
    }
    Ok(out)
}

/// Mark a job running unless it is: a double-click on Run now, or Run now as its time comes,
/// starts it once.
fn claim(id: &str) -> bool {
    match RUNNING.lock().entry(id.to_string()) {
        std::collections::hash_map::Entry::Occupied(_) => false,
        std::collections::hash_map::Entry::Vacant(v) => {
            v.insert(String::new());
            true
        }
    }
}

fn changed() {
    crate::events::emit("agent-jobs", json!({ "type": "changed" }));
}

/// "HH:MM" -> (hour, minute).
fn hm(time: &str) -> Option<(u32, u32)> {
    let (h, m) = time.trim().split_once(':')?;
    let (h, m) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// The first time after `after` (ms) the schedule says to run.
pub fn next_after(s: &Schedule, after: u64) -> Option<u64> {
    match s {
        Schedule::Every { minutes } => Some(after + u64::from((*minutes).max(MIN_MINUTES)) * 60_000),
        Schedule::Daily { time, days } => {
            let (h, m) = hm(time)?;
            let start = Local.timestamp_millis_opt(after as i64).single()?.date_naive();
            (0..8).find_map(|d| {
                let date = start + chrono::Duration::days(d);
                if !days.is_empty() && !days.contains(&(date.weekday().number_from_monday() as u8)) {
                    return None;
                }
                // a time skipped by a clock change doesn't run that day
                let at = Local.from_local_datetime(&date.and_hms_opt(h, m, 0)?).earliest()?.timestamp_millis() as u64;
                (at > after).then_some(at)
            })
        }
    }
}

fn check(job: &Job) -> AppResult<()> {
    if job.name.trim().is_empty() {
        return Err(AppError::msg("Give the job a name"));
    }
    if job.prompt.trim().is_empty() {
        return Err(AppError::msg("Write what the agent should do"));
    }
    match &job.schedule {
        Schedule::Every { minutes } if *minutes < MIN_MINUTES => Err(AppError::msg(format!("Run it at most every {MIN_MINUTES} minutes"))),
        Schedule::Daily { time, days } if hm(time).is_none() || days.iter().any(|d| !(1..=7).contains(d)) => Err(AppError::msg("Pick a time of day (HH:MM)")),
        _ => Ok(()),
    }
}

/// Set up the jobs file and start the scheduler.
pub fn init(data_dir: PathBuf) {
    if FILE.set(data_dir.join("agent-jobs.json")).is_err() {
        return;
    }
    end_interrupted_runs();
    crate::rt::spawn(async {
        // let the app finish starting first
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            tick();
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Runs DevCat quit in the middle of are over (and shown so, not as running forever).
fn end_interrupted_runs() {
    let _ = update(|jobs| {
        for r in jobs.iter_mut().flat_map(|j| j.runs.iter_mut()).filter(|r| r.end.is_none()) {
            r.end = Some("stopped".into());
            r.error = Some("DevCat was closed while it ran.".into());
        }
    });
}

/// Start the jobs that are due.
fn tick() {
    let now = now_ms();
    let mut claimed = Vec::new();
    let due = update(|jobs| {
        let mut due = Vec::new();
        for job in jobs.iter_mut().filter(|j| j.enabled) {
            match job.next_ms {
                Some(at) if at <= now && claim(&job.id) => {
                    claimed.push(job.id.clone());
                    // runs once even if several runs were missed, then on schedule again
                    job.next_ms = next_after(&job.schedule, now);
                    due.push(job.clone());
                }
                None => job.next_ms = next_after(&job.schedule, now),
                _ => {}
            }
        }
        due
    });
    match due {
        Ok(due) => {
            for job in due {
                crate::rt::spawn(run(job));
            }
        }
        // not started after all (the window shows the error when it lists the jobs)
        Err(_) => {
            let mut running = RUNNING.lock();
            for id in claimed {
                running.remove(&id);
            }
        }
    }
}

/// One run of a job (claimed by the caller): a new chat in its folder, the prompt as its message,
/// until the turn ends. The chat is the scheduler's: a tab that shows it doesn't end it.
async fn run(job: Job) {
    let started_ms = now_ms();
    let opened = crate::agent::open(job.cwd.clone(), job.provider_id.clone(), None, "schedule", crate::agent::SCHEDULE).await;
    let session_id = match opened {
        Ok(o) => o.session_id,
        Err(e) => {
            RUNNING.lock().remove(&job.id);
            let error = format!("Couldn't start: {e}");
            let _ = record(&job.id, JobRun { started_ms, session_id: None, end: Some("error".into()), error: Some(error.clone()), finished_ms: Some(now_ms()) });
            finished(&job, None, "error", Some(&error)).await;
            return;
        }
    };
    RUNNING.lock().insert(job.id.clone(), session_id.clone());
    // the previous run's chat stays in the list; it doesn't need to stay open
    if let Some(prev) = job.runs.first().and_then(|r| r.session_id.as_deref()) {
        crate::agent::close_if_idle(prev);
    }
    let _ = record(&job.id, JobRun { started_ms, session_id: Some(session_id.clone()), end: None, error: None, finished_ms: None });
    let when = Local.timestamp_millis_opt(started_ms as i64).single().map(|t| t.format("%b %-d, %H:%M").to_string()).unwrap_or_default();
    let _ = crate::agent::agent_rename(session_id.clone(), format!("{} · {when}", job.name)).await;
    let model = crate::ai::agent_config(&job.provider_id).map(|(_, cfg)| cfg.model).unwrap_or_default();
    // as the chat would: the model's window if the catalog knows it, else a common one
    let window = crate::ai::context_window(&model).await.unwrap_or(128_000);
    let (end, error) = match crate::agent::run_turn(&session_id, &job.prompt, job.policy, Some(window), job.sandbox).await {
        Ok(r) => r,
        Err(e) => ("error".into(), Some(e.to_string())),
    };
    RUNNING.lock().remove(&job.id);
    let _ = update(|jobs| {
        if let Some(r) = jobs.iter_mut().find(|j| j.id == job.id).and_then(|j| j.runs.iter_mut().find(|r| r.session_id.as_deref() == Some(&session_id))) {
            r.end = Some(end.clone());
            r.error = error.clone();
            r.finished_ms = Some(now_ms());
        }
    });
    finished(&job, Some(&session_id), &end, error.as_deref()).await;
}

/// Add a run to a job (newest first).
fn record(job_id: &str, run: JobRun) -> AppResult<()> {
    update(|jobs| {
        if let Some(j) = jobs.iter_mut().find(|j| j.id == job_id) {
            j.runs.insert(0, run);
            j.runs.truncate(KEEP_RUNS);
        }
    })
}

async fn finished(job: &Job, session_id: Option<&str>, end: &str, error: Option<&str>) {
    let message = match (end, error) {
        ("answered", _) => "Finished. Open its chat to read the answer.".to_string(),
        ("stopped", _) => "Stopped.".to_string(),
        ("stepLimit", _) => "Stopped after the step limit without finishing.".to_string(),
        (_, Some(e)) => format!("Failed: {e}"),
        _ => "Failed.".to_string(),
    };
    crate::events::emit("agent-jobs", json!({ "type": "finished", "jobId": job.id, "name": job.name, "sessionId": session_id, "end": end, "message": message }));
    crate::notify::job(&format!("{}: {}", job.name, if end == "answered" { "done" } else { "didn't finish" }), &message, false).await;
}

/// A job's chat waits for the user (an approval, a question): say so, once per request.
pub fn waiting(session_id: &str, what: &str) {
    let job = RUNNING.lock().iter().find(|(_, s)| s.as_str() == session_id).map(|(id, _)| id.clone());
    let Some(job_id) = job else { return };
    let name = read().unwrap_or_default().into_iter().find(|j| j.id == job_id).map(|j| j.name).unwrap_or_else(|| "Scheduled job".into());
    crate::events::emit("agent-jobs", json!({ "type": "waiting", "jobId": job_id, "name": name, "sessionId": session_id, "message": what }));
    let (title, what) = (format!("{name}: needs you"), what.to_string());
    crate::rt::spawn(async move { crate::notify::job(&title, &what, true).await });
}

// ---------------------------------------------------------------- commands

/// The jobs, those for `cwd` only when given; with whether each is running now.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    #[serde(flatten)]
    pub job: Job,
    pub running: bool,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_jobs(cwd: Option<String>) -> AppResult<Vec<JobInfo>> {
    let same = |a: &str, b: &str| {
        let norm = |p: &str| {
            let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
            if cfg!(windows) { p.to_lowercase() } else { p }
        };
        norm(a) == norm(b)
    };
    let running = RUNNING.lock().clone();
    Ok(read()?
        .into_iter()
        .filter(|j| cwd.as_deref().is_none_or(|c| same(c, &j.cwd)))
        .map(|job| JobInfo { running: running.contains_key(&job.id), job })
        .collect())
}

/// Add a job (empty `id`) or change one; its next run is worked out again.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_job_save(job: Job) -> AppResult<Job> {
    check(&job)?;
    crate::ai::agent_config(&job.provider_id)?;
    update(|jobs| {
        let mut job = job;
        job.name = job.name.trim().to_string();
        job.next_ms = if job.enabled { next_after(&job.schedule, now_ms()) } else { None };
        match jobs.iter_mut().find(|j| !job.id.is_empty() && j.id == job.id) {
            Some(old) => {
                job.runs = std::mem::take(&mut old.runs);
                *old = job.clone();
            }
            None => {
                job.id = uuid::Uuid::new_v4().to_string();
                job.runs = vec![];
                jobs.push(job.clone());
            }
        }
        job
    })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_job_delete(id: String) -> AppResult<()> {
    update(|jobs| jobs.retain(|j| j.id != id))
}

/// Run a job now (its schedule stays as it is).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn agent_job_run(id: String) -> AppResult<()> {
    let job = read()?.into_iter().find(|j| j.id == id).ok_or_else(|| AppError::msg("That job was deleted"))?;
    if !claim(&id) {
        return Err(AppError::msg("It's running already"));
    }
    crate::rt::spawn(run(job));
    // the window shows it running right away
    tokio::time::sleep(Duration::from_millis(50)).await;
    changed();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap().timestamp_millis() as u64
    }

    #[test]
    fn next_runs() {
        // Thursday 2026-10-22, 10:00
        let now = at(2026, 10, 22, 10, 0);
        assert_eq!(next_after(&Schedule::Every { minutes: 30 }, now), Some(now + 30 * 60_000));
        assert_eq!(next_after(&Schedule::Every { minutes: 1 }, now), Some(now + 5 * 60_000), "at least every 5 minutes");
        let daily = |time: &str, days: Vec<u8>| Schedule::Daily { time: time.into(), days };
        assert_eq!(next_after(&daily("09:00", vec![]), now), Some(at(2026, 10, 23, 9, 0)), "today's has passed");
        assert_eq!(next_after(&daily("18:30", vec![]), now), Some(at(2026, 10, 22, 18, 30)));
        // weekdays only: Friday, then Monday
        assert_eq!(next_after(&daily("09:00", vec![1, 2, 3, 4, 5]), at(2026, 10, 23, 9, 0)), Some(at(2026, 10, 26, 9, 0)));
        assert_eq!(next_after(&daily("09:00", vec![7]), now), Some(at(2026, 10, 25, 9, 0)), "Sunday");
        assert_eq!(next_after(&daily("9am", vec![]), now), None);
    }

    #[test]
    fn checks() {
        let job = |schedule| Job { id: String::new(), name: "Report".into(), cwd: "/p".into(), provider_id: "x".into(), prompt: "Summarize".into(), policy: Policy::Plan, sandbox: false, schedule, enabled: true, runs: vec![], next_ms: None };
        assert!(check(&job(Schedule::Every { minutes: 60 })).is_ok());
        assert!(check(&job(Schedule::Every { minutes: 2 })).is_err());
        assert!(check(&job(Schedule::Daily { time: "25:00".into(), days: vec![] })).is_err());
        assert!(check(&job(Schedule::Daily { time: "07:30".into(), days: vec![8] })).is_err());
        assert!(check(&Job { prompt: " ".into(), ..job(Schedule::Every { minutes: 60 }) }).is_err());
        // the file format
        let v = serde_json::to_value(job(Schedule::Daily { time: "07:30".into(), days: vec![1, 5] })).unwrap();
        assert_eq!(v["schedule"], json!({ "kind": "daily", "time": "07:30", "days": [1, 5] }));
        assert_eq!(v["policy"], json!("plan"));
    }

    #[test]
    fn a_file_that_cant_be_read_is_never_written_over() {
        let dir = std::env::temp_dir().join(format!("devcat-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // the one test that sets the file
        FILE.set(dir.join("agent-jobs.json")).unwrap();
        let f = FILE.get().unwrap();
        let job = Job { id: "j1".into(), name: "Report".into(), cwd: "/p".into(), provider_id: "x".into(), prompt: "Summarize".into(), policy: Policy::Plan, sandbox: false, schedule: Schedule::Every { minutes: 60 }, enabled: true, runs: vec![], next_ms: Some(1) };
        // no file yet: no jobs
        assert_eq!(read().unwrap(), vec![]);
        // a stray comma, or a newer DevCat's kind of schedule: an error, and the file stays as it is
        for bad in [r#"[{"id":"j1",}]"#.to_string(), serde_json::to_string(&vec![job.clone()]).unwrap().replace(r#""kind":"every""#, r#""kind":"cron""#)] {
            std::fs::write(f, &bad).unwrap();
            assert!(read().is_err());
            assert!(update(|jobs| jobs.clear()).is_err());
            tick();
            assert_eq!(std::fs::read_to_string(f).unwrap(), bad);
        }
        // nothing changed: nothing written (a compact file stays compact)
        let compact = serde_json::to_string(&vec![job.clone()]).unwrap();
        std::fs::write(f, &compact).unwrap();
        update(|_| ()).unwrap();
        assert_eq!(std::fs::read_to_string(f).unwrap(), compact);
        // a change is written
        update(|jobs| jobs[0].name = "Morning report".into()).unwrap();
        assert_eq!(read().unwrap()[0].name, "Morning report");
        // a run still marked running when DevCat starts is over
        update(|jobs| jobs[0].runs = vec![JobRun { started_ms: 1, session_id: Some("s".into()), end: None, error: None, finished_ms: None }]).unwrap();
        end_interrupted_runs();
        assert_eq!(read().unwrap()[0].runs[0].end.as_deref(), Some("stopped"));
        // started once, however it's asked for
        assert!(claim("j2") && !claim("j2"));
        RUNNING.lock().remove("j2");
        let _ = std::fs::remove_dir_all(dir);
    }
}
