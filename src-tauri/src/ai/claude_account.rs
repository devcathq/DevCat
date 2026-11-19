//! Several Claude Code logins: a Claude Code connection with "its own login" runs with
//! `CLAUDE_CONFIG_DIR=<data>/claude-accounts/<id>`, where Claude Code keeps that account's login
//! (`.claude.json`, and the credentials: the keychain entry it names after the folder on macOS,
//! `.credentials.json` elsewhere).
//!
//! Everything else stays the user's one Claude Code: the folder links back to the main config
//! (`~/.claude`) for the transcripts (`projects`, so the Sessions list, resume and memory sync see
//! every account's chats, and a chat started on one account goes on with another), settings,
//! `CLAUDE.md`, skills, agents, commands and plugins. Nothing in the main config is changed,
//! except that `projects` is created there when it doesn't exist yet.
use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};

/// Folders shared with the main config (created there when missing only for `projects`).
const SHARED_DIRS: [&str; 7] = ["projects", "skills", "agents", "commands", "plugins", "output-styles", "hooks"];
/// Files shared with the main config, when it has them.
const SHARED_FILES: [&str; 3] = ["settings.json", "CLAUDE.md", "keybindings.json"];

/// Claude Code's own config folder: `$CLAUDE_CONFIG_DIR` or `~/.claude`.
pub fn main_dir() -> PathBuf {
    crate::claude::sessions::config_dir()
}

/// The `.claude.json` Claude Code keeps its login and per-folder state in for a config folder:
/// inside a `CLAUDE_CONFIG_DIR`, else `~/.claude.json` next to `~/.claude`.
pub fn state_file(config_dir: &Path) -> PathBuf {
    let default = dirs::home_dir().map(|h| h.join(".claude"));
    match (default, std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty())) {
        (Some(d), None) if d == config_dir => d.with_file_name(".claude.json"),
        _ => config_dir.join(".claude.json"),
    }
}

/// The email of the account logged in with a config folder (None = not logged in, or an API key).
pub fn email(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(state_file(config_dir)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.pointer("/oauthAccount/emailAddress").and_then(|e| e.as_str()).map(str::to_string).filter(|e| !e.is_empty())
}

/// Make (or complete) an account's config folder: links to what's shared with `main`. Links
/// already there, and files Claude Code wrote there itself, are left alone.
pub fn prepare(dir: &Path, main: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::create_dir_all(main.join("projects"))?;
    for name in SHARED_DIRS {
        let (target, at) = (main.join(name), dir.join(name));
        if target.is_dir() && at.symlink_metadata().is_err() {
            link_dir(&target, &at).map_err(|e| AppError::msg(format!("Couldn't link {} to {}: {e}", at.display(), target.display())))?;
        }
    }
    for name in SHARED_FILES {
        let (target, at) = (main.join(name), dir.join(name));
        if target.is_file() && at.symlink_metadata().is_err() {
            // a file that can't be linked: the account gets its own once Claude Code writes one
            let _ = link_file(&target, &at);
        }
    }
    Ok(())
}

/// Delete an account's folder (its login). The links go first, one by one, so nothing of the
/// main config they point at can be removed with it.
pub fn remove(dir: &Path) -> AppResult<()> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    for name in SHARED_DIRS.iter().chain(SHARED_FILES.iter()) {
        let at = dir.join(name);
        let Ok(m) = at.symlink_metadata() else { continue };
        if m.file_type().is_symlink() {
            // a link to a folder is removed as a folder on Windows, as a file elsewhere
            std::fs::remove_file(&at).or_else(|_| std::fs::remove_dir(&at))?;
        } else if m.is_dir() {
            // not a link (it couldn't be made): never empty a folder that may be shared
            return Err(AppError::msg(format!("{} isn't a link: delete {} yourself", at.display(), dir.display())));
        }
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[cfg(unix)]
fn link_dir(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(unix)]
fn link_file(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

/// A symlink needs Developer Mode on Windows: else a junction, which doesn't.
#[cfg(windows)]
fn link_dir(target: &Path, at: &Path) -> std::io::Result<()> {
    if std::os::windows::fs::symlink_dir(target, at).is_ok() {
        return Ok(());
    }
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(at)
        .arg(target)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

/// A symlink, else a hard link (same drive; an editor that replaces the file breaks it).
#[cfg(windows)]
fn link_file(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, at).or_else(|_| std::fs::hard_link(target, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("devcat-claude-acct-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn shares_the_main_config_and_removes_only_the_account() {
        let (main, acct) = (tmp(), tmp().join("a1"));
        std::fs::create_dir_all(main.join("skills/x")).unwrap();
        std::fs::write(main.join("skills/x/SKILL.md"), "hi").unwrap();
        std::fs::write(main.join("settings.json"), "{}").unwrap();
        prepare(&acct, &main).unwrap();
        // projects is made in the main config and shared; what the main config lacks isn't linked
        assert!(main.join("projects").is_dir());
        assert!(acct.join("projects").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(acct.join("skills/x/SKILL.md")).unwrap(), "hi");
        assert!(acct.join("settings.json").is_file());
        assert!(acct.join("agents").symlink_metadata().is_err());
        // a transcript written through the account lands in the main config
        std::fs::create_dir_all(acct.join("projects/-tmp-x")).unwrap();
        std::fs::write(acct.join("projects/-tmp-x/s.jsonl"), "{}").unwrap();
        assert!(main.join("projects/-tmp-x/s.jsonl").is_file());
        // again: nothing changes; a file Claude Code wrote there itself stays
        std::fs::write(acct.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"a@b.c"}}"#).unwrap();
        prepare(&acct, &main).unwrap();
        assert_eq!(email(&acct).as_deref(), Some("a@b.c"));

        remove(&acct).unwrap();
        assert!(acct.symlink_metadata().is_err());
        assert!(main.join("projects/-tmp-x/s.jsonl").is_file());
        assert!(main.join("skills/x/SKILL.md").is_file() && main.join("settings.json").is_file());
        let _ = std::fs::remove_dir_all(main);
    }

    #[test]
    fn refuses_to_empty_a_real_shared_folder() {
        let (main, acct) = (tmp(), tmp().join("a2"));
        std::fs::create_dir_all(acct.join("projects")).unwrap();
        std::fs::write(acct.join("projects/keep"), "x").unwrap();
        prepare(&acct, &main).unwrap(); // already there (not a link): left alone
        assert!(remove(&acct).is_err());
        assert!(acct.join("projects/keep").is_file());
        let _ = std::fs::remove_dir_all(main);
        let _ = std::fs::remove_dir_all(acct);
    }
}
