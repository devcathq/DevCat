//! Files opened with DevCat (double-click in Finder / Explorer, "Open With", the Dock icon):
//! .ovpn profiles are imported, anything else opens in the editor. macOS delivers them as
//! `RunEvent::Opened`; Windows / Linux as command-line arguments, either at launch or, while
//! DevCat runs, through the single-instance plugin. Agents open files the same way with the
//! `devcat_open_file` tool ([`show`]). They're queued until a window takes them
//! (`take_opened_files`) after the "opened-files" poke.
use parking_lot::Mutex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedFile {
    pub path: String,
    /// The project it belongs to: its git repository, else its folder (paths are shown
    /// relative to it, and git status comes from it).
    pub root: Option<String>,
    /// An agent opened it (`devcat_open_file`): it opens in a file tab whatever it is. A .ovpn
    /// isn't offered for import, since a VPN config an agent wrote must not reach the import form
    /// (agents' configs may not run programs: `network::agent_config_problem`).
    pub agent: bool,
}

static PENDING: Mutex<Vec<OpenedFile>> = Mutex::new(Vec::new());

/// VPN profiles DevCat imports when opened (everything else opens in the editor).
pub fn is_vpn_profile(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ovpn"))
}

/// The closest folder above `p` with a `.git`, else `p`'s folder.
fn project_root(p: &Path) -> Option<PathBuf> {
    let dir = p.parent()?;
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf).or_else(|| Some(dir.to_path_buf()))
}

/// File arguments from a command line (`argv[0]` is the program; flags are skipped), relative
/// ones resolved against `cwd`.
pub fn from_args(argv: &[String], cwd: Option<&Path>) -> Vec<PathBuf> {
    argv.iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            let p = PathBuf::from(a);
            match cwd {
                Some(c) if p.is_relative() => c.join(p),
                _ => p,
            }
        })
        .collect()
}

fn entry(p: PathBuf, agent: bool) -> OpenedFile {
    let p = std::fs::canonicalize(&p).unwrap_or(p);
    OpenedFile { root: project_root(&p).map(|r| plain(&r)), path: plain(&p), agent }
}

/// A path as people write it: `canonicalize` puts `\\?\` before Windows paths (kept for
/// `\\?\UNC\…` shares, which need it).
fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => s.into_owned(),
    }
}

pub fn accept(app: &AppHandle, paths: impl IntoIterator<Item = PathBuf>) {
    let found: Vec<OpenedFile> = paths.into_iter().filter(|p| p.is_file()).map(|p| entry(p, false)).collect();
    if found.is_empty() {
        return;
    }
    PENDING.lock().extend(found);
    let _ = app.emit("opened-files", ());
}

/// Opens `path` in a window for an agent (`devcat_open_file`), or says why it can't. The file's
/// content stays in the window: nothing goes back to the agent but where it opened.
pub fn show(app: &AppHandle, path: &Path) -> Result<String, String> {
    let f = checked(path)?;
    if app.webview_windows().is_empty() {
        return Err("DevCat has no window open to show it in: the user opens DevCat's window and asks again.".into());
    }
    let shown = f.path.clone();
    PENDING.lock().push(f);
    let _ = app.emit("opened-files", ());
    Ok(shown)
}

/// `path` as a file an agent opens, or why it isn't one.
fn checked(path: &Path) -> Result<OpenedFile, String> {
    if path.is_dir() {
        return Err(format!("{} is a folder: give a file in it.", path.display()));
    }
    if !path.is_file() {
        return Err(format!("There's no file at {}.", path.display()));
    }
    Ok(entry(path.to_path_buf(), true))
}

/// Files waiting to be opened (each is handed out once).
#[cfg_attr(feature = "gui", tauri::command)]
pub fn take_opened_files() -> Vec<OpenedFile> {
    std::mem::take(&mut *PENDING.lock())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_extensions() {
        let argv: Vec<String> = ["devcat", "--flag", "vpn/office.ovpn", "/abs/home.OVPN"].map(String::from).into();
        assert_eq!(from_args(&argv, Some(Path::new("/work"))), [PathBuf::from("/work/vpn/office.ovpn"), PathBuf::from("/abs/home.OVPN")]);
        assert_eq!(from_args(&argv[..1], None), Vec::<PathBuf>::new());
        assert!(is_vpn_profile(Path::new("/abs/home.OVPN")) && is_vpn_profile(Path::new("x.ovpn")));
        assert!(!is_vpn_profile(Path::new("wg0.conf")) && !is_vpn_profile(Path::new("notes.txt")) && !is_vpn_profile(Path::new("ovpn")));
    }

    #[test]
    fn root_is_the_repo_or_the_folder() {
        let d = std::env::temp_dir().join(format!("devcat-opened-{}", std::process::id()));
        std::fs::create_dir_all(d.join("repo/.git")).unwrap();
        std::fs::create_dir_all(d.join("repo/src/deep")).unwrap();
        std::fs::create_dir_all(d.join("loose")).unwrap();
        assert_eq!(project_root(&d.join("repo/src/deep/main.rs")), Some(d.join("repo")));
        assert_eq!(project_root(&d.join("loose/notes.txt")), Some(d.join("loose")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn agents_open_files_only_and_never_for_import() {
        let d = std::env::temp_dir().join(format!("devcat-opened-agent-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("office.ovpn"), "remote x").unwrap();
        let f = checked(&d.join("office.ovpn")).unwrap();
        assert!(f.agent && f.path.ends_with("office.ovpn"), "{f:?}");
        assert_eq!(serde_json::to_value(&f).unwrap()["agent"], true, "the window reads `agent`");
        assert!(checked(&d).unwrap_err().contains("is a folder"));
        assert!(checked(&d.join("gone.pdf")).unwrap_err().starts_with("There's no file at"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn paths_lose_the_verbatim_prefix_but_shares_keep_it() {
        assert_eq!(plain(Path::new(r"\\?\C:\Users\me\a.pdf")), r"C:\Users\me\a.pdf");
        assert_eq!(plain(Path::new(r"\\?\UNC\server\share\a.pdf")), r"\\?\UNC\server\share\a.pdf");
        assert_eq!(plain(Path::new("/home/me/a.pdf")), "/home/me/a.pdf");
    }

    #[test]
    fn queue_hands_each_file_out_once() {
        let f = OpenedFile { path: "/x/a.ovpn".into(), root: Some("/x".into()), agent: false };
        PENDING.lock().push(f.clone());
        assert_eq!(take_opened_files(), [f]);
        assert!(take_opened_files().is_empty());
    }
}
