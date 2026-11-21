//! Slash commands the user wrote: Markdown prompt templates in `.claude/commands/` (Claude Code's
//! format) or `.agents/commands/`, in the project and in the home folder, and the skills by name.
//! `/name some args` sends the template with the arguments put in: `$ARGUMENTS` (all of them),
//! `$1` … `$9` (one each, quoted words stay together). Without either, the arguments follow the
//! template. Front matter `description` and `argument-hint` show in the chat's list.
//!
//! A template is text for the model, nothing more: Claude Code's `` !`command` `` lines aren't run
//! (a file in a repository doesn't get to run programs without the user's OK), and `@file` stays
//! a mention the model can read.
use crate::prompt::{cut, front_matter, skills, strip_front_matter};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Command folders, in the project and in the home folder.
pub const COMMAND_DIRS: &[&str] = &[".claude/commands", ".agents/commands"];

/// Commands listed at most (a folder of hundreds is someone's archive, not a menu).
const MAX_COMMANDS: usize = 200;

/// A template longer than this is cut: it goes with the message to the model.
const MAX_TEMPLATE: usize = 40_000;

/// Names the chat handles itself.
const BUILT_IN: &[&str] = &["compact"];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub path: PathBuf,
    /// A skill (its SKILL.md) rather than a template.
    pub skill: bool,
}

/// The user's commands: the project's first, then the home folder's, then the skills; the first
/// of a name wins. Subfolders give `folder:name`.
pub fn commands(cwd: &Path, home: Option<&Path>) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::new();
    let roots: Vec<PathBuf> = COMMAND_DIRS.iter().map(|d| cwd.join(d)).chain(home.into_iter().flat_map(|h| COMMAND_DIRS.iter().map(move |d| h.join(d)))).collect();
    for root in roots {
        let mut files = Vec::new();
        collect(&root, &root, 0, &mut files);
        files.sort();
        for (name, path) in files {
            if out.len() >= MAX_COMMANDS || BUILT_IN.contains(&name.as_str()) || out.iter().any(|c| c.name == name) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let fm = front_matter(&text);
            let get = |k: &str| fm.iter().find(|(key, _)| key == k).map(|(_, v)| cut(v, 300).to_string()).unwrap_or_default();
            let description = Some(get("description")).filter(|d| !d.is_empty()).unwrap_or_else(|| first_line(strip_front_matter(&text)));
            out.push(Command { name, description, argument_hint: get("argument-hint"), path, skill: false });
        }
    }
    for s in skills(cwd, home) {
        if out.len() < MAX_COMMANDS && !BUILT_IN.contains(&s.name.as_str()) && !out.iter().any(|c| c.name == s.name) {
            out.push(Command { name: s.name, description: s.description, argument_hint: String::new(), path: s.path, skill: true });
        }
    }
    out
}

/// `*.md` files under `dir`, as (`a:b` name, path), two folders deep at most.
fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if p.is_dir() && depth < 2 {
            collect(root, &p, depth + 1, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
            let rel = p.strip_prefix(root).unwrap_or(&p).with_extension("");
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join(":");
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "-_:.".contains(c)) {
                out.push((name, p));
            }
        }
    }
}

fn first_line(text: &str) -> String {
    cut(text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").trim_start_matches('#').trim(), 120).to_string()
}

/// What a message that starts with one of the user's commands asks for: the command and what
/// the model gets for it (see the module docs). None when the message isn't one (no `/`, an
/// unknown name, or a built-in like `/compact`).
pub fn expand(text: &str, cwd: &Path, home: Option<&Path>) -> Option<(Command, String)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(n, a)| (n, a.trim()));
    if name.is_empty() || BUILT_IN.contains(&name) {
        return None;
    }
    let c = commands(cwd, home).into_iter().find(|c| c.name == name)?;
    if c.skill {
        let mut ask = format!("The user ran the skill \"{}\": read {} with `read` first (and the files it points to), then follow it.", c.name, c.path.display());
        if !args.is_empty() {
            ask.push_str(&format!(" What they want from it: {args}"));
        }
        return Some((c, ask));
    }
    let text = std::fs::read_to_string(&c.path).ok()?;
    let body = cut(strip_front_matter(&text).trim(), MAX_TEMPLATE).to_string();
    let filled = fill(&body, args);
    let from = c.path.display();
    Some((c.clone(), format!("The user ran their command /{} (from {from}). Do what it says:\n\n{filled}", c.name)))
}

/// The template with the arguments put in.
fn fill(template: &str, args: &str) -> String {
    let words = split_args(args);
    let mut used = false;
    let mut out = String::with_capacity(template.len() + args.len());
    let mut chars = template.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '$' {
            if template[i + 1..].starts_with("ARGUMENTS") {
                out.push_str(args);
                used = true;
                for _ in 0.."ARGUMENTS".len() {
                    chars.next();
                }
                continue;
            }
            if let Some(&(_, d)) = chars.peek()
                && let Some(n) = d.to_digit(10).filter(|n| *n > 0)
            {
                chars.next();
                out.push_str(words.get(n as usize - 1).map(String::as_str).unwrap_or(""));
                used = true;
                continue;
            }
        }
        out.push(ch);
    }
    if !used && !args.is_empty() {
        out.push_str(&format!("\n\n{args}"));
    }
    out
}

/// Arguments split on whitespace; "double" or 'single' quotes keep words together.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_from_the_project_the_home_folder_and_the_skills() {
        let dir = std::env::temp_dir().join(format!("devcat-commands-{}", uuid::Uuid::new_v4()));
        let (proj, home) = (dir.join("proj"), dir.join("home"));
        std::fs::create_dir_all(proj.join(".claude/commands/git")).unwrap();
        std::fs::create_dir_all(home.join(".claude/commands")).unwrap();
        std::fs::create_dir_all(proj.join(".claude/skills/deploy")).unwrap();
        std::fs::write(proj.join(".claude/commands/review.md"), "---\ndescription: Review a file\nargument-hint: <file> [focus]\n---\nReview $1 with care, mostly $2.\n!`git diff`\n").unwrap();
        std::fs::write(proj.join(".claude/commands/git/pr.md"), "# Open a PR\nWrite the PR for: $ARGUMENTS\n").unwrap();
        std::fs::write(home.join(".claude/commands/review.md"), "the home one, hidden by the project's").unwrap();
        std::fs::write(home.join(".claude/commands/explain.md"), "Explain it simply.").unwrap();
        std::fs::write(home.join(".claude/commands/compact.md"), "not the built-in").unwrap();
        std::fs::write(proj.join(".claude/skills/deploy/SKILL.md"), "---\nname: deploy\ndescription: Ship it\n---\nsteps").unwrap();
        let list = commands(&proj, Some(&home));
        let names: Vec<(&str, &str)> = list.iter().map(|c| (c.name.as_str(), c.description.as_str())).collect();
        assert_eq!(names, vec![("git:pr", "Open a PR"), ("review", "Review a file"), ("explain", "Explain it simply."), ("deploy", "Ship it")]);
        assert_eq!(list[1].argument_hint, "<file> [focus]");

        let (c, ask) = expand("/review src/a.rs \"error handling\"", &proj, Some(&home)).unwrap();
        assert_eq!(c.name, "review");
        assert!(ask.contains("Review src/a.rs with care, mostly error handling.") && ask.contains("!`git diff`") && !ask.contains("description:"), "{ask}");
        assert!(expand("/git:pr fix the login", &proj, Some(&home)).unwrap().1.contains("Write the PR for: fix the login"));
        // no placeholders: the arguments follow
        assert!(expand("/explain the borrow checker", &proj, Some(&home)).unwrap().1.ends_with("Explain it simply.\n\nthe borrow checker"));
        let (c, ask) = expand("/deploy to staging", &proj, Some(&home)).unwrap();
        assert!(c.skill && ask.contains("SKILL.md") && ask.ends_with("What they want from it: to staging"));
        // not commands
        for t in ["/compact", "/nope x", "review this", "/", " /"] {
            assert!(expand(t, &proj, Some(&home)).is_none(), "{t}");
        }
        assert!(commands(&proj, None).iter().all(|c| c.name != "explain"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn arguments_split_like_a_shell() {
        assert_eq!(split_args(r#"a "b c" 'd' "" e"#), vec!["a", "b c", "d", "", "e"]);
        assert_eq!(fill("x=$1 y=$3 all=$ARGUMENTS cost=$ and $0", "1 2"), "x=1 y= all=1 2 cost=$ and $0");
    }
}
