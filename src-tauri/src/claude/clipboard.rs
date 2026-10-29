//! Put files on the OS clipboard (Copy in the Files tree), the way Explorer / Finder do, so pasting
//! in Slack, Teams or a mail attaches them and pasting in Explorer / Finder copies them; and paste
//! files copied there (or in the tree) into a folder of the tree.
//! Windows: `CF_HDROP`; macOS: file URLs on the general pasteboard; Linux X11: `text/uri-list`.
use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};

/// Copy local files or folders to the clipboard.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fs_copy_files(paths: Vec<String>) -> AppResult<()> {
    if paths.is_empty() {
        return Err(AppError::msg("Nothing to copy"));
    }
    for p in &paths {
        let path = Path::new(p);
        if !path.is_absolute() {
            return Err(AppError::msg("Path must be absolute"));
        }
        if !path.exists() {
            return Err(AppError::msg(format!("{p} doesn't exist")));
        }
    }
    #[cfg(feature = "gui")]
    return crate::rt::spawn_blocking(move || set_files(paths))
        .await
        .map_err(|e| AppError::msg(e.to_string()))?;
    #[cfg(not(feature = "gui"))]
    Err(AppError::msg("Copying files needs the DevCat app"))
}

#[cfg(feature = "gui")]
fn set_files(paths: Vec<String>) -> AppResult<()> {
    use clipboard_rs::{Clipboard, ClipboardContext};
    let ctx = ClipboardContext::new().map_err(|e| AppError::msg(format!("Couldn't open the clipboard: {e}")))?;
    // X11 wants file URIs; Windows and macOS take plain paths.
    #[cfg(all(unix, not(target_os = "macos")))]
    let paths = paths.iter().map(|p| file_uri(p)).collect();
    ctx.set_files(paths).map_err(|e| AppError::msg(format!("Couldn't copy to the clipboard: {e}")))
}

/// Put text on the clipboard. For copies the webview refuses because no click or key started them:
/// a program in a terminal copying with OSC 52 (Claude Code), which arrives with its output.
/// Desktop only: the clipboard is this computer's, so it stays out of the phone dispatcher.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn clipboard_write_text(text: String) -> AppResult<()> {
    #[cfg(feature = "gui")]
    return crate::rt::spawn_blocking(move || {
        use clipboard_rs::{Clipboard, ClipboardContext};
        let ctx = ClipboardContext::new().map_err(|e| AppError::msg(format!("Couldn't open the clipboard: {e}")))?;
        ctx.set_text(text).map_err(|e| AppError::msg(format!("Couldn't copy to the clipboard: {e}")))
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;
    #[cfg(not(feature = "gui"))]
    {
        let _ = text;
        Err(AppError::msg("Copying needs the DevCat app"))
    }
}

/// The clipboard's text ("" when it holds none), for Paste in a terminal's right-click menu: read here,
/// so the webview doesn't ask for clipboard permission. Desktop only, like `clipboard_write_text`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn clipboard_read_text() -> AppResult<String> {
    #[cfg(feature = "gui")]
    return crate::rt::spawn_blocking(|| {
        use clipboard_rs::{Clipboard, ClipboardContext};
        let ctx = ClipboardContext::new().map_err(|e| AppError::msg(format!("Couldn't open the clipboard: {e}")))?;
        Ok(ctx.get_text().unwrap_or_default())
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;
    #[cfg(not(feature = "gui"))]
    Err(AppError::msg("Pasting needs the DevCat app"))
}

/// Paste the OS clipboard into the folder `dir` of the workspace `root`: files and folders copied in
/// Explorer / Finder / a Linux file manager (or with Copy in the tree), or else an image (a
/// screenshot), saved as a PNG. A taken name gets " copy" (`a.ts` → `a copy.ts`, `a copy 2.ts`), so
/// nothing is overwritten. Returns the new paths. Desktop only: the clipboard is this computer's.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fs_paste(dir: String, root: String) -> AppResult<Vec<String>> {
    if super::remote::Target::parse(&dir).is_some() {
        return Err(AppError::msg("Pasting into a folder on an SSH host isn't supported yet"));
    }
    let dir = check_target(Path::new(&dir), Path::new(&root))?;
    let files = clipboard_files().await?;
    let pasted = if !files.is_empty() {
        crate::rt::spawn_blocking(move || paste_paths(&files, &dir)).await.map_err(|e| AppError::msg(e.to_string()))??
    } else {
        let Some((bytes, name)) = super::sftp::clipboard_image().await? else {
            return Err(AppError::msg("Nothing to paste: copy files, a folder or an image first"));
        };
        let target = unique_path(&dir, &name, false);
        tokio::fs::write(&target, bytes).await?;
        vec![target]
    };
    Ok(pasted.into_iter().map(|p| p.to_string_lossy().into_owned()).collect())
}

/// `dir` as it is on disk, if it's a folder inside `root`.
fn check_target(dir: &Path, root: &Path) -> AppResult<PathBuf> {
    if !dir.is_absolute() || !root.is_absolute() {
        return Err(AppError::msg("Path must be absolute"));
    }
    let real = dir.canonicalize().map_err(|e| AppError::msg(format!("{}: {e}", dir.display())))?;
    if !real.is_dir() {
        return Err(AppError::msg(format!("{} isn't a folder", dir.display())));
    }
    let root = root.canonicalize().map_err(|e| AppError::msg(format!("{}: {e}", root.display())))?;
    if !real.starts_with(&root) {
        return Err(AppError::msg("Paste only into a folder of this workspace"));
    }
    Ok(dir.to_path_buf())
}

/// Copy `sources` (files and folders) into `dir`; returns the new paths, in order.
fn paste_paths(sources: &[PathBuf], dir: &Path) -> AppResult<Vec<PathBuf>> {
    let real_dir = dir.canonicalize()?;
    // check everything first, so a bad item doesn't leave half a paste behind
    for src in sources {
        let meta = std::fs::metadata(src).map_err(|e| AppError::msg(format!("{}: {e}", src.display())))?;
        if src.file_name().is_none() {
            return Err(AppError::msg(format!("Can't paste {}", src.display())));
        }
        if meta.is_dir() && real_dir.starts_with(src.canonicalize()?) {
            return Err(AppError::msg(format!("Can't paste the folder {} into itself", src.display())));
        }
    }
    let mut out = Vec::new();
    for src in sources {
        let is_dir = src.is_dir();
        let target = unique_path(dir, &src.file_name().unwrap_or_default().to_string_lossy(), is_dir);
        if is_dir {
            copy_dir(src, &target)?;
        } else {
            std::fs::copy(src, &target).map_err(|e| AppError::msg(format!("{}: {e}", src.display())))?;
        }
        out.push(target);
    }
    Ok(out)
}

/// `dir/name`, or else the first free `name copy`, `name copy 2`, … (before a file's extension, as
/// Finder does: `a.ts` → `a copy.ts`).
fn unique_path(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let taken = |p: &Path| p.symlink_metadata().is_ok();
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => name.split_at(i),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") }))
        .find(|p| !taken(p))
        .unwrap_or(first)
}

/// Copy a folder with everything in it. Links to files are copied as files; links to folders are
/// skipped (they could loop).
fn copy_dir(src: &Path, dst: &Path) -> AppResult<()> {
    std::fs::create_dir(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let kind = e.file_type()?;
        if kind.is_dir() {
            copy_dir(&from, &to)?;
        } else if kind.is_file() || (kind.is_symlink() && from.is_file()) {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// File paths on the OS clipboard (none if it holds something else).
#[cfg(feature = "gui")]
async fn clipboard_files() -> AppResult<Vec<PathBuf>> {
    crate::rt::spawn_blocking(|| {
        use clipboard_rs::{Clipboard, ClipboardContext};
        let ctx = ClipboardContext::new().map_err(|e| AppError::msg(format!("Couldn't open the clipboard: {e}")))?;
        // Windows gives paths; macOS and X11 may give file URLs
        Ok(ctx.get_files().unwrap_or_default().into_iter().filter(|f| !f.trim().is_empty()).map(|f| PathBuf::from(from_file_uri(f.trim()))).collect())
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?
}

#[cfg(not(feature = "gui"))]
async fn clipboard_files() -> AppResult<Vec<PathBuf>> {
    Err(AppError::msg("Pasting files needs the DevCat app"))
}

/// A path from a `file://` URI (percent-decoded); anything else as it is.
#[cfg(any(test, feature = "gui"))]
fn from_file_uri(s: &str) -> String {
    let Some(rest) = s.strip_prefix("file://") else { return s.to_string() };
    let bytes = rest.strip_prefix("localhost").unwrap_or(rest).as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file://` URI for an absolute path (RFC 8089), percent-encoding everything but unreserved bytes and `/`.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_file_uris() {
        assert_eq!(file_uri("/home/me/a b/ü#1.md"), "file:///home/me/a%20b/%C3%BC%231.md");
        assert_eq!(from_file_uri(&file_uri("/home/me/a b/ü#1.md")), "/home/me/a b/ü#1.md");
        assert_eq!(from_file_uri("file://localhost/tmp/x%2"), "/tmp/x%2");
        assert_eq!(from_file_uri("C:\\Users\\me\\a.txt"), "C:\\Users\\me\\a.txt");
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-paste-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pastes_files_and_folders_with_copy_names() {
        let base = tmp();
        let (src, proj) = (base.join("src"), base.join("proj"));
        std::fs::create_dir_all(src.join("folder/inner")).unwrap();
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::fs::write(src.join("a.ts"), "a").unwrap();
        std::fs::write(src.join(".env"), "e").unwrap();
        std::fs::write(src.join("folder/inner/deep.txt"), "deep").unwrap();
        let sources = [src.join("a.ts"), src.join(".env"), src.join("folder")];
        let first = paste_paths(&sources, &proj).unwrap();
        assert_eq!(first, [proj.join("a.ts"), proj.join(".env"), proj.join("folder")]);
        assert_eq!(std::fs::read_to_string(proj.join("folder/inner/deep.txt")).unwrap(), "deep");
        // the same again: copy names, nothing overwritten
        std::fs::write(proj.join("a.ts"), "mine").unwrap();
        let second = paste_paths(&sources, &proj).unwrap();
        assert_eq!(second, [proj.join("a copy.ts"), proj.join(".env copy"), proj.join("folder copy")]);
        assert_eq!(std::fs::read_to_string(proj.join("a.ts")).unwrap(), "mine");
        assert_eq!(paste_paths(&sources[..1], &proj).unwrap(), [proj.join("a copy 2.ts")]);
        // a folder named like a file keeps its whole name
        std::fs::create_dir_all(src.join("v1.2")).unwrap();
        std::fs::create_dir_all(proj.join("v1.2")).unwrap();
        assert_eq!(paste_paths(&[src.join("v1.2")], &proj).unwrap(), [proj.join("v1.2 copy")]);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn refuses_a_folder_into_itself_and_folders_outside_the_workspace() {
        let base = tmp();
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("sub/deeper")).unwrap();
        let into_itself = paste_paths(&[proj.join("sub")], &proj.join("sub/deeper")).unwrap_err();
        assert!(into_itself.to_string().contains("into itself"), "{into_itself}");
        assert!(!proj.join("sub/deeper/sub").exists());
        assert!(paste_paths(&[base.join("missing.txt")], &proj).is_err());
        assert!(check_target(&proj.join("sub"), &proj).is_ok());
        assert!(check_target(&proj, &proj).is_ok());
        assert!(check_target(&base, &proj).unwrap_err().to_string().contains("this workspace"));
        assert!(check_target(&proj.join("sub/../../"), &proj).is_err());
        let _ = std::fs::remove_dir_all(base);
    }
}
