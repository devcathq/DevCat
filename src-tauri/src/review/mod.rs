//! Page review: reviewers click elements on a web page and comment; the review is saved as text.

pub mod channel;
pub mod format;
pub mod intake;
pub mod serve;
#[cfg(feature = "gui")]
mod webview;
#[cfg(feature = "gui")]
pub mod commands;
#[cfg(feature = "gui")]
pub use webview::{on_window_destroyed, set_data_dir};

pub use intake::InspectorMsg;

/// Most comments one review holds.
pub const MAX_NOTES: usize = 500;

use crate::error::{AppError, AppResult};
use crate::models::Project;
use parking_lot::Mutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// One comment on one element.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNote {
    pub n: u32,
    /// Path of the page (URL target) or the file's relative path (file target).
    pub page: String,
    /// Width of the page area when the note was made.
    pub width: u32,
    pub selector: String,
    pub text: String,
    pub source: Option<String>,
    pub style: Option<String>,
    pub comment: String,
}

/// What is being reviewed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    Url { origin: String },
    File { root: PathBuf, rel: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDoc {
    pub target: Target,
    pub first_page: String,
    pub width: u32,
    pub height: u32,
    pub notes: Vec<ReviewNote>,
}

/// A review in progress. `saved` is true right after `set_saved` and false after any change.
#[derive(Debug, Clone)]
pub struct Review {
    pub id: String,
    pub secret: String,
    pub doc: ReviewDoc,
    pub note_id: Option<String>,
    pub project_id: Option<String>,
    pub window: String,
    /// Notes on the current page whose element wasn't found (from the last Hello).
    pub missing: Vec<u32>,
    pub saved: bool,
    /// What the review webview opens first (set by `set_load_url`).
    pub load_url: String,
    /// Counts changes to the notes (not to the page size): what was shown is what is saved.
    pub rev: u64,
    /// The text last shown to the user: (rev, styles, text).
    pub rendered: Option<(u64, bool, String)>,
}

/// Reviews in progress, in memory only.
#[derive(Default)]
pub struct ReviewStore {
    reviews: Mutex<HashMap<String, Review>>,
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("OS random number generator unavailable");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Compares in constant time (for equal lengths).
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn no_review() -> AppError {
    AppError::Msg("That review isn't open any more".into())
}

fn renumber(doc: &mut ReviewDoc) {
    for (i, n) in doc.notes.iter_mut().enumerate() {
        n.n = i as u32 + 1;
    }
}

fn edit_note(r: &mut Review, n: u32, comment: &str) -> AppResult<()> {
    let comment = intake::comment_field(comment)?;
    let note = r
        .doc
        .notes
        .iter_mut()
        .find(|x| x.n == n)
        .ok_or_else(|| AppError::Msg(format!("No comment number {n}")))?;
    note.comment = comment;
    r.saved = false;
    r.rev += 1;
    Ok(())
}

fn remove_note(r: &mut Review, n: u32) -> AppResult<()> {
    let i = r
        .doc
        .notes
        .iter()
        .position(|x| x.n == n)
        .ok_or_else(|| AppError::Msg(format!("No comment number {n}")))?;
    r.doc.notes.remove(i);
    renumber(&mut r.doc);
    r.saved = false;
    r.rev += 1;
    Ok(())
}

impl ReviewStore {
    pub fn start(&self, target: Target, first_page: String, project_id: Option<String>, window: String) -> Review {
        let r = Review {
            id: format!("rv-{}", random_hex(16)),
            secret: random_hex(32),
            doc: ReviewDoc { target, first_page, width: 0, height: 0, notes: vec![] },
            note_id: None,
            project_id,
            window,
            missing: vec![],
            saved: false,
            load_url: String::new(),
            rev: 0,
            rendered: None,
        };
        self.reviews.lock().insert(r.id.clone(), r.clone());
        r
    }

    /// Parses and applies a raw message the inspector sent (caps and cleaning happen here); the secret proves it came from this review's page.
    pub fn apply(&self, id: &str, secret: &str, raw: &serde_json::Value) -> AppResult<ReviewDoc> {
        let mut map = self.reviews.lock();
        let r = map.get_mut(id).ok_or_else(no_review)?;
        if !same_secret(&r.secret, secret) {
            return Err(no_review());
        }
        let msg = intake::parse(raw)?;
        match msg {
            InspectorMsg::Hello { width, height, missing, .. } => {
                r.doc.width = width;
                r.doc.height = height;
                r.missing = missing;
            }
            InspectorMsg::Add { page, width, selector, text, source, style, comment } => {
                if r.doc.notes.len() >= MAX_NOTES {
                    return Err(AppError::Msg(format!("A review holds at most {MAX_NOTES} comments")));
                }
                let n = r.doc.notes.len() as u32 + 1;
                r.doc.notes.push(ReviewNote { n, page, width, selector, text, source, style, comment });
                r.saved = false;
                r.rev += 1;
            }
            InspectorMsg::Edit { n, comment } => edit_note(r, n, &comment)?,
            InspectorMsg::Remove { n } => remove_note(r, n)?,
        }
        Ok(r.doc.clone())
    }

    pub fn get(&self, id: &str) -> Option<Review> {
        self.reviews.lock().get(id).cloned()
    }

    pub fn edit(&self, id: &str, n: u32, comment: &str) -> AppResult<ReviewDoc> {
        let mut map = self.reviews.lock();
        let r = map.get_mut(id).ok_or_else(no_review)?;
        edit_note(r, n, comment)?;
        Ok(r.doc.clone())
    }

    pub fn remove(&self, id: &str, n: u32) -> AppResult<ReviewDoc> {
        let mut map = self.reviews.lock();
        let r = map.get_mut(id).ok_or_else(no_review)?;
        remove_note(r, n)?;
        Ok(r.doc.clone())
    }

    /// Renders the review for the user to check, and remembers that text for `shown`.
    pub fn render(&self, id: &str, styles: bool, date: &str) -> AppResult<Rendered> {
        let mut map = self.reviews.lock();
        let r = map.get_mut(id).ok_or_else(no_review)?;
        let text = format::render(&r.doc, styles, date);
        r.rendered = Some((r.rev, styles, text.clone()));
        Ok(Rendered { text, rev: r.rev })
    }

    /// The exact text the user was shown for `rev`; an error when the review changed since.
    pub fn shown(&self, id: &str, styles: bool, rev: u64) -> AppResult<String> {
        let map = self.reviews.lock();
        let r = map.get(id).ok_or_else(no_review)?;
        match &r.rendered {
            Some((at, s, text)) if *at == rev && r.rev == rev && *s == styles => Ok(text.clone()),
            _ => Err(AppError::Msg("The review changed; check it again before saving".into())),
        }
    }

    pub fn set_load_url(&self, id: &str, url: String) {
        if let Some(r) = self.reviews.lock().get_mut(id) {
            r.load_url = url;
        }
    }

    pub fn set_saved(&self, id: &str, note_id: String) {
        if let Some(r) = self.reviews.lock().get_mut(id) {
            r.note_id = Some(note_id);
            r.saved = true;
        }
    }

    pub fn close(&self, id: &str) {
        self.reviews.lock().remove(id);
    }

    /// Drops every review of a window; returns the closed ids.
    pub fn close_window(&self, label: &str) -> Vec<String> {
        let mut map = self.reviews.lock();
        let ids: Vec<String> = map.values().filter(|r| r.window == label).map(|r| r.id.clone()).collect();
        for id in &ids {
            map.remove(id);
        }
        ids
    }
}

pub fn review_store() -> &'static ReviewStore {
    static STORE: OnceLock<ReviewStore> = OnceLock::new();
    STORE.get_or_init(ReviewStore::default)
}

/// The review as text, and the revision of the notes it shows.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rendered {
    pub text: String,
    pub rev: u64,
}

/// Saves the text the user checked (`rev`, from `render`) as a note: the note it was saved to
/// before while that note is there, else a new one in the review's project (General when none,
/// or when the project is gone). Nothing is saved when the review changed since.
pub fn save_review(
    store: &ReviewStore,
    notes: &crate::notes::NoteStore,
    projects: &crate::projects::ProjectStore,
    id: &str,
    styles: bool,
    rev: u64,
) -> AppResult<crate::notes::Note> {
    use crate::notes::Note;
    let body = store.shown(id, styles, rev)?;
    let r = store.get(id).ok_or_else(no_review)?;
    let earlier = r.note_id.as_ref().and_then(|nid| notes.list().ok()?.into_iter().find(|n| &n.id == nid));
    let note = match earlier {
        Some(old) => Note { body, ..old },
        None => {
            let project_id = r.project_id.filter(|p| projects.exists(p).unwrap_or(false));
            Note { id: String::new(), project_id, body, pinned: false, created_at: 0, updated_at: 0 }
        }
    };
    let saved = crate::notes::save_checked(notes, projects, note)?;
    store.set_saved(id, saved.id.clone());
    Ok(saved)
}

/// A review about to start, worked out from what the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Start {
    pub target: Target,
    pub first_page: String,
    pub title: String,
    /// The URL to open (URL targets; file targets get theirs from the file server once the id exists).
    pub load_url: String,
}

fn refuse(msg: &str) -> AppError {
    AppError::Msg(msg.into())
}

/// An http(s) URL to review: `http://` is assumed when no scheme is given, and a URL that would
/// be a local origin to Tauri (it would get every DevCat command) is refused.
pub fn url_start(input: &str, dev_url: Option<&Url>) -> AppResult<Start> {
    let input = input.trim();
    // `localhost:5173/x` parses as scheme `localhost`: a scheme only when `://` follows, or for
    // the one-part forms (`javascript:`, `mailto:`, `data:`), which are refused below
    let has_scheme = input.contains("://")
        || input.split_once(':').is_some_and(|(scheme, rest)| {
            scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                && !rest.starts_with(|c: char| c.is_ascii_digit())
        });
    let full = if has_scheme { input.to_string() } else { format!("http://{input}") };
    let bad = || refuse("Enter a web address (http or https)");
    let url = Url::parse(&full).map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(bad());
    }
    if channel::is_local_target(&url, dev_url) {
        return Err(refuse("DevCat's own pages can't be reviewed"));
    }
    let host = url.host_str().unwrap_or_default();
    let host = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };
    Ok(Start {
        target: Target::Url { origin: url.origin().ascii_serialization() },
        first_page: url.path().to_string(),
        title: format!("Review · {host}"),
        load_url: url.to_string(),
    })
}

fn is_html(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

/// A local `.html` / `.htm` file to review. It's served from the code folder of the DevCat project
/// that holds it (so `../assets/…` works inside a repo), else from its own folder.
pub fn file_start(path: &str, projects: &[Project]) -> AppResult<Start> {
    if path.starts_with("ssh://") {
        return Err(refuse("Files on SSH hosts can't be reviewed"));
    }
    let p = Path::new(path);
    if !p.is_absolute() || !is_html(p) {
        return Err(refuse("Choose an .html file"));
    }
    let file = p.canonicalize().ok().filter(|f| f.is_file()).ok_or_else(|| refuse("That file isn't there"))?;
    // a link named `.html` may lead to any file: the file itself must be one
    if !is_html(&file) {
        return Err(refuse("Choose an .html file"));
    }
    let parent = file.parent().ok_or_else(|| refuse("That file isn't there"))?.to_path_buf();
    let root = projects
        .iter()
        .flat_map(|p| p.folders.iter())
        .filter_map(|f| Path::new(f).canonicalize().ok())
        .filter(|f| file.starts_with(f))
        .max_by_key(|f| f.components().count())
        .unwrap_or(parent);
    let inside = file.strip_prefix(&root).map_err(|_| refuse("That file isn't there"))?;
    if serve::hidden_or_secret(inside) {
        return Err(refuse("Files in hidden folders, and files that look like keys or passwords, can't be reviewed"));
    }
    let rel: Vec<String> = inside
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let rel = rel.join("/");
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Start {
        // the inspector names pages by their URL path, which is percent-encoded
        first_page: serve::encode_path(&rel),
        target: Target::File { root, rel },
        title: format!("Review · {name}"),
        load_url: String::new(),
    })
}

/// The date in a saved review: `7 Nov 2026`.
pub fn review_date(d: chrono::NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

#[cfg(test)]
mod store_tests {
    use super::*;

    fn url() -> Target {
        Target::Url { origin: "http://localhost:3000".into() }
    }
    fn add(c: &str) -> serde_json::Value {
        serde_json::json!({"kind":"add","page":"/","width":800,"selector":"div","text":"t","comment":c})
    }

    #[test]
    fn ids_and_secrets_are_random_hex() {
        let s = ReviewStore::default();
        let a = s.start(url(), "/".into(), None, "w".into());
        let b = s.start(url(), "/".into(), None, "w".into());
        assert!(a.id.starts_with("rv-") && a.id.len() == 35);
        assert_eq!(a.secret.len(), 64);
        assert_ne!(a.secret, b.secret);
    }

    #[test]
    fn apply_needs_secret() {
        let s = ReviewStore::default();
        let r = s.start(url(), "/".into(), None, "w".into());
        assert!(s.apply(&r.id, "nope", &add("x")).is_err());
        assert!(s.get(&r.id).unwrap().doc.notes.is_empty());
        assert_eq!(s.apply(&r.id, &r.secret, &add("x")).unwrap().notes.len(), 1);
    }

    #[test]
    fn remove_renumbers() {
        let s = ReviewStore::default();
        let r = s.start(url(), "/".into(), None, "w".into());
        for c in ["a", "b", "c"] {
            s.apply(&r.id, &r.secret, &add(c)).unwrap();
        }
        let d = s.apply(&r.id, &r.secret, &serde_json::json!({"kind":"remove","n":2})).unwrap();
        let v: Vec<(u32, &str)> = d.notes.iter().map(|n| (n.n, n.comment.as_str())).collect();
        assert_eq!(v, vec![(1, "a"), (2, "c")]);
        assert!(s.remove(&r.id, 9).is_err());
    }

    #[test]
    fn hello_updates_size_and_missing_but_not_saved() {
        let s = ReviewStore::default();
        let r = s.start(url(), "/".into(), None, "w".into());
        let hello = |w: u32, h: u32| serde_json::json!({"kind":"hello","page":"/","width":w,"height":h,"missing":[1]});
        s.apply(&r.id, &r.secret, &hello(800, 600)).unwrap();
        s.set_saved(&r.id, "note1".into());
        let d = s.apply(&r.id, &r.secret, &hello(400, 300)).unwrap();
        assert_eq!((d.width, d.height), (400, 300));
        let g = s.get(&r.id).unwrap();
        assert!(g.saved);
        assert_eq!(g.missing, vec![1]);
        assert_eq!(g.note_id.as_deref(), Some("note1"));
    }

    #[test]
    fn saved_clears_on_change() {
        let s = ReviewStore::default();
        let r = s.start(url(), "/".into(), None, "w".into());
        s.apply(&r.id, &r.secret, &add("a")).unwrap();
        s.set_saved(&r.id, "n".into());
        s.edit(&r.id, 1, "b").unwrap();
        assert!(!s.get(&r.id).unwrap().saved);
        assert!(s.edit(&r.id, 1, "  ").is_err());
    }

    #[test]
    fn close_window_drops_its_reviews() {
        let s = ReviewStore::default();
        let a = s.start(url(), "/".into(), None, "w1".into());
        let b = s.start(url(), "/".into(), None, "w2".into());
        assert_eq!(s.close_window("w1"), vec![a.id.clone()]);
        assert!(s.get(&a.id).is_none() && s.get(&b.id).is_some());
        s.close(&b.id);
        assert!(s.get(&b.id).is_none());
    }

    #[test]
    fn notes_are_capped() {
        let s = ReviewStore::default();
        let r = s.start(url(), "/".into(), None, "w".into());
        for _ in 0..MAX_NOTES {
            s.apply(&r.id, &r.secret, &add("x")).unwrap();
        }
        assert!(s.apply(&r.id, &r.secret, &add("x")).is_err());
        assert_eq!(s.get(&r.id).unwrap().doc.notes.len(), MAX_NOTES);
    }
}

#[cfg(test)]
mod start_tests {
    use super::*;
    use crate::models::Project;
    use reqwest::Url;

    #[test]
    fn start_url_target_parses_origin() {
        let s = url_start("localhost:5173/pricing?x=1", None).unwrap();
        assert_eq!(s.target, Target::Url { origin: "http://localhost:5173".into() });
        assert_eq!(s.first_page, "/pricing");
        assert_eq!(s.load_url, "http://localhost:5173/pricing?x=1");
        assert_eq!(s.title, "Review · localhost:5173");
        let s = url_start("https://a.b/", None).unwrap();
        assert_eq!(s.target, Target::Url { origin: "https://a.b".into() });
        assert_eq!(s.first_page, "/");
        assert_eq!(s.title, "Review · a.b");
        let s = url_start("  example.com ", None).unwrap();
        assert_eq!(s.load_url, "http://example.com/");
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "mailto:a@b.c", "data:text/html,x", "", "ftp://a.b/", "http://"] {
            assert!(url_start(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn start_refuses_local_targets() {
        let dev = Url::parse("http://localhost:1420/").unwrap();
        assert!(url_start("tauri://localhost", Some(&dev)).is_err());
        assert!(url_start("http://localhost:1420/x", Some(&dev)).is_err());
        assert!(url_start("localhost:1420", Some(&dev)).is_err());
        assert!(url_start("http://tauri.localhost/", None).is_err());
        assert!(url_start("http://localhost:1421/", Some(&dev)).is_ok());
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-review-start-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("proj/site")).unwrap();
        std::fs::create_dir_all(d.join("loose/.hidden")).unwrap();
        std::fs::write(d.join("proj/site/index.html"), "<p>").unwrap();
        std::fs::write(d.join("loose/page.HTM"), "<p>").unwrap();
        std::fs::write(d.join("loose/notes.txt"), "x").unwrap();
        std::fs::write(d.join("loose/.hidden/a.html"), "x").unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn start_file_target_root() {
        let d = tmp();
        let projects = vec![Project { id: "p".into(), folders: vec![d.join("proj").to_string_lossy().into()], ..Default::default() }];
        let s = file_start(&d.join("proj/site/index.html").to_string_lossy(), &projects).unwrap();
        assert_eq!(s.target, Target::File { root: d.join("proj"), rel: "site/index.html".into() });
        assert_eq!(s.first_page, "site/index.html");
        assert_eq!(s.title, "Review · index.html");
        let s = file_start(&d.join("loose/page.HTM").to_string_lossy(), &projects).unwrap();
        assert_eq!(s.target, Target::File { root: d.join("loose"), rel: "page.HTM".into() });
        // in no project: the file's own folder, even when that folder is hidden
        let s = file_start(&d.join("loose/.hidden/a.html").to_string_lossy(), &projects).unwrap();
        assert_eq!(s.target, Target::File { root: d.join("loose/.hidden"), rel: "a.html".into() });
        // a hidden folder inside the project's folder is never served
        let hidden = vec![Project { id: "p".into(), folders: vec![d.join("loose").to_string_lossy().into()], ..Default::default() }];
        assert!(file_start(&d.join("loose/.hidden/a.html").to_string_lossy(), &hidden).is_err());
        assert!(file_start(&d.join("loose/notes.txt").to_string_lossy(), &projects).is_err());
        assert!(file_start(&d.join("loose/missing.html").to_string_lossy(), &projects).is_err());
        assert!(file_start("site/index.html", &projects).is_err(), "relative");
        assert!(file_start("ssh://devcat-x/home/a.html", &projects).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn start_file_link_must_be_html_too() {
        let d = tmp();
        std::os::unix::fs::symlink(d.join("loose/notes.txt"), d.join("loose/link.html")).unwrap();
        assert!(file_start(&d.join("loose/link.html").to_string_lossy(), &[]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn stores() -> (PathBuf, crate::notes::NoteStore, crate::projects::ProjectStore) {
        let d = std::env::temp_dir().join(format!("devcat-review-save-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        (d.clone(), crate::notes::NoteStore::new(d.clone()), crate::projects::ProjectStore::new(d))
    }

    fn add(c: &str) -> serde_json::Value {
        serde_json::json!({"kind":"add","page":"/","width":800,"selector":"div","text":"t","comment":c})
    }

    #[test]
    fn finish_refuses_a_review_that_changed_after_it_was_shown() {
        let (d, notes, projects) = stores();
        let s = ReviewStore::default();
        let r = s.start(Target::Url { origin: "http://localhost:3000".into() }, "/".into(), None, "w".into());
        s.apply(&r.id, &r.secret, &add("seen")).unwrap();
        let shown = s.render(&r.id, false, "7 Nov 2026").unwrap();
        s.apply(&r.id, &r.secret, &add("unseen")).unwrap();
        let e = save_review(&s, &notes, &projects, &r.id, false, shown.rev).unwrap_err();
        assert!(e.to_string().contains("check it again"), "{e}");
        assert!(notes.list().unwrap().is_empty());
        assert!(!s.get(&r.id).unwrap().saved);
        // a hello (size only) doesn't count as a change
        let again = s.render(&r.id, false, "7 Nov 2026").unwrap();
        s.apply(&r.id, &r.secret, &serde_json::json!({"kind":"hello","page":"/","width":500,"height":400,"missing":[]})).unwrap();
        // other styles than were shown: refused too
        assert!(save_review(&s, &notes, &projects, &r.id, true, again.rev).is_err());
        let saved = save_review(&s, &notes, &projects, &r.id, false, again.rev).unwrap();
        assert_eq!(saved.body, again.text);
        assert!(saved.body.contains("unseen") && saved.body.contains("7 Nov 2026"));
        assert!(s.get(&r.id).unwrap().saved);
        // saved again: the same note, updated
        s.edit(&r.id, 1, "changed").unwrap();
        let third = s.render(&r.id, false, "8 Nov 2026").unwrap();
        let again_saved = save_review(&s, &notes, &projects, &r.id, false, third.rev).unwrap();
        assert_eq!(again_saved.id, saved.id);
        assert_eq!(notes.list().unwrap().len(), 1);
        assert_eq!(notes.list().unwrap()[0].body, third.text);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dates_read_like_7_nov_2026() {
        assert_eq!(review_date(chrono::NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()), "7 Nov 2026");
        assert_eq!(review_date(chrono::NaiveDate::from_ymd_opt(2027, 1, 23).unwrap()), "23 Jan 2027");
    }
}
