//! The DevCat Agent in a folder on an SSH host (`ssh://…`, see `claude/remote.rs`): the harness's
//! file tools, `shell` and `git`, with the same names and inputs, run on the host over the system
//! `ssh` (the saved session's key, jump host and password, as for its terminals and SFTP). Nothing
//! is installed there: every call is one `ssh` running a small POSIX sh script.
//!
//! What runs where: these tools act on the host; `web_fetch`, DevCat's connections, memory and
//! notes stay on this computer, like the chat itself. Hooks and skills are off in such a chat
//! (they're files and programs on this computer, for a folder that isn't here), and so is the OS
//! sandbox (it guards this computer). Desktop only, like the rest of the SSH workbench: the phone
//! dispatcher refuses these chats (AGENTS.md).
//!
//! The logic the local tools have (numbered lines, exact-string edits, which git calls only read
//! and how they're kept from starting programs) is the harness's own (`devcat_harness::tools::fs`,
//! `git`): only the reading and writing moves to the host.
use crate::claude::remote::{self, sh_path, sh_quote, Target};
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use devcat_harness::approval::Kind;
use devcat_harness::message::ToolSpec;
use devcat_harness::tools::{fs, git, shell, Tool, ToolCtx, ToolOutput};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// The harness tools a chat on an SSH host gets from here instead.
pub const REPLACED: &[&str] = &["read", "write", "edit", "glob", "grep", "shell", "git"];

/// A file bigger than this isn't read whole: `read` shows its start, `edit` refuses.
const MAX_FILE: u64 = 20 * 1024 * 1024;
/// Most bytes a listing (glob, grep's files) takes from the host.
const MAX_LISTING: usize = 32 * 1024 * 1024;
/// How long a file call may take (a first connection may also start the session's VPN).
const FILE_TIMEOUT: Duration = Duration::from_secs(60);
/// glob and grep walk the whole folder.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);
/// `git` on the host, as long as the local one.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long opening a chat waits for the host.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether `cwd` is a folder on an SSH host.
pub fn is_remote(cwd: &str) -> bool {
    cwd.starts_with(remote::SCHEME)
}

/// The host a chat works on, as found when it opened.
#[derive(Debug, Clone)]
pub struct Host {
    /// The chat's folder (`ssh://…`): parsed for every call, so a session's new password or
    /// deletion counts at once.
    pub uri: String,
    /// For the agent and messages: the session's name, or the destination.
    pub label: String,
    /// The project folder on the host, absolute (`pwd -P`), or as written when the host couldn't
    /// be reached (`~` stays `~`: the host expands it).
    pub root: String,
    pub home: Option<String>,
    /// `uname -s` (Linux, Darwin…).
    pub os: String,
    /// What `shell` runs commands in: bash, or sh where there's no bash.
    pub shell: String,
    /// ripgrep is installed: glob and grep use it (gitignore rules, Rust regex syntax, newest first).
    pub rg: bool,
    /// `timeout` is installed: a command that runs too long is stopped on the host too.
    pub timeout: bool,
    /// git's version there, if it has git.
    pub git: Option<(u32, u32)>,
    /// The project's rule files (AGENTS.md, CLAUDE.md…), read there.
    pub rules: Vec<(String, String)>,
    /// Why the host couldn't be used when the chat opened: unreachable, the folder or session gone,
    /// a Windows host (the tools try again on each call).
    pub unreachable: Option<String>,
}

impl Host {
    fn target(&self) -> Result<Target, ToolOutput> {
        let t = Target::parse(&self.uri).ok_or_else(|| ToolOutput::err(format!("Not a folder on an SSH host: {}", self.uri)))?;
        t.check().map_err(|e| ToolOutput::err(e.to_string()))?;
        Ok(t)
    }

    /// `p` from the model as an absolute path on the host: relative to the project folder,
    /// `~/…` in the home folder, `.` and `..` resolved (without asking the host).
    pub fn path(&self, p: &str) -> String {
        let p = p.trim();
        let joined = if p == "~" || p.starts_with("~/") {
            match &self.home {
                Some(h) => format!("{h}{}", &p[1..]),
                None => p.to_string(),
            }
        } else if p.starts_with('/') {
            p.to_string()
        } else if p.is_empty() || p == "." {
            self.root.clone()
        } else {
            format!("{}/{p}", self.root)
        };
        normalize(&joined)
    }

    /// Whether `path` (from [`Host::path`]) is in the project folder.
    pub fn inside(&self, path: &str) -> bool {
        let root = normalize(&self.root);
        root == "/" && path.starts_with('/') || path == root || path.starts_with(&format!("{root}/"))
    }

    /// Run `script` (POSIX sh) on the host with `args` as $1… (each quoted with [`p`] or [`q`]),
    /// `stdin` if any.
    async fn run(&self, script: &str, args: &[String], stdin: Option<&[u8]>, timeout: Duration) -> Result<Out, ToolOutput> {
        let t = self.target()?;
        // the script says it started: an exit code 255 without that is ssh's own. A login script
        // may print before it (a banner in .bashrc): that's dropped.
        let script = format!("printf '%s' {}\n{script}", q(MARK));
        let out = remote::exec(&t, &script, args, stdin, timeout).await.map_err(|e| ToolOutput::err(e.to_string()))?;
        match find(&out.stdout, MARK.as_bytes()) {
            Some(i) => Ok(Out { code: out.code, stdout: out.stdout[i + MARK.len()..].to_vec(), stderr: out.stderr }),
            None if out.code == 255 => Err(ToolOutput::err(remote::connect_error(&t, out.stderr.trim()).to_string())),
            None => Err(ToolOutput::err(format!("{} didn't run the command: {}", self.label, out.stderr.trim()))),
        }
    }

    /// `path` on the host with symlinks resolved (None if it doesn't exist or can't be told).
    pub async fn real_path(&self, path: &str) -> Option<String> {
        let out = self.run(r#"cd "$(dirname -- "$1")" 2>/dev/null && d=$(pwd -P) && b=$(basename -- "$1") && if [ -L "$b" ]; then readlink -f -- "$b" 2>/dev/null || realpath -- "$b"; else printf '%s/%s' "$d" "$b"; fi"#, &[p(path)], None, FILE_TIMEOUT).await.ok()?;
        let r = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.code == 0 && r.starts_with('/')).then(|| normalize(&r))
    }
}

/// A path for a script's arguments: `~` / `~/…` expand on the host.
fn p(path: &str) -> String {
    sh_path(path)
}

/// Any other argument, as it is.
fn q(s: &str) -> String {
    sh_quote(s)
}

/// What a script prints first (see [`Host::run`]).
const MARK: &str = "\u{1}DEVCAT-RUN\u{1}";

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

struct Out {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl Out {
    /// The host's error message, or `what` failed.
    fn error(&self, what: &str) -> ToolOutput {
        let e = self.stderr.trim();
        ToolOutput::err(if e.is_empty() { format!("{what} (exit code {})", self.code) } else { format!("{what}: {e}") })
    }
}

/// `.` and `..` removed from a `/`-separated path; a leading `~` stays (the host expands it).
fn normalize(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "~") {
                    parts.pop();
                }
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else if joined.is_empty() { ".".into() } else { joined }
}

/// What the host is and has, and the project's rule files: one `ssh` when a chat opens. $1 = the
/// folder; $2… = the rule files and folders to read.
const PROBE: &str = r#"cd "$1" 2>/dev/null || { echo "Can't open $1 (no such folder, or no permission)" >&2; exit 3; }
shift
pwd -P | tr -d '\n'; printf '\000'
printf '%s\000' "$HOME"
uname -s 2>/dev/null | tr -d '\n'; printf '\000'
if command -v bash >/dev/null 2>&1; then printf bash; else printf sh; fi; printf '\000'
command -v rg >/dev/null 2>&1 && printf rg; printf '\000'
command -v timeout >/dev/null 2>&1 && printf timeout; printf '\000'
command -v git >/dev/null 2>&1 && git version 2>/dev/null | tr -d '\n'; printf '\000'
for f in "$@"; do
  if [ -f "$f" ]; then printf '%s\000' "$f"; head -c 20000 "$f" | tr -d '\000'; printf '\000'
  elif [ -d "$f" ]; then
    for g in "$f"/*.md "$f"/*.mdc; do
      [ -f "$g" ] && { printf '%s\000' "$g"; head -c 20000 "$g" | tr -d '\000'; printf '\000'; }
    done
  fi
done"#;

/// Ask the host about itself (see [`PROBE`]). When it can't be used (unreachable, the folder or
/// the session gone, a Windows host): what's known from the URI, with why (`unreachable`), so a
/// chat still opens and shows its history. An error only for what isn't an SSH folder at all.
pub async fn probe(uri: &str) -> AppResult<Host> {
    let t = Target::parse(uri).ok_or_else(|| AppError::msg(format!("Not a folder on an SSH host: {uri}")))?;
    let mut host = Host {
        uri: uri.to_string(),
        label: t.label().to_string(),
        root: t.path.clone(),
        home: None,
        os: "Linux or another Unix (not checked yet)".into(),
        shell: "bash (or sh)".into(),
        rg: false,
        timeout: false,
        git: None,
        rules: vec![],
        unreachable: None,
    };
    if let Err(e) = t.check() {
        host.unreachable = Some(e.to_string());
        return Ok(host);
    }
    let mut args = vec![p(&t.path)];
    args.extend(devcat_harness::prompt::PROJECT_RULES.iter().map(|r| q(r)));
    let out = match tokio::time::timeout(PROBE_TIMEOUT, host.run(PROBE, &args, None, PROBE_TIMEOUT)).await {
        Ok(Ok(o)) if o.code == 0 => o,
        Ok(Ok(o)) => {
            let e = o.stderr.trim();
            host.unreachable = Some(if e.is_empty() { format!("Couldn't open the folder on {}.", host.label) } else { e.to_string() });
            return Ok(host);
        }
        Ok(Err(e)) => {
            // it answered, but not in sh: cmd or PowerShell
            host.unreachable = Some(if !e.content.starts_with("Couldn't connect") && remote::host_os(&t).await == remote::HostOs::Windows {
                format!("{} is a Windows host: DevCat Agent works in folders on Linux and macOS hosts. Open a terminal there instead.", t.label())
            } else {
                e.content
            });
            return Ok(host);
        }
        Err(_) => {
            host.unreachable = Some(format!("{} didn't answer in {} s.", host.label, PROBE_TIMEOUT.as_secs()));
            return Ok(host);
        }
    };
    let fields: Vec<String> = out.stdout.split(|b| *b == 0).map(|f| String::from_utf8_lossy(f).into_owned()).collect();
    let get = |i: usize| fields.get(i).cloned().unwrap_or_default();
    if !get(0).is_empty() {
        host.root = get(0);
    }
    host.home = Some(get(1)).filter(|h| h.starts_with('/'));
    if !get(2).is_empty() {
        host.os = get(2);
    }
    host.shell = get(3);
    host.rg = get(4) == "rg";
    host.timeout = get(5) == "timeout";
    host.git = git::git_version(&get(6));
    // then (name, text) pairs, the folders' files sorted by name
    let mut rules: Vec<(String, String)> = fields[7.min(fields.len())..].chunks(2).filter(|c| c.len() == 2 && !c[0].is_empty()).map(|c| (c[0].clone(), devcat_harness::prompt::rule_text(&c[0], &c[1]))).collect();
    let order = |name: &str| devcat_harness::prompt::PROJECT_RULES.iter().position(|r| name == *r || name.starts_with(&format!("{r}/"))).unwrap_or(usize::MAX);
    rules.sort_by(|a, b| order(&a.0).cmp(&order(&b.0)).then_with(|| a.0.cmp(&b.0)));
    host.rules = rules;
    Ok(host)
}

/// The tools for a chat on `host`, in place of [`REPLACED`].
pub fn tools(host: Arc<Host>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Read(host.clone())),
        Arc::new(Write(host.clone())),
        Arc::new(Edit(host.clone())),
        Arc::new(Glob(host.clone())),
        Arc::new(Grep(host.clone())),
        Arc::new(Shell(host.clone())),
        Arc::new(Git(host)),
    ]
}

/// The local tool's spec with its description changed (or as it is).
fn spec_of(local: &dyn Tool, describe: impl FnOnce(String) -> String) -> ToolSpec {
    let mut s = local.spec();
    s.description = describe(s.description);
    s
}

fn path_arg(input: &Value) -> Result<&str, ToolOutput> {
    input.get("path").and_then(Value::as_str).ok_or_else(|| ToolOutput::err("Missing required string parameter `path`."))
}

fn edit_kind(host: &Host, input: &Value) -> Kind {
    match input.get("path").and_then(Value::as_str) {
        Some(p) if host.inside(&host.path(p)) => Kind::Edit,
        _ => Kind::EditOutside,
    }
}

// ---------- read, write, edit ----------

/// $1 = the path, $2 = most bytes to send. A folder: `D` and its entries (folders with `/`); a
/// file: `F<size>` NUL and its start.
const READ: &str = r#"f=$1
if [ -d "$f" ]; then
  printf D
  for x in "$f"/* "$f"/.[!.]* "$f"/..?*; do
    [ -e "$x" ] || [ -L "$x" ] || continue
    if [ -d "$x" ]; then printf '%s/\000' "${x##*/}"; else printf '%s\000' "${x##*/}"; fi
  done
elif [ -f "$f" ]; then
  [ -r "$f" ] || { echo "Permission denied" >&2; exit 3; }
  printf 'F%s\000' "$(wc -c < "$f" | tr -d ' ')"
  head -c "$2" "$f"
elif [ -e "$f" ]; then echo "Not a regular file" >&2; exit 3
else echo "No such file or folder" >&2; exit 3
fi"#;

enum Got {
    Folder(Vec<String>),
    File { size: u64, bytes: Vec<u8> },
}

async fn fetch(host: &Host, path: &str, cap: u64) -> Result<Got, ToolOutput> {
    let out = host.run(READ, &[p(path), q(&cap.to_string())], None, FILE_TIMEOUT).await?;
    if out.code != 0 {
        return Err(out.error(&format!("Can't read {path}")));
    }
    match out.stdout.split_first() {
        Some((b'D', rest)) => {
            let mut names: Vec<String> = rest.split(|b| *b == 0).filter(|n| !n.is_empty()).map(|n| String::from_utf8_lossy(n).into_owned()).collect();
            names.sort();
            Ok(Got::Folder(names))
        }
        Some((b'F', rest)) => {
            let at = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
            let size = String::from_utf8_lossy(&rest[..at]).trim().parse().unwrap_or(0);
            Ok(Got::File { size, bytes: rest.get(at + 1..).unwrap_or_default().to_vec() })
        }
        _ => Err(ToolOutput::err(format!("Can't read {path}: unexpected answer from {}", host.label))),
    }
}

pub struct Read(pub Arc<Host>);

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        spec_of(&fs::Read, |d| d)
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        fs::Read.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        FILE_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let path = match path_arg(&input) {
            Ok(p) => self.0.path(p),
            Err(e) => return e,
        };
        match fetch(&self.0, &path, MAX_FILE).await {
            Err(e) => e,
            Ok(Got::Folder(names)) => ToolOutput::ok(if names.is_empty() { "(empty folder)".to_string() } else { names.join("\n") }),
            Ok(Got::File { size, bytes }) => {
                if fs::looks_binary(&bytes) {
                    return ToolOutput::err(fs::binary_hint(std::path::Path::new(&path), size as usize));
                }
                let mut out = fs::numbered(&String::from_utf8_lossy(&bytes), &input);
                if size > bytes.len() as u64 && !out.is_error {
                    out.content.push_str(&format!("\n(only the first {} MB of this {} MB file were read)", MAX_FILE / 1024 / 1024, size / 1024 / 1024));
                }
                out
            }
        }
    }
}

/// $1 = the path. Creates missing folders; takes all of stdin into a file next to it first (a
/// dropped connection leaves the file as it was), then copies it in place, so the file keeps its
/// mode, owner and links. Prints `C` (created) or `R` (replaced).
const WRITE: &str = r#"f=$1
[ -d "$f" ] && { echo "It's a folder" >&2; exit 3; }
mkdir -p -- "$(dirname -- "$f")" || exit 3
t="$f.devcat-$$"
cat > "$t" || { rm -f -- "$t"; exit 3; }
if [ -e "$f" ]; then printf R; else printf C; fi
cat -- "$t" > "$f"; r=$?
rm -f -- "$t"
exit $r"#;

async fn put(host: &Host, path: &str, content: &str) -> Result<bool, ToolOutput> {
    let out = host.run(WRITE, &[p(path)], Some(content.as_bytes()), FILE_TIMEOUT).await?;
    if out.code != 0 {
        return Err(out.error(&format!("Can't write {path}")));
    }
    Ok(out.stdout.first() == Some(&b'R'))
}

pub struct Write(pub Arc<Host>);

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        spec_of(&fs::Write, |d| d)
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        edit_kind(&self.0, input)
    }
    fn summary(&self, input: &Value) -> String {
        fs::Write.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        FILE_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let (path, content) = match (path_arg(&input), input.get("content").and_then(Value::as_str)) {
            (Ok(p), Some(c)) => (self.0.path(p), c),
            (Err(e), _) => return e,
            (_, None) => return ToolOutput::err("Missing required string parameter `content`."),
        };
        match put(&self.0, &path, content).await {
            Ok(existed) => ToolOutput::ok(format!("{} {path} ({} lines).", if existed { "Replaced" } else { "Created" }, content.lines().count())),
            Err(e) => e,
        }
    }
}

pub struct Edit(pub Arc<Host>);

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        spec_of(&fs::Edit, |d| d)
    }
    fn kind(&self, input: &Value, _: &ToolCtx) -> Kind {
        edit_kind(&self.0, input)
    }
    fn summary(&self, input: &Value) -> String {
        fs::Edit.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        FILE_TIMEOUT * 2 + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let s = |k: &str| input.get(k).and_then(Value::as_str);
        let (path, old, new) = match (path_arg(&input), s("old_string"), s("new_string")) {
            (Ok(p), Some(o), Some(n)) => (self.0.path(p), o, n),
            (Err(e), _, _) => return e,
            _ => return ToolOutput::err("Missing required string parameters `old_string` and `new_string`."),
        };
        let text = match fetch(&self.0, &path, MAX_FILE + 1).await {
            Err(e) => return e,
            Ok(Got::Folder(_)) => return ToolOutput::err(format!("Can't edit {path}: it's a folder.")),
            Ok(Got::File { size, .. }) if size > MAX_FILE => return ToolOutput::err(format!("{path} is too large to edit here ({} MB). Change it with a command in `shell`.", size / 1024 / 1024)),
            Ok(Got::File { bytes, .. }) => match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => return ToolOutput::err(format!("Can't edit {path}: it isn't UTF-8 text.")),
            },
        };
        let (updated, count) = match fs::replaced(&text, old, new, input.get("replace_all").and_then(Value::as_bool).unwrap_or(false), &path) {
            Ok(r) => r,
            Err(e) => return e,
        };
        match put(&self.0, &path, &updated).await {
            Ok(_) => ToolOutput::ok(format!("Edited {path} ({count} replacement{}).", if count == 1 { "" } else { "s" })),
            Err(e) => e,
        }
    }
}

// ---------- glob, grep ----------

/// $1 = the folder, $2 = `rg` to use ripgrep. The files under it, NUL-separated, relative to it:
/// ripgrep's (gitignore rules, newest first), else git's in a repository (tracked and untracked,
/// ignored ones left out), else every file outside hidden folders.
const FILES: &str = r#"cd "$1" 2>/dev/null || { echo "No such folder: $1" >&2; exit 3; }
if [ "$2" = rg ]; then rg --files --null --no-require-git --sortr modified 2>/dev/null
elif command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then git -c core.quotePath=false ls-files -z --cached --others --exclude-standard 2>/dev/null
else find . -mindepth 1 -name '.*' -prune -o -type f -print0 2>/dev/null
fi
exit 0"#;

/// Files under `root` on the host (see [`FILES`]), relative, hidden ones left out.
async fn files(host: &Host, root: &str) -> Result<Vec<String>, ToolOutput> {
    let out = host.run(FILES, &[p(root), q(if host.rg { "rg" } else { "" })], None, SEARCH_TIMEOUT).await?;
    if out.code != 0 {
        return Err(out.error(&format!("Can't search {root}")));
    }
    let bytes = &out.stdout[..out.stdout.len().min(MAX_LISTING)];
    Ok(bytes
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).trim_start_matches("./").to_string())
        // like the local walker: nothing in hidden files or folders (git lists tracked ones)
        .filter(|f| !f.split('/').any(|c| c.starts_with('.')))
        .collect())
}

/// Where a search starts: `path` from the input, or the project folder.
fn search_root(host: &Host, input: &Value) -> String {
    input.get("path").and_then(Value::as_str).map(|p| host.path(p)).unwrap_or_else(|| normalize(&host.root))
}

pub struct Glob(pub Arc<Host>);

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        let rg = self.0.rg;
        spec_of(&fs::Glob, |d| if rg { d } else { d.replace("Newest first, at most", "Sorted by path, at most") })
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        fs::Glob.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        SEARCH_TIMEOUT + Duration::from_secs(5)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(pattern) = input.get("pattern").and_then(Value::as_str) else { return ToolOutput::err("Missing required string parameter `pattern`.") };
        let matches = match fs::name_matcher(pattern) {
            Ok(m) => m,
            Err(e) => return e,
        };
        let mut hits: Vec<String> = match files(&self.0, &search_root(&self.0, &input)).await {
            Ok(f) => f.into_iter().filter(|f| matches(f)).collect(),
            Err(e) => return e,
        };
        if hits.is_empty() {
            return ToolOutput::ok("No files match.");
        }
        if !self.0.rg {
            hits.sort();
        }
        let more = hits.len().saturating_sub(fs::MAX_HITS);
        let mut out = hits.into_iter().take(fs::MAX_HITS).collect::<Vec<_>>().join("\n");
        if more > 0 {
            out.push_str(&format!("\n(and {more} more; narrow the pattern)"));
        }
        ToolOutput::ok(out)
    }
}

/// ripgrep: $1 = folder to cd to, $2 = the regex, $3 = file (or empty: the folder), $4 = `-i` or
/// empty, $5 = a glob or empty. Prints at most one hit over the limit.
const RG: &str = r#"cd "$1" 2>/dev/null || { echo "No such folder: $1" >&2; exit 3; }
set -- "$2" "$3" "$4" "$5"
p=$1; f=$2; i=$3; g=$4
set -- rg --line-number --no-heading --color never --with-filename --null --no-require-git
[ -n "$i" ] && set -- "$@" -i
[ -n "$g" ] && set -- "$@" -g "$g"
set -- "$@" -e "$p" --
# always a path: without one, ripgrep reads stdin when it isn't a terminal (as over ssh)
if [ -n "$f" ]; then set -- "$@" "$f"; else set -- "$@" .; fi
"$@" </dev/null | head -n LIMIT
exit 0"#;

/// grep over a list of files (stdin, NUL-separated): $1 = folder, $2 = the regex, $3 = `-i` or
/// empty. Perl syntax where grep has it (`-P`, closest to the model's Rust regex), else extended.
const GREP: &str = r#"cd "$1" 2>/dev/null || { echo "No such folder: $1" >&2; exit 3; }
if echo x | grep -qP 'x' 2>/dev/null; then s=-P; else s=-E; fi
if [ -n "$3" ]; then xargs -0 grep -nHI --null "$s" -i -e "$2" -- 2>/dev/null; else xargs -0 grep -nHI --null "$s" -e "$2" -- 2>/dev/null; fi | head -n LIMIT
exit 0"#;

pub struct Grep(pub Arc<Host>);

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        let rg = self.0.rg;
        spec_of(&fs::Grep, |d| if rg { d } else { d.replace("(Rust regex syntax)", "(Perl-compatible syntax where the host's grep has it, else extended regex)") })
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        fs::Grep.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        // the file-or-folder check, the file list, then the search
        FILE_TIMEOUT + SEARCH_TIMEOUT * 2 + Duration::from_secs(10)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(pattern) = input.get("pattern").and_then(Value::as_str) else { return ToolOutput::err("Missing required string parameter `pattern`.") };
        // checked here, so a bad pattern says so instead of "no matches"
        if self.0.rg
            && let Err(e) = regex::Regex::new(pattern)
        {
            return ToolOutput::err(format!("Bad regex: {e}"));
        }
        let ci = input.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false);
        let glob = input.get("glob").and_then(Value::as_str).filter(|g| !g.is_empty());
        let filter = match glob.map(fs::file_filter).transpose() {
            Ok(f) => f,
            Err(e) => return e,
        };
        let root = search_root(&self.0, &input);
        let limit = (fs::MAX_HITS + 1).to_string();
        // a file: search it from its folder, so the hit shows its name
        let probe = self.0.run(r#"[ -d "$1" ] && printf d; [ -f "$1" ] && printf f; exit 0"#, &[p(&root)], None, FILE_TIMEOUT).await;
        let is_file = match probe {
            Ok(o) => o.stdout == b"f",
            Err(e) => return e,
        };
        let (dir, file) = if is_file {
            let (d, f) = root.rsplit_once('/').unwrap_or((".", root.as_str()));
            (if d.is_empty() { "/".to_string() } else { d.to_string() }, f.to_string())
        } else {
            (root.clone(), String::new())
        };
        let out = if self.0.rg {
            let script = RG.replace("LIMIT", &limit);
            self.0.run(&script, &[p(&dir), q(pattern), q(&file), q(if ci { "-i" } else { "" }), q(glob.unwrap_or(""))], None, SEARCH_TIMEOUT).await
        } else {
            let list: Vec<String> = if is_file {
                vec![file]
            } else {
                match files(&self.0, &dir).await {
                    Ok(f) => f.into_iter().filter(|f| filter.as_ref().is_none_or(|m| m.is_match(f))).collect(),
                    Err(e) => return e,
                }
            };
            if list.is_empty() {
                return ToolOutput::ok("No matches.");
            }
            let stdin = list.join("\0");
            let script = GREP.replace("LIMIT", &limit);
            self.0.run(&script, &[p(&dir), q(pattern), q(if ci { "-i" } else { "" })], Some(stdin.as_bytes()), SEARCH_TIMEOUT).await
        };
        let out = match out {
            Ok(o) => o,
            Err(e) => return e,
        };
        let hits: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                // `path` NUL `line:text`
                let (path, rest) = l.split_once('\0')?;
                let (n, text) = rest.split_once(':')?;
                Some(fs::hit(path.trim_start_matches("./"), n.parse().ok()?, text))
            })
            .collect();
        if hits.is_empty() {
            let e = out.stderr.trim();
            return if e.is_empty() { ToolOutput::ok("No matches.") } else { ToolOutput::err(e.to_string()) };
        }
        let more = hits.len() > fs::MAX_HITS;
        let mut text = hits.into_iter().take(fs::MAX_HITS).collect::<Vec<_>>().join("\n");
        if more {
            text.push_str(&format!("\n(more than {} matches; showing the first {})", fs::MAX_HITS, fs::MAX_HITS));
        }
        ToolOutput::ok(text)
    }
}

// ---------- shell, git ----------

/// $1 = folder, $2 = seconds (or empty: no `timeout` there), $3 = the shell, $4 = the command.
/// stdout and stderr together; a login shell, so the PATH is the one a terminal there has.
const SHELL: &str = r#"exec 2>&1
cd "$1" || exit 1
if [ -n "$2" ]; then exec timeout -k 5 "$2" "$3" -lc "$4" </dev/null; fi
exec "$3" -lc "$4" </dev/null"#;

pub struct Shell(pub Arc<Host>);

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        let h = &self.0;
        spec_of(&devcat_harness::tools::Shell, |_| {
            format!(
                "Run a {} command on {} (over SSH), in the project folder there, and return its output and exit code. Each call is a new login shell (`cd` doesn't carry over). Not interactive: don't start editors, pagers or servers that never exit. Default timeout 120 s, at most 600 s.",
                h.shell, h.label
            )
        })
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        devcat_harness::tools::Shell.summary(input)
    }
    fn timeout(&self, input: &Value) -> Duration {
        // the pipeline's limit: over the host's own and the local one below, so their message wins
        shell::limit_of(input) + Duration::from_secs(20)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let Some(command) = input.get("command").and_then(Value::as_str) else { return ToolOutput::err("Missing required string parameter `command`.") };
        let limit = shell::limit_of(&input);
        let secs = if self.0.timeout { limit.as_secs().to_string() } else { String::new() };
        let sh = if self.0.shell == "sh" { "sh" } else { "bash" };
        let out = match self.0.run(SHELL, &[p(&normalize(&self.0.root)), q(&secs), q(sh), q(command)], None, limit + Duration::from_secs(10)).await {
            Ok(o) => o,
            Err(e) if e.content.contains("didn't answer in time") => {
                let more = if self.0.timeout { String::new() } else { format!(" {} has no `timeout` command, so it may still be running there.", self.0.label) };
                return ToolOutput::err(format!("Timed out after {} s.{more}", limit.as_secs()));
            }
            Err(e) => return e,
        };
        let text = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        // `timeout` ends with 124 (137 after -k) when it stopped the command
        if self.0.timeout && matches!(out.code, 124 | 137) {
            return ToolOutput::err(format!("{text}\nTimed out after {} s and was stopped, with everything it started.", limit.as_secs()).trim_start().to_string());
        }
        shell::exit_output(text, Some(out.code).filter(|c| *c >= 0))
    }
}

/// $1 = folder, $2 = `1` for a reading call, then git's arguments. stdout and stderr together.
const GIT: &str = r#"exec 2>&1
cd "$1" || exit 1
[ "$2" = 1 ] && export GIT_NO_LAZY_FETCH=1
shift 2
ENV
exec git "$@" </dev/null"#;

pub struct Git(pub Arc<Host>);

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> ToolSpec {
        let label = self.0.label.clone();
        spec_of(&devcat_harness::tools::Git, |d| d.replacen("Run git in the project folder.", &format!("Run git in the project folder on {label} (over SSH)."), 1))
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        devcat_harness::tools::Git.kind(input, ctx)
    }
    fn summary(&self, input: &Value) -> String {
        devcat_harness::tools::Git.summary(input)
    }
    fn timeout(&self, _: &Value) -> Duration {
        GIT_TIMEOUT + FILE_TIMEOUT + Duration::from_secs(10)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let a = match git::args(&input) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let root = normalize(&self.0.root);
        // a call that runs without asking mustn't start a program the repository's config names
        // (see the harness's `git`): its config is read on the host first
        let reading = git::read_only(&a);
        let a = if reading {
            let out = match self.0.run(r#"cd "$1" && git config -z --get-regexp "$2"; exit 0"#, &[p(&root), q(git::RISKY_CONFIG)], None, FILE_TIMEOUT).await {
                Ok(o) => o,
                Err(e) => return e,
            };
            let lazy_off = self.0.git.is_some_and(|v| v >= (2, 45));
            match git::harmless_with(&a, &git::config_entries(&out.stdout), lazy_off) {
                Ok(a) => a,
                Err(e) => return e,
            }
        } else {
            a
        };
        let env: String = git::ENV.iter().map(|(k, v)| format!("export {k}={}\n", sh_quote(v))).collect();
        let script = GIT.replace("ENV", &env);
        // `~` in git's own arguments is git's: quoted as it is
        let mut args = vec![p(&root), q(if reading { "1" } else { "0" })];
        args.extend(git::BASE.iter().map(|s| q(s)));
        args.extend(a.iter().map(|s| q(s)));
        let out = match self.0.run(&script, &args, None, GIT_TIMEOUT).await {
            Ok(o) => o,
            Err(e) => return e,
        };
        shell::exit_output(String::from_utf8_lossy(&out.stdout).trim_end().to_string(), Some(out.code).filter(|c| *c >= 0))
    }
}

// ---------- attachments ----------

/// Files the user attached, copied to the host (its paste folder, `remote::stage_bytes`), so the
/// agent there can open them: `(local path, path on the host or why it couldn't be copied)`.
pub async fn stage(uri: &str, paths: &[String]) -> Vec<(String, Result<String, String>)> {
    let mut out = Vec::new();
    let t = Target::parse(uri);
    for p in paths {
        let r = async {
            let t = t.as_ref().ok_or("not an SSH folder")?;
            let size = tokio::fs::metadata(p).await.map_err(|e| e.to_string())?.len();
            if size > MAX_FILE {
                return Err(format!("larger than {} MB", MAX_FILE / 1024 / 1024));
            }
            let bytes = tokio::fs::read(p).await.map_err(|e| e.to_string())?;
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            remote::stage_bytes(t, &name, &bytes).await.map_err(|e| e.to_string())
        }
        .await;
        out.push((p.clone(), r));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(root: &str, home: Option<&str>) -> Host {
        Host {
            uri: format!("ssh://{}{root}", remote::LOCAL_TEST_HOST),
            label: "test".into(),
            root: root.into(),
            home: home.map(String::from),
            os: "Linux".into(),
            shell: "sh".into(),
            rg: false,
            timeout: false,
            git: None,
            rules: vec![],
            unreachable: None,
        }
    }

    #[test]
    fn paths_on_the_host() {
        let h = host("/srv/app", Some("/home/me"));
        assert_eq!(h.path("src/main.rs"), "/srv/app/src/main.rs");
        assert_eq!(h.path("./a/../b.txt"), "/srv/app/b.txt");
        assert_eq!(h.path("/etc/hosts"), "/etc/hosts");
        assert_eq!(h.path("~/.bashrc"), "/home/me/.bashrc");
        assert_eq!(h.path(""), "/srv/app");
        assert!(h.inside(&h.path("src/x")) && h.inside(&h.path(".")));
        assert!(!h.inside(&h.path("../other/x")) && !h.inside("/srv/application/x") && !h.inside(&h.path("~/x")));
        // the home folder not known yet: `~` stays for the host to expand
        let h = host("~", None);
        assert_eq!(h.path("a/b"), "~/a/b");
        assert_eq!(h.path("~/../x"), "~/x");
        assert!(h.inside(&h.path("a")) && !h.inside("/etc/x"));
        assert!(host("/", None).inside("/anything"));
        assert_eq!(edit_kind(&host("/srv/app", None), &json!({ "path": "../x" })), Kind::EditOutside);
        assert_eq!(edit_kind(&host("/srv/app", None), &json!({ "path": "x" })), Kind::Edit);
    }

    fn project() -> (std::path::PathBuf, Arc<Host>) {
        let dir = std::env::temp_dir().join(format!("devcat-agent-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\r\n    println!(\"hi\");\r\n}\r\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();
        std::fs::write(dir.join(".hidden/x.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let root = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        (dir, Arc::new(host(&root, None)))
    }

    /// The scripts as the host runs them (the test host runs them with the local `sh`).
    #[cfg(unix)]
    #[tokio::test]
    async fn read_write_edit_on_the_host() {
        let (dir, h) = project();
        let ctx = ToolCtx::at(&h.uri);
        let r = Read(h.clone()).run(json!({ "path": "src/main.rs" }), &ctx).await;
        assert_eq!(r.content, "     1\tfn main() {\n     2\t    println!(\"hi\");\n     3\t}\n");
        let ls = Read(h.clone()).run(json!({ "path": "." }), &ctx).await;
        assert_eq!(ls.content, ".hidden/\nAGENTS.md\nsrc/");
        let missing = Read(h.clone()).run(json!({ "path": "nope.txt" }), &ctx).await;
        assert!(missing.is_error && missing.content.contains("No such file"), "{}", missing.content);

        let e = Edit(h.clone()).run(json!({ "path": "src/main.rs", "old_string": "fn main() {\n    println!(\"hi\");", "new_string": "fn main() {\n    println!(\"hello\");" }), &ctx).await;
        assert!(!e.is_error, "{}", e.content);
        assert_eq!(std::fs::read_to_string(dir.join("src/main.rs")).unwrap(), "fn main() {\r\n    println!(\"hello\");\r\n}\r\n");
        let dup = Edit(h.clone()).run(json!({ "path": "src/lib.rs", "old_string": "pub fn", "new_string": "fn" }), &ctx).await;
        assert!(dup.is_error && dup.content.contains("appears 2 times"));

        // quotes, `$`, backticks and newlines arrive as they are
        let tricky = "echo '$HOME' \"`x`\" $(y)\n\\n end\n";
        let w = Write(h.clone()).run(json!({ "path": "docs/new dir/it's.md", "content": tricky }), &ctx).await;
        assert!(w.content.starts_with("Created"), "{}", w.content);
        assert_eq!(std::fs::read_to_string(dir.join("docs/new dir/it's.md")).unwrap(), tricky);
        let w = Write(h.clone()).run(json!({ "path": "docs/new dir/it's.md", "content": "x" }), &ctx).await;
        assert!(w.content.starts_with("Replaced"), "{}", w.content);

        // a write keeps the file's mode, and writes through a link, leaving nothing behind
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("run.sh"), "old").unwrap();
        std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink("run.sh", dir.join("link.sh")).unwrap();
        assert!(!Write(h.clone()).run(json!({ "path": "link.sh", "content": "new" }), &ctx).await.is_error);
        assert_eq!(std::fs::read_to_string(dir.join("run.sh")).unwrap(), "new");
        assert_eq!(std::fs::metadata(dir.join("run.sh")).unwrap().permissions().mode() & 0o777, 0o750);
        assert!(std::fs::symlink_metadata(dir.join("link.sh")).unwrap().file_type().is_symlink());
        assert!(!std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".devcat-")));

        // what a link points to, on the host (`ssh_read` checks it for secrets)
        std::os::unix::fs::symlink(".hidden/x.rs", dir.join("notes.txt")).unwrap();
        assert_eq!(h.real_path(&h.path("notes.txt")).await.as_deref(), Some(format!("{}/.hidden/x.rs", h.root).as_str()));
        assert_eq!(h.real_path(&h.path("src/lib.rs")).await.as_deref(), Some(format!("{}/src/lib.rs", h.root).as_str()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn glob_grep_shell_and_git_on_the_host() {
        let (dir, h) = project();
        let ctx = ToolCtx::at(&h.uri);
        let g = Glob(h.clone()).run(json!({ "pattern": "*.rs" }), &ctx).await;
        assert_eq!(g.content, "src/lib.rs\nsrc/main.rs");

        let r = Grep(h.clone()).run(json!({ "pattern": "fn (a|main)", "glob": "*.rs" }), &ctx).await;
        let mut lines: Vec<&str> = r.content.lines().collect();
        lines.sort();
        assert_eq!(lines, vec!["src/lib.rs:1: pub fn a() {}", "src/main.rs:1: fn main() {"]);
        let one = Grep(h.clone()).run(json!({ "pattern": "FN B", "path": "src/lib.rs", "case_insensitive": true }), &ctx).await;
        assert_eq!(one.content, "lib.rs:2: pub fn b() {}");
        assert_eq!(Grep(h.clone()).run(json!({ "pattern": "zzz" }), &ctx).await.content, "No matches.");

        let ok = Shell(h.clone()).run(json!({ "command": "pwd; echo err >&2" }), &ctx).await;
        assert_eq!((ok.content.as_str(), ok.is_error), (format!("{}\nerr", h.root).as_str(), false));
        let fail = Shell(h.clone()).run(json!({ "command": "echo boom; exit 255" }), &ctx).await;
        assert!(fail.is_error && fail.content.contains("boom") && fail.content.contains("exit code 255"), "{}", fail.content);

        let plain = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&dir).output().unwrap();
        plain(&["init", "-q"]);
        let st = Git(h.clone()).run(json!({ "args": ["status", "--short"] }), &ctx).await;
        assert!(!st.is_error && st.content.contains("?? src/"), "{}", st.content);
        // a reading call: a filter the repository's config names is turned off
        plain(&["config", "filter.evil.clean", "touch ran; cat"]);
        std::fs::write(dir.join(".gitattributes"), "*.rs filter=evil\n").unwrap();
        plain(&["add", "src"]);
        let d = Git(h.clone()).run(json!({ "args": ["diff", "--stat"] }), &ctx).await;
        assert!(!d.is_error, "{}", d.content);
        let _ = std::fs::remove_file(dir.join("ran"));
        let _ = Git(h.clone()).run(json!({ "args": ["status"] }), &ctx).await;
        assert!(!dir.join("ran").exists(), "a reading git call ran the repository's filter");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_probe_finds_the_folder_and_its_rules() {
        let (dir, h) = project();
        std::fs::create_dir_all(dir.join(".cursor/rules")).unwrap();
        std::fs::write(dir.join(".cursor/rules/b.mdc"), "---\nglobs: x\n---\nNo default exports.").unwrap();
        let p = probe(&h.uri).await.unwrap();
        assert_eq!(p.root, h.root);
        assert!(p.unreachable.is_none() && p.home.is_some() && !p.os.is_empty());
        assert_eq!(p.rules, vec![("AGENTS.md".to_string(), "Use tabs.\n".to_string()), (".cursor/rules/b.mdc".to_string(), "No default exports.".to_string())]);
        // the chat still opens: it says why
        let gone = probe(&format!("ssh://{}/no/such/folder", remote::LOCAL_TEST_HOST)).await.unwrap();
        assert!(gone.unreachable.as_deref().is_some_and(|w| w.contains("Can't open")), "{gone:?}");
        assert!(probe("/not/ssh").await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every tool against a real SSH server, through the system `ssh` (a key in ssh-agent, the
    /// host key known): `DEVCAT_TEST_SSH=ssh://user@host:port/an/empty/folder cargo test --lib
    /// against_a_real_host -- --ignored`.
    #[tokio::test]
    #[ignore = "needs an SSH server: DEVCAT_TEST_SSH=ssh://user@host:port/folder"]
    async fn against_a_real_host() {
        let uri = std::env::var("DEVCAT_TEST_SSH").expect("DEVCAT_TEST_SSH");
        let h = Arc::new(probe(&uri).await.unwrap());
        assert!(h.unreachable.is_none(), "{:?}", h.unreachable);
        assert!(h.root.starts_with('/') && h.home.is_some(), "{h:?}");
        eprintln!("host: {} {} rg={} timeout={} git={:?}", h.os, h.shell, h.rg, h.timeout, h.git);
        let ctx = ToolCtx::at(&h.uri);
        let ok = |o: ToolOutput| {
            assert!(!o.is_error, "{}", o.content);
            o.content
        };
        let tricky = "echo '$HOME' \"`x`\" $(y)\n\\n end\n";
        assert!(ok(Write(h.clone()).run(json!({ "path": "src/it's here.rs", "content": tricky }), &ctx).await).starts_with("Created"));
        ok(Write(h.clone()).run(json!({ "path": "src/lib.rs", "content": "pub fn a() {}\npub fn b() {}\n" }), &ctx).await);
        ok(Write(h.clone()).run(json!({ "path": ".hidden/x.rs", "content": "fn hidden() {}\n" }), &ctx).await);
        assert_eq!(ok(Read(h.clone()).run(json!({ "path": "src/it's here.rs", "offset": 2 }), &ctx).await), "     2\t\\n end\n");
        ok(Edit(h.clone()).run(json!({ "path": "src/lib.rs", "old_string": "pub fn b", "new_string": "pub fn bee" }), &ctx).await);
        assert_eq!(ok(Glob(h.clone()).run(json!({ "pattern": "**/*.rs" }), &ctx).await).lines().count(), 2);
        let g = ok(Grep(h.clone()).run(json!({ "pattern": "fn \\w+ee", "glob": "*.rs" }), &ctx).await);
        assert_eq!(g, "src/lib.rs:2: pub fn bee() {}");
        // (a login shell: what the account's profile prints comes first, as in a terminal)
        assert_eq!(ok(Shell(h.clone()).run(json!({ "command": "cd src && ls | wc -l" }), &ctx).await).lines().last().unwrap().trim(), "2");
        let slow = Shell(h.clone()).run(json!({ "command": "sleep 30", "timeout_secs": 2 }), &ctx).await;
        assert!(slow.is_error && slow.content.contains("Timed out"), "{}", slow.content);
        let code = Shell(h.clone()).run(json!({ "command": "exit 255" }), &ctx).await;
        assert!(code.is_error && code.content.contains("exit code 255"), "{}", code.content);
        if h.git.is_some() {
            ok(Shell(h.clone()).run(json!({ "command": "git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm first" }), &ctx).await);
            assert!(ok(Git(h.clone()).run(json!({ "args": ["log", "--oneline"] }), &ctx).await).contains("first"));
            assert!(ok(Git(h.clone()).run(json!({ "args": ["status", "--short"] }), &ctx).await).contains("(no output)"));
        }
    }
}

