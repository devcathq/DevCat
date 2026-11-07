//! Page review: reviewers click elements on a web page and comment; the review is saved as text.

pub mod format;
pub mod intake;

#[allow(unused_imports)]
pub use intake::InspectorMsg;

/// Most comments one review holds.
pub const MAX_NOTES: usize = 500;

use serde::{Deserialize, Serialize};
use crate::error::{AppError, AppResult};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
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
#[allow(dead_code)] // fields are read by later tasks
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
    Ok(())
}

// Later tasks (commands, file server) use the whole store.
#[allow(dead_code)]
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

#[allow(dead_code)]
pub fn review_store() -> &'static ReviewStore {
    static STORE: OnceLock<ReviewStore> = OnceLock::new();
    STORE.get_or_init(ReviewStore::default)
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
