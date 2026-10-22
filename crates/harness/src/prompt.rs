//! The system prompt, assembled from ordered sections: who the agent is, how to work, the
//! environment, the rules the user and the project wrote for coding agents, and the skills it can
//! load. Rules use the files other agents already read (Claude Code, Codex, Cursor, Copilot,
//! Windsurf, Cline, Gemini), so a project set up for one of them works here too.
use std::path::{Path, PathBuf};

/// Rule files and folders in the project, in this order. Folders contribute their `.md` / `.mdc`
/// files, sorted by name.
pub const PROJECT_RULES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".claude/CLAUDE.md",
    ".claude/rules",
    "GEMINI.md",
    ".cursorrules",
    ".cursor/rules",
    ".github/copilot-instructions.md",
    ".github/instructions",
    ".windsurfrules",
    ".clinerules",
];
/// Rule files in the user's home folder (their own rules for every project).
pub const USER_RULES: &[&str] = &[".claude/CLAUDE.md", ".codex/AGENTS.md"];
/// Skill folders (each skill is `<folder>/<name>/SKILL.md`), in the project and in the home folder.
pub const SKILL_DIRS: &[&str] = &[".claude/skills", ".agents/skills", ".codex/skills"];

const MAX_RULE_FILE: usize = 20_000;
const MAX_RULES: usize = 60_000;
const MAX_SKILLS: usize = 100;

const IDENTITY: &str = "You are DevCat Agent, a coding agent working in the user's project through tools. \
You read and change files and run commands to get the user's task done.";

const WORKING: &str = "\
# How to work
- Keep working until the task is done. Don't end your answer by announcing what you will do next: do it now with a tool call. End the turn only when the work is finished or you need the user.
- Look before you change: read the relevant files and search the code (`glob`, `grep`, `read`) instead of guessing.
- Make the smallest change that does the job, in the style of the surrounding code.
- Use `edit` for changes to existing files and `write` for new files. Never paste whole files back to the user.
- After changing code, run the project's checks or tests with `shell` when it makes sense, and fix what fails.
- If a tool call is refused, don't retry the same call; ask the user or try another way.
- For work with several steps, keep a checklist with `todo_write` and update it as you go.
- Use `web_fetch` to read documentation, issues or pages the user links to. Never put secrets or project contents in a URL.
- If you have `devcat_connections`, the user's databases and clusters are open to you: check a schema with `db_schema` before writing a query, read with `db_query`, `k8s_get` and `k8s_logs`. Keep queries small (LIMIT, few columns), and don't change data unless the user asked for it.
- When a decision belongs to the user (which option, naming, scope, anything destructive or hard to undo), ask with `ask_user` and offer short options. Don't ask about things you can find out yourself.
- Keep answers short. Say what you changed and what's left; don't repeat file contents.

# Files your tools can't read
For PDFs, Word, Excel, PowerPoint, archives, databases and other binary files, write a small script and run it with `shell` to extract what you need (for example Python with pypdf, python-docx, openpyxl or pandas, or `pdftotext`). Print only the part you need. If a library is missing, ask before installing it. Files the user attaches are listed in their message with their paths; attached images are shown to you directly.";

pub struct Environment<'a> {
    pub cwd: &'a Path,
    pub os: &'a str,
    pub shell: &'a str,
    pub date: &'a str,
}

/// The user's home folder.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).filter(|p| p.is_dir())
}

/// The full system prompt for a session in `env.cwd`. It only changes when the files it reads
/// change (or the date), so providers can cache it.
pub fn system_prompt(env: &Environment<'_>) -> String {
    system_prompt_with_home(env, home_dir().as_deref())
}

pub fn system_prompt_with_home(env: &Environment<'_>, home: Option<&Path>) -> String {
    let mut sections = vec![IDENTITY.to_string(), WORKING.to_string()];
    sections.push(format!(
        "# Environment\n- Project folder (the working directory): {}\n- Operating system: {}\n- Shell used by the `shell` tool: {}\n- Today's date: {}",
        env.cwd.display(),
        env.os,
        env.shell,
        env.date
    ));
    if let Some(r) = rules(env.cwd, home) {
        sections.push(r);
    }
    if let Some(s) = skills_section(env.cwd, home) {
        sections.push(s);
    }
    sections.join("\n\n")
}

/// The rule files found, as one section (project rules after the user's own).
pub fn rules(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut add = |label: String, text: String| {
        let text = text.trim();
        if text.is_empty() || total >= MAX_RULES {
            return;
        }
        let text = cut(text, MAX_RULE_FILE.min(MAX_RULES - total));
        total += text.len();
        parts.push(format!("## {label}\n{text}"));
    };
    if let Some(home) = home {
        for name in USER_RULES {
            if let Ok(t) = std::fs::read_to_string(home.join(name)) {
                add(format!("~/{name} (your rules for every project)"), t);
            }
        }
    }
    for name in PROJECT_RULES {
        let p = cwd.join(name);
        if p.is_dir() {
            for f in sorted_files(&p, &["md", "mdc"]) {
                if let Ok(t) = std::fs::read_to_string(&f) {
                    add(rel(cwd, &f), strip_front_matter(&t).to_string());
                }
            }
        } else if let Ok(t) = std::fs::read_to_string(&p) {
            add(name.to_string(), t);
        }
    }
    (!parts.is_empty()).then(|| format!("# Rules\nInstructions the user and the project wrote for coding agents. Follow them.\n\n{}", parts.join("\n\n")))
}

/// A skill: instructions for one kind of task, loaded when it's needed.
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Skills in the project (first) and in the home folder; a project skill hides a user skill of
/// the same name.
pub fn skills(cwd: &Path, home: Option<&Path>) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    let roots: Vec<PathBuf> = SKILL_DIRS.iter().map(|d| cwd.join(d)).chain(home.into_iter().flat_map(|h| SKILL_DIRS.iter().map(move |d| h.join(d)))).collect();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
        dirs.sort();
        for d in dirs {
            let file = d.join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let fm = front_matter(&text);
            let name = fm.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()).unwrap_or_else(|| d.file_name().unwrap_or_default().to_string_lossy().into_owned());
            if out.iter().any(|s| s.name == name) || out.len() >= MAX_SKILLS {
                continue;
            }
            let description = fm.iter().find(|(k, _)| k == "description").map(|(_, v)| cut(v, 300).to_string()).unwrap_or_default();
            out.push(Skill { name, description, path: file });
        }
    }
    out
}

fn skills_section(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let list = skills(cwd, home);
    if list.is_empty() {
        return None;
    }
    let lines: Vec<String> = list.iter().map(|s| format!("- {}: {} ({})", s.name, s.description, s.path.display())).collect();
    Some(format!(
        "# Skills\nSkills are instructions for particular tasks. When a task matches a skill, read its SKILL.md with `read` first (and the files it points to), then follow it.\n{}",
        lines.join("\n")
    ))
}

/// `key: value` pairs of a `---` front matter block (single-line values, quotes removed).
fn front_matter(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return vec![];
    }
    lines
        .take_while(|l| l.trim() != "---")
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        .collect()
}

fn strip_front_matter(text: &str) -> &str {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        return rest[end + 4..].trim_start_matches(['\r', '\n']);
    }
    text
}

fn sorted_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e)))
        .collect();
    v.sort();
    v
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-harness-prompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rules_from_every_agent_convention_and_the_home_folder() {
        let (proj, home) = (tmp(), tmp());
        std::fs::write(proj.join("AGENTS.md"), "Use tabs.\n").unwrap();
        std::fs::create_dir_all(proj.join(".cursor/rules")).unwrap();
        std::fs::write(proj.join(".cursor/rules/style.mdc"), "---\nglobs: *.ts\n---\nNo default exports.\n").unwrap();
        std::fs::create_dir_all(proj.join(".github")).unwrap();
        std::fs::write(proj.join(".github/copilot-instructions.md"), "Write tests.").unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/CLAUDE.md"), "Answer in English.").unwrap();
        let p = system_prompt_with_home(&Environment { cwd: &proj, os: "linux", shell: "bash", date: "2026-10-21" }, Some(&home));
        assert!(p.starts_with("You are DevCat Agent"));
        assert!(p.contains("Today's date: 2026-10-21"));
        let order = ["Answer in English.", "## AGENTS.md\nUse tabs.", "## .cursor/rules/style.mdc\nNo default exports.", "Write tests."];
        let pos: Vec<usize> = order.iter().map(|s| p.find(s).unwrap_or_else(|| panic!("missing {s}"))).collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "rules out of order");
        assert!(!p.contains("globs:"));
        let _ = (std::fs::remove_dir_all(proj), std::fs::remove_dir_all(home));
    }

    #[test]
    fn skills_listed_with_descriptions_project_first() {
        let (proj, home) = (tmp(), tmp());
        for (root, name, desc) in [(&proj, "deploy", "Ship to staging"), (&home, "deploy", "user version"), (&home, "pdf-report", "Make a PDF report")] {
            let d = root.join(".claude/skills").join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), format!("---\nname: {name}\ndescription: \"{desc}\"\n---\nSteps…")).unwrap();
        }
        let s = skills(&proj, Some(&home));
        assert_eq!(s.iter().map(|s| (s.name.as_str(), s.description.as_str())).collect::<Vec<_>>(), vec![("deploy", "Ship to staging"), ("pdf-report", "Make a PDF report")]);
        let p = system_prompt_with_home(&Environment { cwd: &proj, os: "linux", shell: "bash", date: "d" }, Some(&home));
        assert!(p.contains("# Skills") && p.contains("- pdf-report: Make a PDF report ("));
        let _ = (std::fs::remove_dir_all(proj), std::fs::remove_dir_all(home));
    }
}
