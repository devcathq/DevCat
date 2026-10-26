//! "Keep awake" (like Caffeine / Amphetamine): stop the display and the system from sleeping
//! while it's on, optionally for a limited time. Uses OS power assertions via `keepawake`
//! (IOKit on macOS, SetThreadExecutionState on Windows, the session inhibitor on Linux).
//!
//! The assertion is owned by one dedicated thread: on Windows it is per-thread, so it must be
//! taken and released on the same, long-lived thread. State changes are broadcast to every
//! window as `keep-awake` events.
use crate::error::{AppError, AppResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeState {
    pub on: bool,
    /// Unix ms when it turns itself off; None = until turned off.
    pub until: Option<i64>,
}

enum Cmd {
    On(mpsc::Sender<Result<(), String>>),
    Off,
}

struct Holder {
    tx: mpsc::Sender<Cmd>,
    state: KeepAwakeState,
    /// Bumped on every change so an old timer can't turn off a newer request.
    generation: u64,
}

static HOLDER: Lazy<Mutex<Holder>> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("keep-awake".into())
        .spawn(move || {
            let mut guard: Option<keepawake::KeepAwake> = None;
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Cmd::On(reply) => {
                        if guard.is_none() {
                            match keepawake::Builder::default()
                                .display(true)
                                .idle(true)
                                .reason("Keep awake is on in DevCat")
                                .app_name("DevCat")
                                .app_reverse_domain(crate::APP_ID)
                                .create()
                            {
                                Ok(g) => guard = Some(g),
                                Err(e) => {
                                    let _ = reply.send(Err(e.to_string()));
                                    continue;
                                }
                            }
                        }
                        let _ = reply.send(Ok(()));
                    }
                    Cmd::Off => guard = None,
                }
            }
        })
        .expect("spawn keep-awake thread");
    Mutex::new(Holder { tx, state: KeepAwakeState::default(), generation: 0 })
});

fn broadcast(state: &KeepAwakeState) {
    crate::events::emit("keep-awake", state);
}

#[cfg_attr(feature = "gui", tauri::command)]
pub fn keep_awake_status() -> KeepAwakeState {
    HOLDER.lock().state.clone()
}

/// Turn keep-awake on (for `minutes`, or until turned off) or off.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn keep_awake_set(on: bool, minutes: Option<u32>) -> AppResult<KeepAwakeState> {
    let (state, generation) = {
        let mut h = HOLDER.lock();
        if on {
            let (rtx, rrx) = mpsc::channel();
            h.tx.send(Cmd::On(rtx)).map_err(|_| AppError::msg("Keep-awake service stopped"))?;
            rrx.recv_timeout(Duration::from_secs(5))
                .map_err(|_| AppError::msg("Keep-awake did not respond"))?
                .map_err(|e| AppError::msg(format!("Could not keep the system awake: {e}")))?;
            let until = minutes.filter(|m| *m > 0).map(|m| chrono::Utc::now().timestamp_millis() + i64::from(m) * 60_000);
            h.state = KeepAwakeState { on: true, until };
        } else {
            let _ = h.tx.send(Cmd::Off);
            h.state = KeepAwakeState::default();
        }
        h.generation += 1;
        (h.state.clone(), h.generation)
    };
    broadcast(&state);
    if let (true, Some(m)) = (on, minutes.filter(|m| *m > 0)) {
        crate::rt::spawn(async move {
            tokio::time::sleep(Duration::from_secs(u64::from(m) * 60)).await;
            let off = {
                let mut h = HOLDER.lock();
                if h.generation != generation {
                    return; // changed since: leave it
                }
                let _ = h.tx.send(Cmd::Off);
                h.state = KeepAwakeState::default();
                h.generation += 1;
                h.state.clone()
            };
            broadcast(&off);
        });
    }
    Ok(state)
}

/// Release on app exit (the OS also drops assertions when the process ends).
pub fn shutdown() {
    let h = HOLDER.lock();
    let _ = h.tx.send(Cmd::Off);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Takes and releases a real assertion (harmless; macOS shows it in `pmset -g assertions`).
    #[test]
    fn toggles_on_dedicated_thread() {
        let h = HOLDER.lock();
        let (rtx, rrx) = mpsc::channel();
        h.tx.send(Cmd::On(rtx)).unwrap();
        let r = rrx.recv_timeout(Duration::from_secs(5)).unwrap();
        if cfg!(target_os = "linux") && r.is_err() {
            return; // no session bus in CI containers
        }
        r.unwrap();
        #[cfg(target_os = "macos")]
        {
            // Only this process's assertions: a running DevCat may hold its own.
            let mine = format!("pid {}(", std::process::id());
            let pm = || {
                let out = String::from_utf8_lossy(&std::process::Command::new("pmset").args(["-g", "assertions"]).output().unwrap().stdout).into_owned();
                out.lines().filter(|l| l.contains(&mine)).collect::<Vec<_>>().join("\n")
            };
            assert!(pm().contains("Keep awake is on in DevCat"), "assertion not registered");
            h.tx.send(Cmd::Off).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            assert!(!pm().contains("Keep awake is on in DevCat"), "assertion not released");
            return;
        }
        #[allow(unreachable_code)]
        h.tx.send(Cmd::Off).unwrap();
    }
}
