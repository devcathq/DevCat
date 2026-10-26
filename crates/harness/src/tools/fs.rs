//! File tools: read, write, edit (exact string replacement), glob and grep. Searches respect
//! `.gitignore` and skip hidden folders like `.git`.
use super::{Tool, ToolCtx, ToolOutput, inside, resolve, str_arg};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;

const READ_LINES: usize = 2000;
const LINE_CHARS: usize = 2000;
const MAX_HITS: usize = 200;

fn edit_kind(input: &Value, ctx: &ToolCtx) -> Kind {
    match input.get("path").and_then(Value::as_str) {
        Some(p) if inside(ctx, &resolve(ctx, p)) => Kind::Edit,
        _ => Kind::EditOutside,
    }
}

fn path_summary(input: &Value) -> String {
    input.get("path").and_then(Value::as_str).unwrap_or("?").to_string()
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// What to do with a file `read` can't show: write a small extraction script, like a person would.
fn binary_hint(path: &Path, size: usize) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let how = match ext.as_str() {
        "pdf" => "Extract its text with a script, e.g. `python -c \"import pypdf,sys; print('\n'.join(p.extract_text() or '' for p in pypdf.PdfReader(sys.argv[1]).pages))\" FILE` or `pdftotext FILE -`.",
        "docx" => "Extract its text with a script, e.g. python-docx (`docx.Document(FILE).paragraphs`).",
        "xlsx" | "xlsm" | "xls" => "Read it with a script, e.g. openpyxl or pandas (`pandas.read_excel(FILE)`), printing only the sheets and rows you need.",
        "pptx" => "Extract its text with a script, e.g. python-pptx.",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => "It's an image: if the user attached it, it's in their message for you to see; otherwise use a script (e.g. Pillow) for size or metadata.",
        "zip" | "gz" | "tar" | "7z" => "List or extract it with a script or the shell (`tar -tf`, `python -m zipfile -l`).",
        "sqlite" | "db" => "Query it with `sqlite3` or Python's sqlite3 module.",
        _ => "If you need what's inside, write a small script that converts it to text.",
    };
    format!("{} is a binary file ({size} bytes), so `read` can't show it. {how} Run scripts with `shell`; if a library is missing, ask the user before installing it.", path.display())
}

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: format!(
                "Read a text file. Returns numbered lines (`  12\\tcode`). Reads up to {READ_LINES} lines; use `offset` (1-based first line) and `limit` for more. On a folder, lists its entries."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, relative to the project or absolute" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        path_summary(input)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let path = match str_arg(&input, "path") {
            Ok(p) => resolve(ctx, p),
            Err(e) => return e,
        };
        if path.is_dir() {
            return list_dir(&path).await;
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("Can't read {}: {e}", path.display())),
        };
        if looks_binary(&bytes) {
            return ToolOutput::err(binary_hint(&path, bytes.len()));
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        let limit = input.get("limit").and_then(Value::as_u64).map(|l| l as usize).unwrap_or(READ_LINES);
        let total = text.lines().count();
        if total == 0 {
            return ToolOutput::ok("(empty file)");
        }
        if offset > total {
            return ToolOutput::err(format!("The file has only {total} lines."));
        }
        let mut out = String::new();
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let line: String = if line.chars().count() > LINE_CHARS { format!("{}…", line.chars().take(LINE_CHARS).collect::<String>()) } else { line.to_string() };
            out.push_str(&format!("{:>6}\t{line}\n", i + 1));
        }
        let last = (offset - 1 + limit).min(total);
        if last < total {
            out.push_str(&format!("\n(showing lines {offset}-{last} of {total}; use offset {} to continue)", last + 1));
        }
        ToolOutput::ok(out)
    }
}

async fn list_dir(path: &Path) -> ToolOutput {
    let mut rd = match tokio::fs::read_dir(path).await {
        Ok(r) => r,
        Err(e) => return ToolOutput::err(format!("Can't list {}: {e}", path.display())),
    };
    let mut names = Vec::new();
    while let Ok(Some(e)) = rd.next_entry().await {
        let dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        names.push(format!("{}{}", e.file_name().to_string_lossy(), if dir { "/" } else { "" }));
    }
    names.sort();
    ToolOutput::ok(if names.is_empty() { "(empty folder)".to_string() } else { names.join("\n") })
}

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Create a file, or replace a file's whole content. Creates missing folders. Prefer `edit` for changing part of an existing file.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        edit_kind(input, ctx)
    }
    fn summary(&self, input: &Value) -> String {
        path_summary(input)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let (path, content) = match (str_arg(&input, "path"), str_arg(&input, "content")) {
            (Ok(p), Ok(c)) => (resolve(ctx, p), c),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        if let Some(dir) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(dir).await {
                return ToolOutput::err(format!("Can't create {}: {e}", dir.display()));
            }
        let existed = path.exists();
        match tokio::fs::write(&path, content).await {
            Ok(()) => ToolOutput::ok(format!("{} {} ({} lines).", if existed { "Replaced" } else { "Created" }, path.display(), content.lines().count())),
            Err(e) => ToolOutput::err(format!("Can't write {}: {e}", path.display())),
        }
    }
}

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace an exact piece of text in a file. `old_string` must appear exactly once (include enough surrounding lines to make it unique), unless `replace_all` is true. Read the file first; match its indentation exactly.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        edit_kind(input, ctx)
    }
    fn summary(&self, input: &Value) -> String {
        path_summary(input)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let (path, old, new) = match (str_arg(&input, "path"), str_arg(&input, "old_string"), str_arg(&input, "new_string")) {
            (Ok(p), Ok(o), Ok(n)) => (resolve(ctx, p), o, n),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
        };
        if old.is_empty() {
            return ToolOutput::err("`old_string` is empty. Use `write` to create a file.");
        }
        if old == new {
            return ToolOutput::err("`old_string` and `new_string` are the same.");
        }
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => return ToolOutput::err(format!("Can't read {}: {e}", path.display())),
        };
        // Models write \n; keep a CRLF file CRLF.
        let crlf = text.contains("\r\n");
        let (old, new) = if crlf && !old.contains("\r\n") { (old.replace('\n', "\r\n"), new.replace('\n', "\r\n")) } else { (old.to_string(), new.to_string()) };
        let count = text.matches(&old).count();
        let replace_all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
        let updated = match (count, replace_all) {
            (0, _) => return ToolOutput::err(format!("`old_string` was not found in {}. Read the file again and copy the text exactly, including whitespace.", path.display())),
            (1, _) | (_, true) => text.replace(&old, &new),
            (n, false) => return ToolOutput::err(format!("`old_string` appears {n} times in {}. Add surrounding lines to make it unique, or set `replace_all`.", path.display())),
        };
        match tokio::fs::write(&path, updated).await {
            Ok(()) => ToolOutput::ok(format!("Edited {} ({count} replacement{}).", path.display(), if count == 1 { "" } else { "s" })),
            Err(e) => ToolOutput::err(format!("Can't write {}: {e}", path.display())),
        }
    }
}

/// Files under `root` (respecting .gitignore), with their path relative to `root`.
fn walk(root: &Path) -> impl Iterator<Item = (std::path::PathBuf, String)> {
    let root2 = root.to_path_buf();
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(move |e| {
            let rel = e.path().strip_prefix(&root2).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
            (e.into_path(), rel)
        })
}

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: format!("Find files by name pattern, e.g. `**/*.rs` or `src/**/test_*.py`. Respects .gitignore. Newest first, at most {MAX_HITS}."),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Folder to search (default: the project)" }
                },
                "required": ["pattern"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input.get("pattern").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let pattern = match str_arg(&input, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };
        let root = input.get("path").and_then(Value::as_str).map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
        let matcher = match globset::GlobBuilder::new(&pattern).literal_separator(false).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("Bad pattern: {e}")),
        };
        // a bare "*.rs" means anywhere, like most tools
        let anywhere = (!pattern.contains('/')).then(|| globset::Glob::new(&format!("**/{pattern}")).ok()).flatten().map(|g| g.compile_matcher());
        let hits = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<(std::time::SystemTime, String)> = walk(&root)
                .filter(|(_, rel)| matcher.is_match(rel) || anywhere.as_ref().is_some_and(|m| m.is_match(rel)))
                .map(|(p, rel)| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), rel))
                .collect();
            hits.sort_by_key(|h| std::cmp::Reverse(h.0));
            hits
        })
        .await
        .unwrap_or_default();
        if hits.is_empty() {
            return ToolOutput::ok("No files match.");
        }
        let more = hits.len().saturating_sub(MAX_HITS);
        let mut out = hits.into_iter().take(MAX_HITS).map(|(_, r)| r).collect::<Vec<_>>().join("\n");
        if more > 0 {
            out.push_str(&format!("\n(and {more} more; narrow the pattern)"));
        }
        ToolOutput::ok(out)
    }
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: format!("Search file contents with a regular expression (Rust regex syntax). Returns `path:line: text`, at most {MAX_HITS} matches. Respects .gitignore."),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Folder or file to search (default: the project)" },
                    "glob": { "type": "string", "description": "Only files matching this, e.g. `*.ts`" },
                    "case_insensitive": { "type": "boolean" }
                },
                "required": ["pattern"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        input.get("pattern").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let pattern = match str_arg(&input, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };
        let ci = input.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false);
        let re = match regex::RegexBuilder::new(&pattern).case_insensitive(ci).build() {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("Bad regex: {e}")),
        };
        let filter = match input.get("glob").and_then(Value::as_str) {
            Some(g) => match globset::Glob::new(&if g.contains('/') { g.to_string() } else { format!("**/{g}") }) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolOutput::err(format!("Bad glob: {e}")),
            },
            None => None,
        };
        let root = input.get("path").and_then(Value::as_str).map(|p| resolve(ctx, p)).unwrap_or_else(|| ctx.cwd.clone());
        let hits = tokio::task::spawn_blocking(move || {
            let files: Vec<(std::path::PathBuf, String)> = if root.is_file() {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                vec![(root.clone(), name)]
            } else {
                walk(&root).collect()
            };
            let mut hits = Vec::new();
            let mut total = 0usize;
            for (path, rel) in files {
                if filter.as_ref().is_some_and(|f| !f.is_match(&rel)) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                if looks_binary(&bytes) {
                    continue;
                }
                for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                    if re.is_match(line) {
                        total += 1;
                        if hits.len() < MAX_HITS {
                            let line = line.trim_end();
                            let line: String = if line.chars().count() > 300 { format!("{}…", line.chars().take(300).collect::<String>()) } else { line.to_string() };
                            hits.push(format!("{rel}:{}: {line}", i + 1));
                        }
                    }
                }
            }
            (hits, total)
        })
        .await
        .unwrap_or_default();
        let (hits, total) = hits;
        if hits.is_empty() {
            return ToolOutput::ok("No matches.");
        }
        let mut out = hits.join("\n");
        if total > MAX_HITS {
            out.push_str(&format!("\n({total} matches in all; showing the first {MAX_HITS})"));
        }
        ToolOutput::ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (std::path::PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("devcat-harness-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\r\n    println!(\"hi\");\r\n}\r\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();
        std::fs::write(dir.join("target/junk.rs"), "fn main() {}\n").unwrap();
        (dir.clone(), ToolCtx::at(dir))
    }

    #[tokio::test]
    async fn read_edit_write() {
        let (dir, ctx) = project();
        let r = Read.run(json!({ "path": "src/main.rs" }), &ctx).await;
        assert_eq!(r.content, "     1\tfn main() {\n     2\t    println!(\"hi\");\n     3\t}\n");

        // \n in the edit, CRLF in the file: still matches, file stays CRLF
        let e = Edit.run(json!({ "path": "src/main.rs", "old_string": "fn main() {\n    println!(\"hi\");", "new_string": "fn main() {\n    println!(\"hello\");" }), &ctx).await;
        assert!(!e.is_error, "{}", e.content);
        assert_eq!(std::fs::read_to_string(dir.join("src/main.rs")).unwrap(), "fn main() {\r\n    println!(\"hello\");\r\n}\r\n");

        let dup = Edit.run(json!({ "path": "src/lib.rs", "old_string": "pub fn", "new_string": "fn" }), &ctx).await;
        assert!(dup.is_error && dup.content.contains("appears 2 times"));
        let all = Edit.run(json!({ "path": "src/lib.rs", "old_string": "pub fn", "new_string": "fn", "replace_all": true }), &ctx).await;
        assert!(!all.is_error);
        let missing = Edit.run(json!({ "path": "src/lib.rs", "old_string": "nope", "new_string": "x" }), &ctx).await;
        assert!(missing.is_error && missing.content.contains("not found"));

        let w = Write.run(json!({ "path": "docs/new/readme.md", "content": "# Hi\n" }), &ctx).await;
        assert!(w.content.starts_with("Created"), "{}", w.content);
        assert_eq!(Write.kind(&json!({ "path": "../elsewhere.txt" }), &ctx), Kind::EditOutside);
        assert_eq!(Write.kind(&json!({ "path": "a.txt" }), &ctx), Kind::Edit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn glob_and_grep_respect_gitignore() {
        let (dir, ctx) = project();
        let g = Glob.run(json!({ "pattern": "*.rs" }), &ctx).await;
        let mut found: Vec<&str> = g.content.lines().collect();
        found.sort();
        assert_eq!(found, vec!["src/lib.rs", "src/main.rs"]);

        let r = Grep.run(json!({ "pattern": "fn (a|main)", "glob": "*.rs" }), &ctx).await;
        let mut lines: Vec<&str> = r.content.lines().collect();
        lines.sort();
        assert_eq!(lines, vec!["src/lib.rs:1: pub fn a() {}", "src/main.rs:1: fn main() {"]);
        let none = Grep.run(json!({ "pattern": "zzz" }), &ctx).await;
        assert_eq!(none.content, "No matches.");
        let _ = std::fs::remove_dir_all(dir);
    }
}
