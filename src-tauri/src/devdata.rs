//! Dev builds ("DevCat Dev", `APP_ID` in lib.rs) and the installed DevCat's data.
//!
//! - `pnpm tauri dev --prod-data` (`DEVCAT_PROD_DATA`): run on the installed DevCat's data folder and
//!   keychain entries. Only while that DevCat is closed (scripts/tauri.mjs checks): both apps keep
//!   their data in memory and save whole files, so the last one to save would undo the other's changes.
//! - `pnpm dev:copy-data` (`devcat dev-copy-data`): copy them into DevCat Dev's own, once. What makes
//!   a DevCat someone (the rooms key, the phone-pairing key and paired phones) stays behind, and the
//!   copy doesn't auto-connect VPNs or watch ticket boards, so both apps can run side by side.
use crate::{APP_ID, PROD_ID};
use std::path::{Path, PathBuf};

/// `pnpm tauri dev --prod-data`: the installed DevCat's data folder (None otherwise, and in release builds).
pub fn prod_data_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) || std::env::var_os("DEVCAT_PROD_DATA").is_none() {
        return None;
    }
    let dir = dirs::data_dir()?.join(PROD_ID);
    eprintln!("DevCat Dev: on the installed DevCat's data ({})", dir.display());
    Some(dir)
}

/// Not copied: the rooms identity and memberships, remote access (host key, paired phones, audit),
/// and what only describes a running VPN.
fn skip(rel: &Path) -> bool {
    let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().to_string()).unwrap_or_default();
    let name = rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    matches!(first.as_str(), "rooms" | "remote" | "rooms-keeper") || name.ends_with(".run.ovpn") || name.ends_with(".log")
}

fn copy_dir(from: &Path, to: &Path, root: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let src = e.path();
        if skip(src.strip_prefix(root).unwrap_or(&src)) {
            continue;
        }
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            n += copy_dir(&src, &dst, root)?;
        } else {
            std::fs::copy(&src, &dst)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Set `key` to false in each object of the JSON list in `path` (if the file is there).
fn turn_off(path: &Path, key: &str) -> Result<(), String> {
    let Ok(raw) = std::fs::read_to_string(path) else { return Ok(()) };
    let mut v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    for it in v.as_array_mut().into_iter().flatten() {
        if let Some(o) = it.as_object_mut().filter(|o| o.contains_key(key)) {
            o.insert(key.into(), false.into());
        }
    }
    let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// `devcat dev-copy-data [--force]`
pub fn copy_main(args: &[String]) -> i32 {
    if !cfg!(debug_assertions) {
        eprintln!("dev-copy-data is for dev builds (DevCat Dev).");
        return 2;
    }
    let force = args.iter().any(|a| a == "--force");
    let Some(base) = dirs::data_dir() else {
        eprintln!("Can't find the data folder.");
        return 1;
    };
    let (from, to) = (base.join(PROD_ID), base.join(APP_ID));
    if !from.join("connections.json").exists() && !from.join("projects.json").exists() {
        eprintln!("No installed DevCat data in {}.", from.display());
        return 1;
    }
    // what every launch creates (empty rooms / remote folders) doesn't count
    let has_data = ["connections.json", "projects.json", "notes.json", "network.json", "history.json", "memory", "tickets"].iter().any(|f| to.join(f).exists());
    if has_data {
        if !force {
            eprintln!("DevCat Dev already has data in {}.\nRun `pnpm dev:copy-data --force` to replace it (the current one is kept as a backup next to it).", to.display());
            return 1;
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let backup = base.join(format!("{APP_ID}.bak-{stamp}"));
        if let Err(e) = std::fs::rename(&to, &backup) {
            eprintln!("Can't move DevCat Dev's data aside: {e}");
            return 1;
        }
        println!("DevCat Dev's data so far: {}", backup.display());
    }
    let files = match copy_dir(&from, &to, &from) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Copy failed: {e}");
            return 1;
        }
    };
    // Both apps on: one of them connects the VPNs and works on tickets, not both.
    for (file, key) in [("network.json", "autoConnect"), ("tickets/boards.json", "watch")] {
        if let Err(e) = turn_off(&to.join(file), key) {
            eprintln!("{e}");
        }
    }
    println!("Copied {files} files from {} to {}.", from.display(), to.display());
    println!("Not copied: rooms and remote access (phone pairing). VPN auto-connect and ticket watching are off in the copy.");

    #[cfg(target_os = "macos")]
    {
        println!("Copying saved passwords: macOS may ask to allow access to DevCat's keychain item.");
        match crate::secrets::copy_service(PROD_ID, APP_ID) {
            Ok(n) => println!("Copied {n} saved passwords and tokens."),
            Err(e) => {
                eprintln!("Couldn't copy the saved passwords: {e}\nThe connections are there; enter their passwords again in DevCat Dev.");
                return 1;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    println!("Saved passwords aren't copied on this OS (the keychain can't list them): enter them again in DevCat Dev, or use `pnpm tauri dev --prod-data`.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_identity_and_running_state() {
        for p in ["rooms/key", "remote/host.key", "remote", "network/a.run.ovpn", "network/a.log"] {
            assert!(skip(Path::new(p)), "{p}");
        }
        for p in ["connections.json", "network/a.ovpn", "memory/global/x.md", "tickets/boards.json"] {
            assert!(!skip(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn turns_off_auto_connect_and_watch() {
        let dir = std::env::temp_dir().join(format!("devcat-devdata-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("network.json");
        std::fs::write(&f, r#"[{"id":"a","autoConnect":true},{"id":"b"}]"#).unwrap();
        turn_off(&f, "autoConnect").unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v[0]["autoConnect"], false);
        assert!(v[1].get("autoConnect").is_none());
        turn_off(&dir.join("missing.json"), "watch").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
