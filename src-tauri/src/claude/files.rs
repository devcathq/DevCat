//! Claude workspace: lazy file tree listing and file previews.
use super::git::status_index;
use super::*;
use crate::error::{AppError, AppResult};
use base64::Engine;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const TEXT_CAP: u64 = 2 * 1024 * 1024;
const IMAGE_CAP: u64 = 30 * 1024 * 1024;
const PDF_CAP: u64 = 50 * 1024 * 1024;
const SHEET_FILE_CAP: u64 = 100 * 1024 * 1024;
const DOCX_FILE_CAP: u64 = 100 * 1024 * 1024;
const SHEET_MAX_ROWS: usize = 5000;
const SHEET_MAX_COLS: usize = 200;
const SNIFF_LEN: usize = 8192;

// ---------- fs_list ----------

/// Nearest ancestor (inclusive) containing a `.git` dir or file.
fn find_repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// Whether `dir` (or one of its ancestors below `root`) is excluded by a .gitignore/.ignore
/// or .git/info/exclude. The `ignore` walker only matches direct children, so children of an
/// ignored directory would otherwise show up as not ignored.
fn inside_ignored_dir(dir: &Path, root: &Path) -> bool {
    use ignore::gitignore::Gitignore;
    if dir == root || !dir.starts_with(root) {
        return false;
    }
    let mut matchers: Vec<Gitignore> = Vec::new();
    // deepest first: deeper ignore files take precedence
    for anc in dir.parent().into_iter().flat_map(Path::ancestors) {
        for name in [".ignore", ".gitignore"] {
            let f = anc.join(name);
            if f.is_file() {
                matchers.push(Gitignore::new(&f).0);
            }
        }
        if anc == root {
            break;
        }
    }
    let exclude = root.join(".git").join("info").join("exclude");
    if exclude.is_file() {
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        b.add(&exclude);
        if let Ok(gi) = b.build() {
            matchers.push(gi);
        }
    }
    for m in &matchers {
        let r = m.matched_path_or_any_parents(dir, true);
        if r.is_ignore() {
            return true;
        }
        if r.is_whitelist() {
            return false;
        }
    }
    false
}

/// Names in `dir` that survive .gitignore/.ignore/global excludes.
fn visible_names(dir: &Path) -> HashSet<std::ffi::OsString> {
    ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .parents(true)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| e.file_name().to_os_string())
        .collect()
}

fn rfc3339(t: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
}

struct RawEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: Option<String>,
    ignored: bool,
    is_repo: bool,
}

fn list_dir(dir: &Path, show_ignored: bool) -> AppResult<(Vec<RawEntry>, Option<(PathBuf, String)>)> {
    let rd = fs::read_dir(dir).map_err(|e| AppError::msg(format!("Cannot read {}: {e}", dir.display())))?;
    let canon = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let repo = find_repo_root(&canon);
    let ancestor_ignored = repo.as_deref().map(|r| inside_ignored_dir(&canon, r)).unwrap_or(false);
    let visible = visible_names(dir);
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name_os = ent.file_name();
        if name_os == ".git" {
            continue;
        }
        let ignored = ancestor_ignored || !visible.contains(&name_os);
        if ignored && !show_ignored {
            continue;
        }
        let path = ent.path();
        // follow symlinks for type/size, fall back to the link itself when broken
        let Ok(meta) = fs::metadata(&path).or_else(|_| fs::symlink_metadata(&path)) else { continue };
        out.push(RawEntry {
            name: name_os.to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            is_repo: meta.is_dir() && path.join(".git").exists(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok().map(rfc3339),
            path,
            ignored,
        });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
    // repo root + this dir's path relative to it ("" for the root)
    let rel = repo.and_then(|r| {
        let rel = canon.strip_prefix(&r).ok()?.to_string_lossy().replace('\\', "/");
        Some((r, rel))
    });
    Ok((out, rel))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fs_list(dir: String, show_ignored: Option<bool>) -> AppResult<Vec<FileEntry>> {
    let show_ignored = show_ignored.unwrap_or(false);
    if let Some(t) = super::remote::Target::parse(&dir) {
        return super::remote::fs_list(&t, show_ignored).await;
    }
    let dir_path = PathBuf::from(&dir);
    let (raw, repo) = crate::rt::spawn_blocking(move || list_dir(&dir_path, show_ignored))
        .await
        .map_err(|e| AppError::msg(e.to_string()))??;
    let status = match &repo {
        Some((root, _)) => status_index(root).await,
        None => None,
    };
    Ok(raw
        .into_iter()
        .map(|e| {
            let git = match (&status, &repo) {
                (Some(idx), Some((_, rel))) if !e.ignored => {
                    let child = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
                    idx.lookup(&child, e.is_dir)
                }
                _ => None,
            };
            FileEntry {
                name: e.name,
                path: e.path.to_string_lossy().into_owned(),
                is_dir: e.is_dir,
                size: e.size,
                modified: e.modified,
                git,
                ignored: e.ignored,
                is_repo: e.is_repo,
            }
        })
        .collect())
}

// ---------- fs_preview ----------

fn preview(kind: &str, path: &str, size: u64) -> FilePreview {
    FilePreview {
        kind: kind.into(),
        path: path.into(),
        mime: None,
        size,
        text: None,
        language: None,
        data_base64: None,
        sheets: None,
        truncated: false,
        editable: false,
        readonly_reason: None,
        version: None,
        line_ending: None,
    }
}

/// Monaco language id from file name / extension (both lowercase).
fn language_for(name: &str, ext: &str) -> &'static str {
    if name == "dockerfile" || name == "containerfile" || name.starts_with("dockerfile.") || ext == "dockerfile" {
        return "dockerfile";
    }
    if name == ".env" || name.starts_with(".env.") || name == ".editorconfig" || name == ".npmrc" || name == ".gitconfig" {
        return "ini";
    }
    match ext {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        "py" | "pyw" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "m" | "mm" => "cpp",
        "cs" => "csharp",
        "fs" | "fsx" => "fsharp",
        "scala" | "sc" => "scala",
        "dart" => "dart",
        "lua" => "lua",
        "r" => "r",
        "pl" | "pm" => "perl",
        "sql" => "sql",
        "json" | "jsonc" | "json5" | "webmanifest" => "json",
        "yaml" | "yml" => "yaml",
        "toml" | "ini" | "cfg" | "conf" | "properties" | "env" => "ini",
        "xml" | "svg" | "plist" | "xsd" | "xsl" | "csproj" | "fsproj" | "props" => "xml",
        "html" | "htm" | "vue" | "svelte" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "sh" | "bash" | "zsh" | "fish" | "ksh" => "shell",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "bat",
        "graphql" | "gql" => "graphql",
        "proto" => "protobuf",
        "tf" | "hcl" | "tfvars" => "hcl",
        "md" | "mdx" | "markdown" => "markdown",
        "dockerfile" => "dockerfile",
        "ex" | "exs" => "elixir",
        "clj" | "cljs" | "edn" => "clojure",
        "sol" => "sol",
        _ => "plaintext",
    }
}

fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" | "svgz" => "image/svg+xml",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// Magic-number sniffing for files whose extension didn't decide the kind.
fn sniff_mime(head: &[u8]) -> Option<&'static str> {
    const SIGS: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"%PDF-", "application/pdf"),
        (b"BM", "image/bmp"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1f\x8b", "application/gzip"),
        (b"\x7fELF", "application/x-elf"),
    ];
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    SIGS.iter().find(|(sig, _)| head.starts_with(sig)).map(|(_, m)| *m)
}

fn binary_mime(ext: &str) -> &'static str {
    match ext {
        "zip" | "jar" | "war" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "doc" => "application/msword",
        "ppt" | "pptx" => "application/vnd.ms-powerpoint",
        "sqlite" | "db" => "application/vnd.sqlite3",
        "exe" | "dll" => "application/vnd.microsoft.portable-executable",
        _ => "application/octet-stream",
    }
}

fn read_head(path: &Path, n: usize) -> AppResult<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    fs::File::open(path)?.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_capped(path: &Path, cap: u64) -> AppResult<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    fs::File::open(path)?.take(cap).read_to_end(&mut buf)?;
    let truncated = fs::metadata(path).map(|m| m.len() > cap).unwrap_or(false);
    Ok((buf, truncated))
}

fn base64_preview(kind: &str, path: &str, size: u64, mime: &str, cap: u64) -> AppResult<FilePreview> {
    let mut p = preview(kind, path, size);
    p.mime = Some(mime.into());
    if size > cap {
        p.kind = "too_large".into();
        return Ok(p);
    }
    p.data_base64 = Some(base64::engine::general_purpose::STANDARD.encode(fs::read(path)?));
    Ok(p)
}

fn text_preview(kind: &str, path: &str, size: u64, language: Option<&str>, mime: &str) -> AppResult<FilePreview> {
    let (bytes, truncated) = read_capped(Path::new(path), TEXT_CAP)?;
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let mut p = preview(kind, path, size);
    let valid_utf8 = std::str::from_utf8(bytes).is_ok();
    let text = String::from_utf8_lossy(bytes).into_owned();
    p.line_ending = Some(line_ending(&text).into());
    p.text = Some(text);
    p.language = language.map(Into::into);
    p.mime = Some(mime.into());
    p.truncated = truncated;
    if kind == "text" {
        p.readonly_reason = if truncated {
            Some(format!("This file is too large to edit here (over {} MB); only its beginning is shown.", TEXT_CAP / (1024 * 1024)))
        } else if !valid_utf8 {
            Some("This file isn't valid UTF-8 text, so it can't be edited here without corrupting it.".into())
        } else if fs::OpenOptions::new().write(true).open(path).is_err() {
            Some("You don't have permission to write this file.".into())
        } else {
            None
        };
        p.editable = p.readonly_reason.is_none();
    }
    Ok(p)
}

/// Dominant line ending of `text`: "crlf" when most line breaks are CRLF, else "lf".
fn line_ending(text: &str) -> &'static str {
    let lf = text.matches('\n').count();
    let crlf = text.matches("\r\n").count();
    if crlf * 2 > lf { "crlf" } else { "lf" }
}

/// Files up to this size also get a content hash in their version token.
const VERSION_HASH_MAX: u64 = 16 * 1024 * 1024;

/// Version token for conflict detection: modification time (ns) + length, plus a content hash
/// for files small enough to edit. mtime alone misses same-size rewrites that land within one
/// timestamp tick (coarse-grained filesystems, or two saves in quick succession).
fn version_of(path: &Path, meta: &fs::Metadata) -> String {
    use sha2::{Digest, Sha256};
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let hash = (meta.is_file() && meta.len() <= VERSION_HASH_MAX)
        .then(|| fs::read(path).ok())
        .flatten()
        .map(|b| hex::encode(&Sha256::digest(&b)[..8]));
    match hash {
        Some(h) => format!("{mtime}-{}-{h}", meta.len()),
        None => format!("{mtime}-{}", meta.len()),
    }
}

fn format_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn format_cell(c: &calamine::Data) -> String {
    use calamine::Data;
    match c {
        Data::Empty => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => format_float(*f),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Data::Error(e) => e.to_string(),
        Data::DateTime(dt) => {
            if dt.is_duration() {
                let secs = dt.as_duration().map(|d| d.num_seconds()).unwrap_or(0);
                return format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
            }
            match dt.as_datetime() {
                // time-only values sit on the Excel epoch
                Some(d) if dt.as_f64() < 1.0 => d.format("%H:%M:%S").to_string(),
                Some(d) if d.time() == chrono::NaiveTime::MIN => d.format("%Y-%m-%d").to_string(),
                Some(d) => d.format("%Y-%m-%dT%H:%M:%S").to_string(),
                None => format_float(dt.as_f64()),
            }
        }
    }
}

fn read_sheets(path: &Path) -> AppResult<(Vec<SheetData>, bool)> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| AppError::msg(format!("Cannot read spreadsheet: {e}")))?;
    let mut sheets = Vec::new();
    let mut any_truncated = false;
    for name in wb.sheet_names() {
        let range = match wb.worksheet_range(&name) {
            Ok(r) => r,
            // chart sheets etc. have no cell range
            Err(_) => {
                sheets.push(SheetData { name, rows: vec![], truncated: false });
                continue;
            }
        };
        // keep absolute cell positions: pad up to the range's top-left corner
        let (r0, c0) = range.start().map(|(r, c)| (r as usize, c as usize)).unwrap_or((0, 0));
        let total_rows = if range.is_empty() { 0 } else { r0 + range.height() };
        let total_cols = if range.is_empty() { 0 } else { c0 + range.width() };
        let (nrows, ncols) = (total_rows.min(SHEET_MAX_ROWS), total_cols.min(SHEET_MAX_COLS));
        let mut rows = vec![vec![String::new(); ncols]; r0.min(nrows)];
        for row in range.rows().take(nrows.saturating_sub(r0)) {
            let mut out = vec![String::new(); c0.min(ncols)];
            out.extend(row.iter().take(ncols.saturating_sub(c0)).map(format_cell));
            rows.push(out);
        }
        let truncated = total_rows > SHEET_MAX_ROWS || total_cols > SHEET_MAX_COLS;
        any_truncated |= truncated;
        sheets.push(SheetData { name, rows, truncated });
    }
    Ok((sheets, any_truncated))
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text from WordprocessingML: `<w:t>` runs, tabs/breaks, a newline per paragraph.
fn docx_xml_to_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        if in_text {
            out.push_str(&xml_unescape(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let self_closing = tag.ends_with('/');
        let name = tag.trim_end_matches('/').split(|c: char| c.is_whitespace()).next().unwrap_or("");
        match name {
            "w:t" if !self_closing => in_text = true,
            "/w:t" => in_text = false,
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            "/w:p" => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn read_docx(path: &Path) -> AppResult<String> {
    let mut zip = zip::ZipArchive::new(fs::File::open(path)?).map_err(|e| AppError::msg(format!("Cannot read document: {e}")))?;
    let entry = zip.by_name("word/document.xml").map_err(|e| AppError::msg(format!("Not a Word document: {e}")))?;
    let mut xml = String::new();
    entry.take(64 * 1024 * 1024).read_to_string(&mut xml)?;
    Ok(docx_xml_to_text(&xml))
}

pub(super) fn preview_sync(path: &str) -> AppResult<FilePreview> {
    let meta = fs::metadata(path).map_err(|e| AppError::msg(format!("Cannot open {path}: {e}")))?;
    if meta.is_dir() {
        return Err(AppError::msg(format!("{path} is a directory")));
    }
    // taken before reading: a write racing the read makes the token stale (→ conflict), never newer
    let mut pv = preview_kind(path, meta.len())?;
    pv.version = Some(version_of(Path::new(path), &meta));
    Ok(pv)
}

fn preview_kind(path: &str, size: u64) -> AppResult<FilePreview> {
    let p = Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();

    if let Some(mime) = image_mime(&ext) {
        return base64_preview("image", path, size, mime, IMAGE_CAP);
    }
    match ext.as_str() {
        "pdf" => return base64_preview("pdf", path, size, "application/pdf", PDF_CAP),
        "md" | "mdx" | "markdown" => return text_preview("markdown", path, size, Some("markdown"), "text/markdown"),
        "csv" => return text_preview("csv", path, size, None, "text/csv"),
        "tsv" => return text_preview("csv", path, size, None, "text/tab-separated-values"),
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => {
            let mut pv = preview("sheet", path, size);
            pv.mime = Some(
                if ext == "ods" { "application/vnd.oasis.opendocument.spreadsheet" } else { "application/vnd.ms-excel" }.into(),
            );
            if size > SHEET_FILE_CAP {
                pv.kind = "too_large".into();
                return Ok(pv);
            }
            let (sheets, truncated) = read_sheets(p)?;
            pv.sheets = Some(sheets);
            pv.truncated = truncated;
            return Ok(pv);
        }
        "docx" => {
            let mut pv = preview("docx", path, size);
            pv.mime = Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document".into());
            if size > DOCX_FILE_CAP {
                pv.kind = "too_large".into();
                return Ok(pv);
            }
            let mut text = read_docx(p)?;
            if text.len() as u64 > TEXT_CAP {
                let mut cut = TEXT_CAP as usize;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                pv.truncated = true;
            }
            pv.text = Some(text);
            return Ok(pv);
        }
        _ => {}
    }

    let head = read_head(p, SNIFF_LEN)?;
    match sniff_mime(&head) {
        Some("application/pdf") => return base64_preview("pdf", path, size, "application/pdf", PDF_CAP),
        Some(m) if m.starts_with("image/") => return base64_preview("image", path, size, m, IMAGE_CAP),
        _ => {}
    }
    if head.contains(&0) {
        let mut pv = preview("binary", path, size);
        pv.mime = Some(sniff_mime(&head).unwrap_or_else(|| binary_mime(&ext)).into());
        return Ok(pv);
    }
    text_preview("text", path, size, Some(language_for(&name, &ext)), "text/plain")
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fs_preview(path: String) -> AppResult<FilePreview> {
    if let Some(t) = super::remote::Target::parse(&path) {
        return super::remote::fs_preview(&t).await;
    }
    crate::rt::spawn_blocking(move || preview_sync(&path)).await.map_err(|e| AppError::msg(e.to_string()))?
}

// ---------- fs_write_text ----------

/// Error prefix the frontend matches to offer Overwrite / Reload.
const CONFLICT: &str = "CONFLICT:";

pub(super) fn normalize_eol(content: &str, eol: Option<&str>) -> String {
    match eol {
        Some("crlf") => content.replace("\r\n", "\n").replace('\n', "\r\n"),
        Some("lf") => content.replace("\r\n", "\n"),
        _ => content.to_string(),
    }
}

fn write_text_sync(path: &str, content: &str, expected_version: Option<&str>, eol: Option<&str>) -> AppResult<String> {
    let given = Path::new(path);
    if !given.is_absolute() {
        return Err(AppError::msg("Path must be absolute"));
    }
    // follow symlinks like the preview does, so the link itself survives the rename
    let target = fs::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
    let existing = match fs::metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::msg(format!("Cannot open {path}: {e}"))),
    };
    match (&existing, expected_version) {
        (Some(m), _) if !m.is_file() => return Err(AppError::msg(format!("{path} is not a regular file"))),
        (Some(m), Some(v)) if version_of(&target, m) != v => {
            return Err(AppError::msg(format!("{CONFLICT} {path} was changed on disk since it was opened.")))
        }
        (None, Some(_)) => return Err(AppError::msg(format!("{CONFLICT} {path} was deleted or moved since it was opened."))),
        _ => {}
    }
    // the rename only needs a writable directory; don't let it bypass a read-only file
    if existing.is_some() && fs::OpenOptions::new().write(true).open(&target).is_err() {
        return Err(AppError::msg(format!("You don't have permission to write {path}")));
    }

    let mut bytes = Vec::with_capacity(content.len() + 3);
    // keep a UTF-8 BOM the preview stripped
    if existing.is_some() && read_head(&target, 3).map(|h| h == b"\xef\xbb\xbf").unwrap_or(false) && !content.starts_with('\u{feff}') {
        bytes.extend_from_slice(b"\xef\xbb\xbf");
    }
    bytes.extend_from_slice(normalize_eol(content, eol).as_bytes());

    let dir = target.parent().ok_or_else(|| AppError::msg("Invalid path"))?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.devcat-{}.tmp", uuid::Uuid::new_v4().simple()));
    let atomic = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        if let Some(m) = &existing {
            fs::set_permissions(&tmp, m.permissions())?;
        }
        fs::rename(&tmp, &target)
    })();
    if let Err(e) = atomic {
        let _ = fs::remove_file(&tmp);
        // directory not writable (but the file may be): fall back to an in-place write
        if e.kind() != std::io::ErrorKind::PermissionDenied || existing.is_none() {
            return Err(AppError::msg(format!("Cannot save {path}: {e}")));
        }
        fs::write(&target, &bytes).map_err(|e| AppError::msg(format!("Cannot save {path}: {e}")))?;
    }
    Ok(version_of(&target, &fs::metadata(&target)?))
}

/// Save text to a file atomically (temp file + rename, original permissions kept).
/// `expected_version` (from `FilePreview.version`) guards against overwriting changes made on
/// disk meanwhile — mismatches fail with a "CONFLICT:" error; pass None to overwrite (or
/// recreate a deleted file). `line_ending` "lf" / "crlf" normalizes line breaks. Returns the
/// new version token.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fs_write_text(path: String, content: String, expected_version: Option<String>, line_ending: Option<String>) -> AppResult<String> {
    if let Some(t) = super::remote::Target::parse(&path) {
        return super::remote::fs_write_text(&t, &content, expected_version.as_deref(), line_ending.as_deref()).await;
    }
    crate::rt::spawn_blocking(move || write_text_sync(&path, &content, expected_version.as_deref(), line_ending.as_deref()))
        .await
        .map_err(|e| AppError::msg(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::super::git::tests::TempRepo;
    use super::*;
    use std::io::Write;

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn entry<'a>(entries: &'a [FileEntry], name: &str) -> &'a FileEntry {
        entries.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("{name} missing"))
    }

    #[tokio::test]
    async fn list_respects_gitignore_and_marks_git() {
        let repo = TempRepo::new("fslist");
        repo.write(".gitignore", "node_modules/\n*.log\n");
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("src/lib.rs", "\n");
        repo.write("README.md", "# hi\n");
        repo.write("b.txt", "b\n");
        repo.write("node_modules/pkg/index.js", "x\n");
        repo.write("debug.log", "log\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.write("src/main.rs", "fn main() { changed() }\n");
        repo.write("new.txt", "new\n");
        repo.write("newdir/a/x.txt", "x\n");

        let list = fs_list(repo.cwd(), None).await.unwrap();
        // dirs first, then case-insensitive names; dotfiles shown, .git hidden, ignored hidden
        assert_eq!(names(&list), vec!["newdir", "src", ".gitignore", "b.txt", "new.txt", "README.md"]);
        assert_eq!(entry(&list, "src").git.as_deref(), Some("M"));
        assert!(entry(&list, "src").is_dir);
        assert_eq!(entry(&list, "new.txt").git.as_deref(), Some("??"));
        assert_eq!(entry(&list, "newdir").git.as_deref(), Some("??"));
        assert_eq!(entry(&list, "b.txt").git, None);
        assert_eq!(entry(&list, "b.txt").size, 2);
        assert!(entry(&list, "b.txt").modified.is_some());

        let src = fs_list(repo.dir.join("src").to_string_lossy().into_owned(), Some(false)).await.unwrap();
        assert_eq!(names(&src), vec!["lib.rs", "main.rs"]);
        assert_eq!(entry(&src, "main.rs").git.as_deref(), Some("M"));
        assert_eq!(entry(&src, "lib.rs").git, None);
        let nested = fs_list(repo.dir.join("newdir/a").to_string_lossy().into_owned(), None).await.unwrap();
        assert_eq!(entry(&nested, "x.txt").git.as_deref(), Some("??"));

        let all = fs_list(repo.cwd(), Some(true)).await.unwrap();
        assert!(entry(&all, "node_modules").ignored);
        assert!(entry(&all, "debug.log").ignored);
        assert_eq!(entry(&all, "debug.log").git, None);
        assert!(!entry(&all, "src").ignored);
        assert!(!names(&all).contains(&".git"));

        // children of an ignored dir are ignored too
        let nm = repo.dir.join("node_modules/pkg").to_string_lossy().into_owned();
        assert!(fs_list(nm.clone(), Some(false)).await.unwrap().is_empty());
        assert!(entry(&fs_list(nm, Some(true)).await.unwrap(), "index.js").ignored);
    }

    #[tokio::test]
    async fn list_outside_repo_and_errors() {
        let dir = std::env::temp_dir().join(format!("devcat-plain-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("Sub")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("B.txt"), "b").unwrap();
        fs::write(dir.join(".hidden"), "h").unwrap();
        let list = fs_list(dir.to_string_lossy().into_owned(), None).await.unwrap();
        assert_eq!(names(&list), vec!["Sub", ".hidden", "a.txt", "B.txt"]);
        assert!(list.iter().all(|e| e.git.is_none() && !e.ignored));
        assert!(fs_list(dir.join("missing").to_string_lossy().into_owned(), None).await.is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    struct Fixtures {
        dir: PathBuf,
    }

    impl Fixtures {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("devcat-preview-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Fixtures { dir }
        }
        fn file(&self, name: &str, bytes: &[u8]) -> String {
            let p = self.dir.join(name);
            fs::write(&p, bytes).unwrap();
            p.to_string_lossy().into_owned()
        }
        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Fixtures {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn preview_text_kinds() {
        let fx = Fixtures::new();
        let ts = fs_preview(fx.file("app.tsx", b"export const x = 1;\n")).await.unwrap();
        assert_eq!((ts.kind.as_str(), ts.language.as_deref()), ("text", Some("typescript")));
        assert_eq!(ts.text.as_deref(), Some("export const x = 1;\n"));
        assert_eq!(fs_preview(fx.file("Dockerfile", b"FROM alpine\n")).await.unwrap().language.as_deref(), Some("dockerfile"));
        assert_eq!(fs_preview(fx.file("Cargo.toml", b"[a]\n")).await.unwrap().language.as_deref(), Some("ini"));
        assert_eq!(fs_preview(fx.file("Makefile", b"all:\n")).await.unwrap().language.as_deref(), Some("plaintext"));
        assert_eq!(fs_preview(fx.file("x.proto", b"syntax = \"proto3\";")).await.unwrap().language.as_deref(), Some("protobuf"));

        let md = fs_preview(fx.file("README.md", b"# Title\n")).await.unwrap();
        assert_eq!(md.kind, "markdown");
        assert_eq!(md.text.as_deref(), Some("# Title\n"));
        let csv = fs_preview(fx.file("data.tsv", b"a\tb\n1\t2\n")).await.unwrap();
        assert_eq!(csv.kind, "csv");
        assert_eq!(csv.text.as_deref(), Some("a\tb\n1\t2\n"));

        let big = vec![b'a'; (TEXT_CAP + 10) as usize];
        let t = fs_preview(fx.file("big.txt", &big)).await.unwrap();
        assert!(t.truncated);
        assert_eq!(t.text.unwrap().len() as u64, TEXT_CAP);
        assert_eq!(t.size, TEXT_CAP + 10);

        let lossy = fs_preview(fx.file("latin1.txt", b"caf\xe9\n")).await.unwrap();
        assert_eq!(lossy.kind, "text");
        assert!(lossy.text.unwrap().starts_with("caf"));

        assert!(fs_preview(fx.dir.to_string_lossy().into_owned()).await.is_err());
        assert!(fs_preview(fx.path("nope.txt")).await.is_err());
    }

    #[tokio::test]
    async fn preview_binary_kinds() {
        let fx = Fixtures::new();
        let png_bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let png = fs_preview(fx.file("img.png", png_bytes)).await.unwrap();
        assert_eq!((png.kind.as_str(), png.mime.as_deref()), ("image", Some("image/png")));
        assert_eq!(png.data_base64.as_deref(), Some(base64::engine::general_purpose::STANDARD.encode(png_bytes).as_str()));
        // sniffed without an extension
        assert_eq!(fs_preview(fx.file("noext", png_bytes)).await.unwrap().kind, "image");

        let svg = fs_preview(fx.file("logo.svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>")).await.unwrap();
        assert_eq!((svg.kind.as_str(), svg.mime.as_deref()), ("image", Some("image/svg+xml")));

        let pdf = fs_preview(fx.file("doc.pdf", b"%PDF-1.4\n%fake\n")).await.unwrap();
        assert_eq!((pdf.kind.as_str(), pdf.mime.as_deref()), ("pdf", Some("application/pdf")));
        assert!(pdf.data_base64.is_some());

        let bin = fs_preview(fx.file("blob.bin", b"\x00\x01\x02\x03")).await.unwrap();
        assert_eq!(bin.kind, "binary");
        assert_eq!(bin.size, 4);
        assert!(bin.text.is_none());
        assert_eq!(fs_preview(fx.file("a.woff2", b"wOF2\0\0")).await.unwrap().mime.as_deref(), Some("font/woff2"));
    }

    #[tokio::test]
    async fn preview_sheet() {
        let fx = Fixtures::new();
        let path = fx.path("book.xlsx");
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet().set_name("Data").unwrap();
        ws.write_string(0, 0, "name").unwrap();
        ws.write_string(0, 1, "n").unwrap();
        ws.write_string(0, 2, "when").unwrap();
        ws.write_string(1, 0, "alpha").unwrap();
        ws.write_number(1, 1, 42.0).unwrap();
        let date = rust_xlsxwriter::ExcelDateTime::from_ymd(2024, 3, 5).unwrap();
        let fmt = rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd");
        ws.write_datetime_with_format(1, 2, &date, &fmt).unwrap();
        ws.write_number(2, 1, 1.5).unwrap();
        ws.write_boolean(2, 0, true).unwrap();
        wb.add_worksheet().set_name("Second").unwrap().write_string(1, 1, "offset").unwrap();
        wb.save(&path).unwrap();

        let pv = fs_preview(path).await.unwrap();
        assert_eq!(pv.kind, "sheet");
        let sheets = pv.sheets.unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "Data");
        assert_eq!(sheets[0].rows[0], vec!["name", "n", "when"]);
        assert_eq!(sheets[0].rows[1], vec!["alpha", "42", "2024-03-05"]);
        assert_eq!(sheets[0].rows[2][..2], ["TRUE".to_string(), "1.5".to_string()]);
        assert!(!sheets[0].truncated);
        // positions preserved relative to A1
        assert_eq!(sheets[1].rows, vec![vec!["".to_string(), "".into()], vec!["".into(), "offset".into()]]);
    }

    #[tokio::test]
    async fn preview_docx() {
        let fx = Fixtures::new();
        let path = fx.path("letter.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> &amp; world</w:t></w:r></w:p><w:p><w:r><w:t>A</w:t><w:tab/><w:t>B</w:t><w:br/><w:t>C&#x41;</w:t></w:r></w:p></w:body></w:document>"#,
        )
        .unwrap();
        zip.finish().unwrap();

        let pv = fs_preview(path).await.unwrap();
        assert_eq!(pv.kind, "docx");
        assert_eq!(pv.text.as_deref(), Some("Hello & world\nA\tB\nCA\n"));
        assert!(fs_preview(fx.file("bad.docx", b"not a zip")).await.is_err());
    }

    #[tokio::test]
    async fn preview_editability() {
        let fx = Fixtures::new();
        let ok = fs_preview(fx.file("page.html", b"<p>hi</p>\n")).await.unwrap();
        assert_eq!((ok.kind.as_str(), ok.editable, ok.readonly_reason.as_deref()), ("text", true, None));
        assert_eq!(ok.line_ending.as_deref(), Some("lf"));
        assert!(ok.version.is_some());

        let crlf = fs_preview(fx.file("win.txt", b"a\r\nb\r\nc\n")).await.unwrap();
        assert_eq!(crlf.line_ending.as_deref(), Some("crlf"));

        let big = fs_preview(fx.file("big.txt", &vec![b'a'; (TEXT_CAP + 1) as usize])).await.unwrap();
        assert!(!big.editable && big.readonly_reason.unwrap().contains("too large"));
        let latin1 = fs_preview(fx.file("latin1.txt", b"caf\xe9\n")).await.unwrap();
        assert!(!latin1.editable && latin1.readonly_reason.unwrap().contains("UTF-8"));
        // preview-only kinds are never editable
        assert!(!fs_preview(fx.file("README.md", b"# x\n")).await.unwrap().editable);
        assert!(!fs_preview(fx.file("a.csv", b"a,b\n")).await.unwrap().editable);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = fx.file("ro.txt", b"x\n");
            fs::set_permissions(&ro, fs::Permissions::from_mode(0o444)).unwrap();
            let pv = fs_preview(ro.clone()).await.unwrap();
            assert!(!pv.editable && pv.readonly_reason.unwrap().contains("permission"));
            assert!(fs_write_text(ro, "y".into(), pv.version, None).await.is_err());
        }
    }

    #[tokio::test]
    async fn write_roundtrip_and_conflicts() {
        let fx = Fixtures::new();
        let path = fx.file("a.txt", b"one\n");
        let v1 = fs_preview(path.clone()).await.unwrap().version;
        let v2 = fs_write_text(path.clone(), "two\n".into(), v1.clone(), None).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        let pv = fs_preview(path.clone()).await.unwrap();
        assert_eq!((pv.text.as_deref(), pv.version.as_deref()), (Some("two\n"), Some(v2.as_str())));
        // no temp files left behind
        assert_eq!(fs::read_dir(&fx.dir).unwrap().count(), 1);

        // stale token → conflict, file untouched; None overwrites
        let err = fs_write_text(path.clone(), "three\n".into(), v1, None).await.unwrap_err().to_string();
        assert!(err.starts_with(CONFLICT), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        fs_write_text(path.clone(), "three\n".into(), None, None).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "three\n");

        // deleted since opened → conflict, unless overwriting
        let gone = fx.file("gone.txt", b"x");
        let v = fs_preview(gone.clone()).await.unwrap().version;
        fs::remove_file(&gone).unwrap();
        assert!(fs_write_text(gone.clone(), "y".into(), v, None).await.unwrap_err().to_string().starts_with(CONFLICT));
        fs_write_text(gone.clone(), "y".into(), None, None).await.unwrap();
        assert_eq!(fs::read_to_string(&gone).unwrap(), "y");

        assert!(fs_write_text(fx.dir.to_string_lossy().into_owned(), "x".into(), None, None).await.is_err());
        assert!(fs_write_text("relative.txt".into(), "x".into(), None, None).await.is_err());
    }

    #[tokio::test]
    async fn write_preserves_eol_bom_permissions_and_symlinks() {
        let fx = Fixtures::new();
        let path = fx.file("win.txt", b"\xef\xbb\xbfa\r\nb\r\n");
        let pv = fs_preview(path.clone()).await.unwrap();
        assert_eq!(pv.text.as_deref(), Some("a\r\nb\r\n"));
        fs_write_text(path.clone(), "a\nb\r\nc\n".into(), pv.version, pv.line_ending).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xef\xbb\xbfa\r\nb\r\nc\r\n");
        fs_write_text(path.clone(), "x\r\ny\r\n".into(), None, Some("lf".into())).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xef\xbb\xbfx\ny\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = fx.file("mode.txt", b"x");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
            fs_write_text(p.clone(), "y".into(), None, None).await.unwrap();
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);

            // writing through a symlink updates the target and keeps the link
            let real = fx.file("real.txt", b"r");
            let link = fx.path("link.txt");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let v = fs_preview(link.clone()).await.unwrap().version;
            fs_write_text(link.clone(), "via link".into(), v, None).await.unwrap();
            assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert_eq!(fs::read_to_string(&real).unwrap(), "via link");
        }
    }

    #[test]
    fn cell_formatting() {
        assert_eq!(format_float(3.0), "3");
        assert_eq!(format_float(-2.0), "-2");
        assert_eq!(format_float(0.25), "0.25");
        assert_eq!(xml_unescape("a &lt;b&gt; &#65; &unknown; & c"), "a <b> A &unknown; & c");
    }
}
