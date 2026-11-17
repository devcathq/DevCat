//! Project focus: which folders the user used with which project (a chat, terminal, file or Git
//! tab opened while the project was focused). The window keeps the list (`src/store/folderProjects.ts`)
//! and saves it here too, so a paired phone's focus shows the same folders (`focus_folders` in
//! remote/dispatch.rs). Only folder paths and project ids; it decides nothing on the computer.
use crate::error::AppResult;
use once_cell::sync::OnceCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

static DIR: OnceCell<PathBuf> = OnceCell::new();

/// Folder → the projects it belongs to.
pub type Folders = BTreeMap<String, Vec<String>>;

pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir);
}

fn path() -> Option<PathBuf> {
    DIR.get().map(|d| d.join("focus-folders.json"))
}

pub fn load() -> Folders {
    path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The window's list, whenever it changes.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn focus_folders_save(folders: Folders) -> AppResult<()> {
    let Some(p) = path() else { return Ok(()) };
    // a sane size: folder paths, a few project ids each
    let folders: Folders = folders.into_iter().filter(|(f, _)| f.len() <= 4096).take(5000).map(|(f, ids)| (f, ids.into_iter().take(50).collect())).collect();
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&folders)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}
