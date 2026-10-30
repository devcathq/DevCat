//! "Keep awake" (like Caffeine / Amphetamine): stop the display and the system from sleeping
//! while it's on, optionally for a limited time. Uses OS power assertions via `keepawake`
//! (IOKit on macOS, SetThreadExecutionState on Windows, the session inhibitor on Linux).
//!
//! The assertion is owned by one dedicated thread: on Windows it is per-thread, so it must be
//! taken and released on the same, long-lived thread. State changes are broadcast to every
//! window as `keep-awake` events.
//!
//! Power assertions don't stop the sleep that closing a laptop's lid forces. On macOS "with the
//! lid closed" ([`lid`]) also turns system sleep off (`pmset -a disablesleep 1`, which asks for an
//! admin password), like Amphetamine's closed-display mode, and turns it back on when keep awake
//! ends (turned off, its time is up, DevCat quits, or at the next start after a crash).
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
    /// Also with the lid closed: system sleep is off (macOS, see [`lid`]).
    pub lid: bool,
}

/// Where to note that DevCat turned system sleep off (`<data>/keep-awake-lid`), so a crash
/// doesn't leave the Mac unable to sleep.
static MARKER: once_cell::sync::OnceCell<std::path::PathBuf> = once_cell::sync::OnceCell::new();

/// At startup: if DevCat turned system sleep off and didn't get to turn it back on (it crashed or
/// was killed), turn it back on now (this asks for the admin password).
pub fn init(data_dir: std::path::PathBuf) {
    let _ = MARKER.set(data_dir.join("keep-awake-lid"));
    if MARKER.get().is_some_and(|m| m.exists()) {
        std::thread::spawn(|| {
            if let Err(e) = lid::restore("DevCat quit while keeping your Mac awake with the lid closed. Enter your password to let it sleep again.") {
                eprintln!("keep awake: couldn't turn system sleep back on: {e}");
            }
        });
    }
}

/// "With the lid closed" on macOS: system sleep off with `pmset -a disablesleep 1` (as an admin,
/// asked by macOS), back on with `0`. Only what DevCat turned off is turned back on: if sleep was
/// already off (the user's own setting), it's left alone.
pub mod lid {
    use super::MARKER;

    /// Whether system sleep is off now (`SleepDisabled 1` in `pmset -g`; no admin needed).
    pub fn sleep_disabled() -> bool {
        let Ok(out) = std::process::Command::new("/usr/bin/pmset").arg("-g").output() else { return false };
        parse_sleep_disabled(&String::from_utf8_lossy(&out.stdout))
    }

    pub(super) fn parse_sleep_disabled(pmset_g: &str) -> bool {
        pmset_g.lines().any(|l| {
            let mut w = l.split_whitespace();
            w.next() == Some("SleepDisabled") && w.next() == Some("1")
        })
    }

    /// Run `command` as an admin: macOS asks for the password with `prompt`.
    fn admin(command: &str, prompt: &str) -> Result<(), String> {
        let script = format!("do shell script \"{command}\" with prompt \"{}\" with administrator privileges", prompt.replace(['"', '\\'], ""));
        let out = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        Err(if err.contains("-128") || err.contains("canceled") { "Cancelled".into() } else { err.trim().to_string() })
    }

    /// Turn system sleep off. Returns whether DevCat did (false: it was already off).
    pub fn disable_sleep() -> Result<bool, String> {
        if !cfg!(target_os = "macos") {
            return Err("Keeping awake with the lid closed is only on macOS".into());
        }
        if sleep_disabled() {
            return Ok(false);
        }
        admin("/usr/bin/pmset -a disablesleep 1", "DevCat wants to keep your Mac awake with the lid closed (Keep awake). It turns sleep back on when keep awake ends.")?;
        if let Some(m) = MARKER.get() {
            let _ = std::fs::write(m, "pmset -a disablesleep 1\n");
        }
        Ok(true)
    }

    /// Turn system sleep back on, if DevCat turned it off.
    pub fn restore(prompt: &str) -> Result<(), String> {
        let Some(m) = MARKER.get().filter(|m| m.exists()) else { return Ok(()) };
        if sleep_disabled() {
            admin("/usr/bin/pmset -a disablesleep 0", prompt)?;
        }
        let _ = std::fs::remove_file(m);
        Ok(())
    }
}

/// Turn system sleep back on after keep awake with the lid closed (off the async runtime: it
/// waits for the password).
async fn restore_lid() -> Result<(), String> {
    tokio::task::spawn_blocking(|| lid::restore("Keep awake is off: enter your password to let your Mac sleep with the lid closed again."))
        .await
        .map_err(|e| e.to_string())?
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

/// Turn keep-awake on (for `minutes`, or until turned off) or off. `lid`: also with the lid closed
/// (macOS; asks for the admin password); None keeps what's on now. Off, or on without the lid,
/// turns system sleep back on if keep awake had turned it off.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn keep_awake_set(on: bool, minutes: Option<u32>, lid: Option<bool>) -> AppResult<KeepAwakeState> {
    let was_lid = HOLDER.lock().state.lid;
    let want_lid = on && lid.unwrap_or(was_lid);
    // the lid first: if the password is refused, nothing else changes
    if want_lid && !was_lid {
        tokio::task::spawn_blocking(lid::disable_sleep)
            .await
            .map_err(|e| AppError::msg(e.to_string()))?
            .map_err(|e| AppError::msg(if e == "Cancelled" { "Keep awake with the lid closed needs your password: it wasn't turned on.".to_string() } else { format!("Couldn't keep the Mac awake with the lid closed: {e}") }))?;
    }
    if was_lid
        && !want_lid
        && let Err(e) = restore_lid().await
    {
        // still off: say so, and keep showing it (turning keep awake off again retries)
        HOLDER.lock().state.lid = true;
        broadcast(&HOLDER.lock().state.clone());
        return Err(AppError::msg(format!("Your Mac still won't sleep with the lid closed ({e}). Turn keep awake off again to retry, or run `sudo pmset -a disablesleep 0`.")));
    }
    let (state, generation) = {
        let mut h = HOLDER.lock();
        if on {
            let (rtx, rrx) = mpsc::channel();
            h.tx.send(Cmd::On(rtx)).map_err(|_| AppError::msg("Keep-awake service stopped"))?;
            rrx.recv_timeout(Duration::from_secs(5))
                .map_err(|_| AppError::msg("Keep-awake did not respond"))?
                .map_err(|e| AppError::msg(format!("Could not keep the system awake: {e}")))?;
            let until = minutes.filter(|m| *m > 0).map(|m| chrono::Utc::now().timestamp_millis() + i64::from(m) * 60_000);
            h.state = KeepAwakeState { on: true, until, lid: want_lid };
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
                let lid = h.state.lid;
                h.state = KeepAwakeState::default();
                h.generation += 1;
                (h.state.clone(), lid)
            };
            broadcast(&off.0);
            if off.1 && let Err(e) = restore_lid().await {
                HOLDER.lock().state.lid = true;
                broadcast(&HOLDER.lock().state.clone());
                crate::notify::job("Keep awake", &format!("Your Mac still won't sleep with the lid closed ({e}). Open DevCat and turn keep awake off to retry."), true).await;
            }
        });
    }
    Ok(state)
}

/// Release on app exit (the OS also drops assertions when the process ends), and let the Mac sleep
/// with the lid closed again.
pub fn shutdown() {
    let lid = {
        let h = HOLDER.lock();
        let _ = h.tx.send(Cmd::Off);
        h.state.lid
    };
    if lid {
        // (if this is refused, the next start asks again)
        let _ = lid::restore("DevCat is quitting: enter your password to let your Mac sleep with the lid closed again.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_whether_system_sleep_is_off() {
        assert!(lid::parse_sleep_disabled(" sleep                1\n SleepDisabled        1\n"));
        assert!(!lid::parse_sleep_disabled(" SleepDisabled        0\n"));
        assert!(!lid::parse_sleep_disabled(" sleep 1\n"));
    }

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
