//! The text a review is saved as: one instruction line, then one line per note.

use super::{ReviewDoc, ReviewNote, Target};

/// Backticks are escaped and line breaks become spaces, so a value can't break out of its line.
fn esc(s: &str) -> String {
    s.replace('`', "\\`").replace(['\r', '\n'], " ")
}

fn prefix(doc: &ReviewDoc, note: &ReviewNote) -> String {
    let mut parts = Vec::new();
    if note.page != doc.first_page {
        parts.push(esc(&note.page));
    }
    if note.width != doc.width {
        parts.push(format!("{}w", note.width));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("[{}] ", parts.join(" "))
    }
}

/// Renders the review. `date` comes from the caller.
#[allow(dead_code)]
pub fn render(doc: &ReviewDoc, styles: bool, date: &str) -> String {
    let (title_where, header_where) = match &doc.target {
        Target::Url { origin } => {
            let host = origin.split_once("://").map_or(origin.as_str(), |(_, h)| h);
            (
                esc(&format!("{host}{}", doc.first_page)),
                esc(&format!("{origin}{}", doc.first_page)),
            )
        }
        Target::File { rel, .. } => (esc(rel), esc(rel)),
    };
    let mut out = format!(
        "UI review · {title_where} · {}\nUI review of {header_where} ({}×{}). Format: `selector` \"visible text\" (source) → comment. Search the code for the text or classes when no source is given.\n\n",
        esc(date),
        doc.width,
        doc.height
    );
    for note in &doc.notes {
        out.push_str(&format!("{}. {}`{}` ", note.n, prefix(doc, note), esc(&note.selector)));
        if !note.text.is_empty() {
            out.push_str(&format!("\"{}\" ", esc(&note.text)));
        }
        if let Some(src) = &note.source {
            out.push_str(&format!("({}) ", esc(src)));
        }
        out.push_str(&format!("→ {}\n", esc(&note.comment)));
        if let (true, Some(style)) = (styles, &note.style) {
            out.push_str(&format!("   (style: {})\n", esc(style)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(n: u32, page: &str, width: u32, sel: &str, text: &str, comment: &str) -> ReviewNote {
        ReviewNote {
            n,
            page: page.into(),
            width,
            selector: sel.into(),
            text: text.into(),
            source: None,
            style: None,
            comment: comment.into(),
        }
    }

    fn url_doc(notes: Vec<ReviewNote>) -> ReviewDoc {
        ReviewDoc {
            target: Target::Url { origin: "http://localhost:5173".into() },
            first_page: "/pricing".into(),
            width: 1280,
            height: 800,
            notes,
        }
    }

    fn spec_doc() -> ReviewDoc {
        let mut n1 = note(1, "/pricing", 1280, "h1", "Simple pricing for growing teams", "too large on mobile, cap at 32px");
        n1.source = Some("src/pages/Pricing.tsx:18".into());
        url_doc(vec![
            n1,
            note(2, "/pricing", 1280, ".cta", "Start free trial", "use our Button component, not a span"),
            note(3, "/checkout", 1280, ".card:nth-of-type(2) .price", "$12", "add \"/seat/mo\""),
        ])
    }

    #[test]
    fn format_matches_spec_example() {
        assert_eq!(
            render(&spec_doc(), false, "7 Nov 2026"),
            "UI review · localhost:5173/pricing · 7 Nov 2026\n\
UI review of http://localhost:5173/pricing (1280×800). Format: `selector` \"visible text\" (source) → comment. Search the code for the text or classes when no source is given.\n\n\
1. `h1` \"Simple pricing for growing teams\" (src/pages/Pricing.tsx:18) → too large on mobile, cap at 32px\n\
2. `.cta` \"Start free trial\" → use our Button component, not a span\n\
3. [/checkout] `.card:nth-of-type(2) .price` \"$12\" → add \"/seat/mo\"\n"
        );
    }

    #[test]
    fn styles_line_only_when_asked() {
        let mut n = note(1, "/pricing", 1280, ".price", "$12", "bigger");
        n.style = Some("220×28, 20px/700, color rgb(29,35,48), bg transparent, pad 0px".into());
        let doc = url_doc(vec![n, note(2, "/pricing", 1280, ".cta", "Go", "x")]);
        let with = render(&doc, true, "d");
        assert!(with.contains(
            "1. `.price` \"$12\" → bigger\n   (style: 220×28, 20px/700, color rgb(29,35,48), bg transparent, pad 0px)\n2. "
        ));
        assert_eq!(with.matches("(style:").count(), 1);
        assert!(!render(&doc, false, "d").contains("(style:"));
    }

    #[test]
    fn width_prefix_when_different() {
        let doc = url_doc(vec![
            note(1, "/pricing", 390, "h1", "", "a"),
            note(2, "/checkout", 390, "h1", "", "b"),
            note(3, "/checkout", 1280, "h1", "", "c"),
            note(4, "/pricing", 1280, "h1", "", "d"),
        ]);
        let out = render(&doc, false, "d");
        assert!(out.contains("1. [390w] `h1`"));
        assert!(out.contains("2. [/checkout 390w] `h1`"));
        assert!(out.contains("3. [/checkout] `h1`"));
        assert!(out.contains("4. `h1`"));
    }

    #[test]
    fn file_target_uses_relative_path() {
        let doc = ReviewDoc {
            target: Target::File { root: "/work/proj".into(), rel: "site/index.html".into() },
            first_page: "site/index.html".into(),
            width: 1024,
            height: 768,
            notes: vec![note(1, "site/index.html", 1024, "h1", "Hi", "x")],
        };
        let out = render(&doc, false, "7 Nov 2026");
        assert!(out.starts_with(
            "UI review · site/index.html · 7 Nov 2026\nUI review of site/index.html (1024×768). Format:"
        ));
        assert!(out.contains("\n\n1. `h1` \"Hi\" → x\n"));
        assert!(!out.contains("/work/proj"));
    }

    #[test]
    fn format_escapes_hostile_values() {
        let doc = url_doc(vec![note(1, "/pricing", 1280, "h1", "", "a\nb `x` → c")]);
        let out = render(&doc, false, "d");
        assert_eq!(out.lines().filter(|l| l.starts_with("1. ")).count(), 1);
        assert!(out.contains("→ a b \\`x\\` → c"));
        let doc = url_doc(vec![note(1, "/p\r\nq", 1280, "a`b\nc", "t`\r", "k")]);
        let out = render(&doc, false, "d");
        assert!(out.contains("1. [/p  q] `a\\`b c` \"t\\` \" → k\n"));
    }

    #[test]
    fn empty_text_is_omitted() {
        let doc = url_doc(vec![note(1, "/pricing", 1280, "img.logo", "", "needs alt text")]);
        assert!(render(&doc, false, "d").contains("\n1. `img.logo` → needs alt text\n"));
    }
}
