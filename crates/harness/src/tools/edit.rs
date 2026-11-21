//! What `edit` does to a file's text: find each `old_string` and put its `new_string` there.
//!
//! The text is looked for exactly first. When it isn't there, it's looked for again with both
//! sides *normalized*: trailing whitespace on each line is ignored, and typographic quotes, dashes
//! and unusual spaces count as their plain ASCII forms. Models often retype a line with a curly
//! quote straightened or a trailing space dropped; failing the edit for that costs a step (a new
//! `read`, a new try). The rules stay strict otherwise: a normalized match must be unique too, and
//! the parts of the file the edit doesn't cover keep their bytes (see [`Norm::original`]).
//!
//! Several changes can be made in one call: each is matched against the file as it was, they may
//! not overlap, and either all of them are made or none.

/// One change: replace `old` with `new` (every occurrence when `all`).
#[derive(Debug, Clone, Copy)]
pub struct Change<'a> {
    pub old: &'a str,
    pub new: &'a str,
    pub all: bool,
}

/// The file's new text, and what it took.
#[derive(Debug, PartialEq)]
pub struct Applied {
    pub text: String,
    /// Places replaced, over all changes.
    pub replacements: usize,
    /// Changes that were found only after normalizing (see the module docs).
    pub fuzzy: usize,
}

/// A character as normalized matching sees it: typographic quotes, dashes and spaces as their
/// plain forms. One character always stays one character, so a position in the normalized text
/// is the same character count into the original line.
fn plain(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        c => c,
    }
}

/// `s` normalized: [`plain`] characters, and each line without its trailing whitespace (a `\r`
/// included, so line endings don't matter either).
fn normalize(s: &str) -> String {
    s.split('\n').map(|l| l.chars().map(plain).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

/// A text normalized, with the way back from a position in it to one in the original.
struct Norm<'a> {
    original: &'a str,
    text: String,
    /// Per line: where it starts in `text`, and in `original`.
    lines: Vec<(usize, usize)>,
}

impl<'a> Norm<'a> {
    fn new(original: &'a str) -> Norm<'a> {
        let mut text = String::with_capacity(original.len());
        let mut lines = Vec::new();
        let mut at = 0;
        for (i, line) in original.split('\n').enumerate() {
            if i > 0 {
                text.push('\n');
            }
            lines.push((text.len(), at));
            text.push_str(line.chars().map(plain).collect::<String>().trim_end());
            at += line.len() + 1;
        }
        Norm { original, text, lines }
    }

    /// The byte offset in the original that position `p` of the normalized text stands for. A
    /// position at the end of a normalized line is the end of the original line's visible text,
    /// before its trailing whitespace.
    fn original(&self, p: usize) -> usize {
        let l = self.lines.partition_point(|(n, _)| *n <= p) - 1;
        let (n_start, o_start) = self.lines[l];
        let chars = self.text[n_start..p].chars().count();
        let line = &self.original[o_start..];
        o_start + line.char_indices().nth(chars).map_or(line.len(), |(i, _)| i)
    }
}

/// The file's text with `changes` made, or what's wrong (for the model). `label(i)` names change
/// `i` in messages (`old_string`, or `edits[2]`).
pub fn apply(text: &str, changes: &[Change], label: impl Fn(usize) -> String) -> Result<Applied, String> {
    // Models write \n; a CRLF file stays CRLF.
    let crlf = text.contains("\r\n");
    let mut norm: Option<Norm> = None;
    // (start, end, replacement, change) in the original text
    let mut spans: Vec<(usize, usize, String, usize)> = Vec::new();
    let mut fuzzy = 0;
    for (i, c) in changes.iter().enumerate() {
        let name = label(i);
        if c.old.is_empty() {
            return Err(format!("{name} is empty. Use `write` to create a file, or include the text you're replacing."));
        }
        if c.old == c.new {
            return Err(format!("{name} and its `new_string` are the same: that changes nothing."));
        }
        let (old, new) = if crlf && !c.old.contains("\r\n") { (c.old.replace('\n', "\r\n"), c.new.replace('\n', "\r\n")) } else { (c.old.to_string(), c.new.to_string()) };
        let exact: Vec<(usize, usize)> = text.match_indices(&old).map(|(s, m)| (s, s + m.len())).collect();
        let found = if !exact.is_empty() {
            exact
        } else {
            let wanted = normalize(c.old);
            if wanted.trim().is_empty() {
                Vec::new()
            } else {
                let n = norm.get_or_insert_with(|| Norm::new(text));
                // `old_string` ending in whitespace (often the next line's indentation) matches
                // only up to the end of a line: otherwise its whitespace, gone after normalizing,
                // would let it end in the middle of a word or before indentation it doesn't replace
                let open_end = c.old.split('\n').next_back().is_some_and(|l| l.ends_with(|c: char| plain(c).is_whitespace()));
                let hits: Vec<(usize, usize)> = n
                    .text
                    .match_indices(&wanted)
                    .filter(|(s, m)| !open_end || n.text[s + m.len()..].starts_with('\n') || s + m.len() == n.text.len())
                    .map(|(s, m)| (n.original(s), n.original(s + m.len())))
                    .collect();
                if !hits.is_empty() {
                    fuzzy += 1;
                }
                hits
            }
        };
        match (found.len(), c.all) {
            (0, _) => return Err(format!("{name} was not found. Read the file again and copy the text exactly, including whitespace.")),
            (1, _) | (_, true) => {}
            (n, false) => return Err(format!("{name} appears {n} times. Add surrounding lines to make it unique, or set `replace_all`.")),
        }
        spans.extend(found.into_iter().map(|(s, e)| (s, e, new.clone(), i)));
    }
    spans.sort_by_key(|s| (s.0, s.1));
    for w in spans.windows(2) {
        if w[1].0 < w[0].1 {
            let (a, b) = (w[0].3.min(w[1].3), w[0].3.max(w[1].3));
            return Err(if a == b {
                format!("{} matches overlapping text.", label(a))
            } else {
                format!("{} and {} change overlapping text: merge them into one edit.", label(a), label(b))
            });
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e, new, _) in &spans {
        out.push_str(&text[at..*s]);
        out.push_str(new);
        at = *e;
    }
    out.push_str(&text[at..]);
    Ok(Applied { text: out, replacements: spans.len(), fuzzy })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str, old: &str, new: &str) -> Result<Applied, String> {
        apply(text, &[Change { old, new, all: false }], |_| "`old_string`".into())
    }

    #[test]
    fn exact_first() {
        let a = one("let a = 1;\nlet b = 2;\n", "let b = 2;", "let b = 3;").unwrap();
        assert_eq!(a, Applied { text: "let a = 1;\nlet b = 3;\n".into(), replacements: 1, fuzzy: 0 });
        assert!(one("x\nx\n", "x", "y").unwrap_err().contains("appears 2 times"));
        assert!(one("x\n", "nope", "y").unwrap_err().contains("not found"));
        assert!(one("x\n", "", "y").unwrap_err().contains("empty"));
        assert!(one("x\n", "x", "x").unwrap_err().contains("the same"));
    }

    #[test]
    fn typographic_characters_and_trailing_whitespace_are_forgiven() {
        // the file has curly quotes, an en dash and trailing spaces; the model typed plain ones
        let file = "fn main() {\n    println!(\u{201C}it\u{2019}s 1\u{2013}2\u{201D});   \n    done();\n}\n";
        let a = one(file, "    println!(\"it's 1-2\");\n    done();", "    println!(\"ok\");\n    done();").unwrap();
        assert_eq!(a.text, "fn main() {\n    println!(\"ok\");\n    done();\n}\n");
        assert_eq!(a.fuzzy, 1);
        // the model added trailing whitespace the file doesn't have, and a non-breaking space
        let a = one("a = 1\nb = 2\n", "a\u{00A0}= 1  \nb = 2", "a = 9\nb = 2").unwrap();
        assert_eq!(a.text, "a = 9\nb = 2\n");
        // still unique, or refused
        assert!(one("x = \u{201C}a\u{201D}\nx = \"a\"  \n", "x = \"a\"", "y").is_ok(), "exact wins over normalized");
        assert!(one("x = \u{201C}a\u{201D} \ny\nx = \u{201C}a\u{201D}\n", "x = \"a\"\n", "z\n").unwrap_err().contains("appears 2 times"));
        // only whitespace isn't a thing to look for
        assert!(one("a\n  \nb\n", "  \t", "x").unwrap_err().contains("not found"));
    }

    #[test]
    fn what_the_edit_doesnt_cover_keeps_its_bytes() {
        // a match that starts mid-line: the start of the line stays as it was, curly quotes and all
        let file = "say(\u{201C}hi\u{201D}); call(\u{2018}x\u{2019});  \nnext\n";
        let a = one(file, "call('x');\nnext", "call('y');\nnext").unwrap();
        assert_eq!(a.text, "say(\u{201C}hi\u{201D}); call('y');\nnext\n");
        // a match that ends mid-line keeps that line's rest, trailing spaces included
        let a = one("one \u{2014} two   \nthree\n", "one - two", "1 - 2").unwrap();
        assert_eq!(a.text, "1 - 2   \nthree\n");
    }

    #[test]
    fn whitespace_at_the_end_of_old_string_isnt_dropped_into_a_match() {
        // the next line's indentation at the end: not a match that ends before that indentation
        assert!(one("a = \u{201C}x\u{201D}\n    b\n", "a = \"x\"\n    ", "a = 'y'\n    ").unwrap_err().contains("not found"));
        // nor a match that ends in the middle of a word
        assert!(one("ab\n", "a ", "Z").unwrap_err().contains("not found"));
        // at the end of a line it still matches (the file has no trailing space there)
        assert_eq!(one("a = \u{201C}x\u{201D}\nb\n", "a = \"x\"  ", "a = 1").unwrap().text, "a = 1\nb\n");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let file = "a\r\nb  \r\nc\r\n";
        // exact (the \n become \r\n)
        assert_eq!(one(file, "a\nb  ", "A\nB").unwrap().text, "A\r\nB\r\nc\r\n");
        // normalized (the trailing spaces and the \r don't matter)
        let a = one(file, "b\nc", "B\nC").unwrap();
        assert_eq!((a.text.as_str(), a.fuzzy), ("a\r\nB\r\nC\r\n", 1));
    }

    #[test]
    fn several_changes_at_once_or_none() {
        let file = "alpha\nbeta\ngamma\ndelta\n";
        let label = |i: usize| format!("edits[{i}]");
        let a = apply(file, &[Change { old: "gamma", new: "GAMMA", all: false }, Change { old: "alpha", new: "ALPHA", all: false }], label).unwrap();
        assert_eq!((a.text.as_str(), a.replacements), ("ALPHA\nbeta\nGAMMA\ndelta\n", 2));
        // each is matched against the file as it was, not after the change before it
        assert!(apply(file, &[Change { old: "beta", new: "BETA", all: false }, Change { old: "BETA", new: "b", all: false }], label).unwrap_err().contains("edits[1] was not found"));
        let e = apply(file, &[Change { old: "beta\ngamma", new: "x", all: false }, Change { old: "gamma\ndelta", new: "y", all: false }], label).unwrap_err();
        assert!(e.contains("edits[0] and edits[1]") && e.contains("merge"), "{e}");
        let a = apply("x x x\n", &[Change { old: "x", new: "y", all: true }], label).unwrap();
        assert_eq!((a.text.as_str(), a.replacements), ("y y y\n", 3));
    }
}
