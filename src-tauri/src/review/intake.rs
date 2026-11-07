//! Everything the inspector sends comes from the reviewed page, which is untrusted: it is
//! parsed, capped and stripped of control characters here before it reaches a review.

#![allow(dead_code)] // later review tasks use these

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};

pub const MAX_COMMENT: usize = 2000;
pub const MAX_SELECTOR: usize = 300;
pub const MAX_TEXT: usize = 40;
pub const MAX_SOURCE: usize = 200;
pub const MAX_STYLE: usize = 200;
pub const MAX_PAGE: usize = 500;
const MAX_MISSING: usize = 10_000;

/// A message from the inspector script in the reviewed page.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InspectorMsg {
    #[serde(rename_all = "camelCase")]
    Hello { page: String, width: u32, height: u32, missing: Vec<u32> },
    #[serde(rename_all = "camelCase")]
    Add {
        page: String,
        width: u32,
        selector: String,
        text: String,
        source: Option<String>,
        style: Option<String>,
        comment: String,
    },
    Edit { n: u32, comment: String },
    Remove { n: u32 },
}

/// Drop control characters (and the Unicode line/paragraph separators); with `keep_nl`, keep
/// `\n` and turn `\r\n` / `\r` into it.
pub fn clean(s: &str, keep_nl: bool) -> String {
    let s = if keep_nl { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.to_string() };
    s.chars()
        .filter(|&c| (keep_nl && c == '\n') || !(c.is_control() || c == '\u{2028}' || c == '\u{2029}'))
        .collect()
}

/// Cut to at most `max` chars (never mid code point).
fn cut(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s,
    }
}

fn field(s: &str, max: usize) -> String {
    cut(clean(s, false), max)
}

/// Visible text: at most `MAX_TEXT` chars, a cut text ends with `…`.
fn text_field(s: &str) -> String {
    let s = clean(s, false);
    if s.chars().count() <= MAX_TEXT {
        return s;
    }
    let mut t = cut(s, MAX_TEXT - 1);
    t.push('…');
    t
}

/// A comment: `\n` kept, capped, and never empty.
pub fn comment_field(s: &str) -> AppResult<String> {
    let c = cut(clean(s, true), MAX_COMMENT);
    if c.trim().is_empty() {
        return Err(AppError::Msg("The comment is empty".into()));
    }
    Ok(c)
}

fn opt(s: Option<String>, max: usize) -> Option<String> {
    s.map(|s| field(&s, max)).filter(|s| !s.is_empty())
}

pub fn parse(raw: &serde_json::Value) -> AppResult<InspectorMsg> {
    let msg: InspectorMsg = serde_json::from_value(raw.clone())
        .map_err(|e| AppError::Msg(format!("Unreadable message from the page: {e}")))?;
    Ok(match msg {
        InspectorMsg::Hello { page, width, height, mut missing } => {
            missing.truncate(MAX_MISSING);
            InspectorMsg::Hello {
                page: field(&page, MAX_PAGE),
                width: width.clamp(1, 10_000),
                height: height.clamp(1, 10_000),
                missing,
            }
        }
        InspectorMsg::Add { page, width, selector, text, source, style, comment } => {
            let selector = field(&selector, MAX_SELECTOR);
            if selector.is_empty() {
                return Err(AppError::Msg("The element has no selector".into()));
            }
            InspectorMsg::Add {
                page: field(&page, MAX_PAGE),
                width: width.clamp(1, 10_000),
                selector,
                text: text_field(&text),
                source: opt(source, MAX_SOURCE),
                style: opt(style, MAX_STYLE),
                comment: comment_field(&comment)?,
            }
        }
        InspectorMsg::Edit { n, comment } => InspectorMsg::Edit { n, comment: comment_field(&comment)? },
        m @ InspectorMsg::Remove { .. } => m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn add(over: serde_json::Value) -> serde_json::Value {
        let mut v = json!({"kind":"add","page":"/","width":800,"selector":"div","text":"hi","comment":"ok"});
        for (k, x) in over.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    #[test]
    fn intake_caps_lengths() {
        let m = parse(&add(json!({
            "comment": "c".repeat(2500), "text": "t".repeat(50), "selector": "s".repeat(400)
        })))
        .unwrap();
        let InspectorMsg::Add { comment, text, selector, .. } = m else { panic!() };
        assert_eq!(comment.chars().count(), 2000);
        assert_eq!(text.chars().count(), 40);
        assert!(text.ends_with('…'));
        assert_eq!(selector.chars().count(), 300);
    }

    #[test]
    fn intake_strips_control_chars() {
        let m = parse(&add(json!({
            "comment": "a\u{0}b\u{7}\r\nc\u{2028}d", "selector": "x\ny\u{0}", "text": "t\nu"
        })))
        .unwrap();
        let InspectorMsg::Add { comment, selector, text, .. } = m else { panic!() };
        assert_eq!(comment, "ab\ncd");
        assert_eq!(selector, "xy");
        assert_eq!(text, "tu");
    }

    #[test]
    fn intake_rejects_unknown_kind_and_empties() {
        assert!(parse(&json!({"kind":"explode"})).is_err());
        assert!(parse(&add(json!({"selector": "\u{0}"}))).is_err());
        assert!(parse(&add(json!({"comment": " \n "}))).is_err());
        assert!(parse(&json!({"kind":"edit","n":1,"comment":""})).is_err());
    }

    #[test]
    fn intake_clamps_size_and_multibyte_cut() {
        let m = parse(&json!({"kind":"hello","page":"/","width":0,"height":99999,"missing":[]})).unwrap();
        assert_eq!(m, InspectorMsg::Hello { page: "/".into(), width: 1, height: 10000, missing: vec![] });
        let InspectorMsg::Add { comment, .. } = parse(&add(json!({"comment": "é".repeat(2100)}))).unwrap() else { panic!() };
        assert_eq!(comment.chars().count(), 2000);
    }
}
