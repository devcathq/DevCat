//! DevCat Agent tools over the user's DevCat notes (Markdown, each in a project or in General):
//! search and read them, and write one. Reading asks in "Ask first" like a query (the text goes to
//! the model's provider, and notes can hold private things); writing asks like an edit outside the
//! project. Open windows reload their notes after a write.
use crate::notes::Note;
use crate::state::St;
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{Registry, Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Notes listed per search; the model narrows the search for more.
const MAX_LISTED: usize = 30;

pub fn add(tools: &mut Registry) {
    if St::try_get().is_some() {
        tools.add(Notes);
        tools.add(NoteWrite);
    }
}

/// First non-empty line without Markdown markers (as the Notes panel shows it).
fn title(body: &str) -> String {
    let line = body.lines().map(|l| strip_marker(l.trim())).find(|l| !l.is_empty());
    line.map(|l| l.chars().take(120).collect()).unwrap_or_else(|| "Untitled note".into())
}

fn strip_marker(l: &str) -> &str {
    let l = l.trim_start_matches('#').trim_start();
    let l = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).or_else(|| l.strip_prefix("> ")).unwrap_or(l);
    l.strip_prefix("[ ] ").or_else(|| l.strip_prefix("[x] ")).or_else(|| l.strip_prefix("[X] ")).unwrap_or(l).trim()
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Project names by id.
fn projects(st: &St) -> HashMap<String, String> {
    st.projects.list().unwrap_or_default().into_iter().map(|p| (p.id, p.name)).collect()
}

fn project_name(names: &HashMap<String, String>, id: &Option<String>) -> String {
    id.as_ref().and_then(|id| names.get(id).cloned()).unwrap_or_else(|| "General".into())
}

/// The project the model names: Ok(None) for General, Err when there's no such project.
fn project_id(names: &HashMap<String, String>, want: &str) -> Result<Option<String>, ToolOutput> {
    if want.eq_ignore_ascii_case("general") {
        return Ok(None);
    }
    if let Some((id, _)) = names.iter().find(|(id, name)| *id == want || name.eq_ignore_ascii_case(want)) {
        return Ok(Some(id.clone()));
    }
    let mut all: Vec<&str> = names.values().map(String::as_str).collect();
    all.sort();
    Err(ToolOutput::err(format!("There's no DevCat project called \"{want}\". Projects: General{}.", all.iter().map(|n| format!(", {n}")).collect::<String>())))
}

fn arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

// ---------- devcat_notes

struct Notes;

#[async_trait]
impl Tool for Notes {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "devcat_notes".into(),
            description: "Search or read the user's DevCat notes (Markdown notes they keep in DevCat, each in a project or in General). Without `id`: the notes containing every word of `query` (all notes when it's left out), newest first, with id, project, date and title. With `id`: that note's full text. `project` limits the search to one project by name (\"General\" for notes without one).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "project": { "type": "string" },
                    "id": { "type": "string" }
                }
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        match (arg(input, "id"), arg(input, "query")) {
            (Some(id), _) => format!("note {id}"),
            (None, Some(q)) => q.to_string(),
            (None, None) => "all notes".into(),
        }
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(st) = St::try_get() else { return ToolOutput::err("Notes need the DevCat app.") };
        let all = match st.notes.list() {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(format!("Couldn't read the notes: {e}")),
        };
        let names = projects(&st);
        if let Some(id) = arg(&input, "id") {
            return match all.iter().find(|n| n.id == id) {
                Some(n) => ToolOutput::ok(format!("Note {} · {} · updated {}\n\n{}", n.id, project_name(&names, &n.project_id), when(n.updated_at), n.body)),
                None => ToolOutput::err(format!("There's no note with id {id}; search with `devcat_notes` first.")),
            };
        }
        let only = match arg(&input, "project").map(|p| project_id(&names, p)) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => return e,
            None => None,
        };
        let words: Vec<String> = arg(&input, "query").map(|q| q.to_lowercase().split_whitespace().map(String::from).collect()).unwrap_or_default();
        let mut found: Vec<&Note> = all
            .iter()
            .filter(|n| only.as_ref().is_none_or(|p| &n.project_id == p))
            .filter(|n| {
                let body = n.body.to_lowercase();
                words.iter().all(|w| body.contains(w.as_str()))
            })
            .collect();
        found.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        if found.is_empty() {
            return ToolOutput::ok(if all.is_empty() { "The user has no DevCat notes.".to_string() } else { "No note matches.".to_string() });
        }
        let mut lines: Vec<String> = found
            .iter()
            .take(MAX_LISTED)
            .map(|n| {
                let mut l = format!("- {} · {} · {} · {}", n.id, project_name(&names, &n.project_id), when(n.updated_at), title(&n.body));
                if let Some(w) = words.first()
                    && let Some(s) = snippet(&n.body, w)
                {
                    l.push_str(&format!("\n  {s}"));
                }
                l
            })
            .collect();
        if found.len() > MAX_LISTED {
            lines.push(format!("({} more: narrow the search)", found.len() - MAX_LISTED));
        }
        ToolOutput::ok(lines.join("\n"))
    }
}

/// About 80 characters of `body` around the first `word` (lowercase), on one line.
fn snippet(body: &str, word: &str) -> Option<String> {
    let flat: Vec<char> = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    // lowercase char by char, so positions match `flat`
    let lower: Vec<char> = flat.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let w: Vec<char> = word.chars().collect();
    let at = lower.windows(w.len().max(1)).position(|win| win == w.as_slice())?;
    let (from, to) = (at.saturating_sub(30), (at + w.len() + 50).min(flat.len()));
    let text: String = flat[from..to].iter().collect();
    Some(format!("{}{text}{}", if from > 0 { "…" } else { "" }, if to < flat.len() { "…" } else { "" }))
}

// ---------- devcat_note_write

struct NoteWrite;

#[async_trait]
impl Tool for NoteWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "devcat_note_write".into(),
            description: "Create a DevCat note, or replace the text of one (`id` from `devcat_notes`; read it first so nothing is lost). Markdown; the first line is its title. `project` puts a new note in a project by name (General when left out); on an existing note it moves it there. Write notes the user asks for, not scratch work.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "body": { "type": "string" },
                    "id": { "type": "string" },
                    "project": { "type": "string" }
                },
                "required": ["body"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::EditOutside
    }
    fn summary(&self, input: &Value) -> String {
        let t = title(input.get("body").and_then(Value::as_str).unwrap_or_default());
        let place = arg(input, "project").map(|p| format!(" (in {p})")).unwrap_or_default();
        if arg(input, "id").is_some() { format!("Update note: {t}{place}") } else { format!("New note: {t}{place}") }
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(st) = St::try_get() else { return ToolOutput::err("Notes need the DevCat app.") };
        let Some(body) = input.get("body").and_then(Value::as_str).filter(|b| !b.trim().is_empty()) else {
            return ToolOutput::err("Missing `body`: the note's Markdown text.");
        };
        let names = projects(&st);
        let project = match arg(&input, "project").map(|p| project_id(&names, p)) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => return e,
            None => None,
        };
        let note = match arg(&input, "id") {
            Some(id) => match st.notes.list().ok().and_then(|all| all.into_iter().find(|n| n.id == id)) {
                Some(old) => Note { body: body.to_string(), project_id: project.unwrap_or(old.project_id.clone()), ..old },
                None => return ToolOutput::err(format!("There's no note with id {id}; leave `id` out to create one.")),
            },
            None => Note { id: String::new(), project_id: project.flatten(), body: body.to_string(), pinned: false, created_at: 0, updated_at: 0 },
        };
        match crate::notes::save_checked(&st.notes, &st.projects, note) {
            Ok(saved) => {
                // every window reloads its notes (the same message a window sends after a save)
                crate::events::emit("devcat://window-sync", json!({ "kind": "notes", "from": "agent" }));
                ToolOutput::ok(format!("Saved note {} · {} · {}", saved.id, project_name(&names, &saved.project_id), title(&saved.body)))
            }
            Err(e) => ToolOutput::err(format!("Couldn't save the note: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_snippets() {
        assert_eq!(title("\n## Deploy steps\nrun it"), "Deploy steps");
        assert_eq!(title("- [x] done item"), "done item");
        assert_eq!(title("   \n"), "Untitled note");
        assert_eq!(snippet("Line one\nthe Staging password is in vault", "staging").as_deref(), Some("Line one the Staging password is in vault"));
        assert_eq!(snippet("nothing here", "staging"), None);
    }

    #[test]
    fn projects_by_name_or_general() {
        let names: HashMap<String, String> = [("p1".to_string(), "Nexus".to_string())].into();
        assert_eq!(project_id(&names, "nexus").ok(), Some(Some("p1".into())));
        assert_eq!(project_id(&names, "General").ok(), Some(None));
        assert!(project_id(&names, "Other").unwrap_err().content.contains("Projects: General, Nexus"));
        assert_eq!(project_name(&names, &Some("p1".into())), "Nexus");
        assert_eq!(project_name(&names, &None), "General");
    }
}
