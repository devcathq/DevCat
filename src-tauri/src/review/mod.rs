//! Page review: reviewers click elements on a web page and comment; the review is saved as text.

pub mod format;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
