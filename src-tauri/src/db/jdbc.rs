//! Databases reached through a JDBC driver jar, without Java in DevCat.
//!
//! DevCat stays Rust: it embeds a small Java helper (`jdbc-bridge/DevcatJdbc.java`, built into
//! `jdbc-bridge/devcat-jdbc.jar`, ~20 KB, no dependencies) and starts it with a Java that's already
//! installed, then speaks one JSON object per line with it (requests on its stdin, answers on its
//! stdout). One helper process holds one JDBC connection; the read-only twin
//! (`St::read_only_driver`) is a second process.
//!
//! Where the helper runs:
//! - **this computer**: the connection's Java path, else `JAVA_HOME`, macOS `java_home`, a Java
//!   that ships with DBeaver / DataGrip / IntelliJ, else `java` on the PATH;
//! - **another computer over SSH** (the connection's SSH settings on): DevCat logs in with its own
//!   SSH client, copies the helper to `~/.devcat/jdbc/` there if it isn't there yet, and runs it
//!   with that computer's Java. The database is then reached from that computer, as with an SSH
//!   tunnel. This is how a computer without Java uses JDBC.
//!
//! The driver jars are the user's (never shipped), named by path on the computer the helper runs on.
//!
//! A read-only session can't rely on the driver: `Connection.setReadOnly` is only a hint. So the
//! helper also turns auto-commit off and rolls back after every statement, and DevCat checks each
//! statement's words first, as for every other database.

use super::sql_common as sc;
use super::{
    ColumnDef, ColumnInfo, Dialect, Driver, ExecOptions, ForeignKeyDef, IndexDef, NodeKind, QueryResult, RowChange, SchemaSnapshot,
    TableDataPage, TableDataRequest, TableRef, TableStructure, TreeNode,
};
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// The helper, built by `jdbc-bridge/build.sh` (Java 11 bytecode).
const HELPER_JAR: &[u8] = include_bytes!("../../jdbc-bridge/devcat-jdbc.jar");
/// The protocol the helper speaks (its `ready` line says which).
const PROTOCOL: u64 = 2;
const D: Dialect = Dialect::Generic;

/// Name of the helper on disk: changes with its content, so an old copy is never run.
fn helper_name() -> String {
    let h = sha2::Sha256::digest(HELPER_JAR);
    format!("devcat-jdbc-{}.jar", &hex::encode(h)[..12])
}

static DATA_DIR: once_cell::sync::OnceCell<PathBuf> = once_cell::sync::OnceCell::new();

/// Where the helper is written for local runs (the app and `devcat mcp`, from their data folder).
pub fn init(data_dir: PathBuf) {
    let _ = DATA_DIR.set(data_dir);
}

fn data_dir() -> AppResult<PathBuf> {
    DATA_DIR
        .get()
        .cloned()
        .or_else(crate::cli::default_data_dir)
        .ok_or_else(|| AppError::msg("JDBC: no data folder to keep the helper in"))
}

/// The helper as a file in DevCat's data folder (not a shared temp folder, where someone else
/// could put their own jar first), rewritten when its content isn't ours.
fn local_helper() -> AppResult<PathBuf> {
    let dir = data_dir()?.join("jdbc");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(helper_name());
    if std::fs::read(&path).ok().as_deref() != Some(HELPER_JAR) {
        let tmp = dir.join(format!(".{}.{}", helper_name(), uuid::Uuid::new_v4()));
        std::fs::write(&tmp, HELPER_JAR)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

// -------------------------------------------------------------------------------------------------
// Connection settings

/// What a JDBC connection keeps, from [`ConnectionConfig`]: `url` the JDBC URL, `user` /
/// `password`, `database` the default catalog, and `options`: `driverJars` (paths, one per line
/// or `;`-separated; a folder means every jar in it), `driverClass` (else found from the jars),
/// `javaPath` (a `java` program or a Java home), `schema` (default schema), `properties`
/// (`key=value` lines for the driver).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub url: String,
    pub jars: Vec<String>,
    pub driver_class: Option<String>,
    pub java: Option<String>,
    pub schema: Option<String>,
    pub props: Map<String, Value>,
    /// a Java charset (MS932, EUC-JP…) the helper makes the JVM's default before loading the driver
    pub encoding: Option<String>,
}

fn opt(cfg: &ConnectionConfig, k: &str) -> Option<String> {
    cfg.options.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Settings {
    pub fn of(cfg: &ConnectionConfig) -> AppResult<Self> {
        let url = cfg.url.as_deref().map(str::trim).unwrap_or("").to_string();
        if !url.to_ascii_lowercase().starts_with("jdbc:") {
            return Err(AppError::msg("JDBC: the URL must start with jdbc: (for example jdbc:sqlbase://host:2155/DB)"));
        }
        let jars: Vec<String> = cfg
            .options
            .get("driverJars")
            .map(|s| s.split(['\n', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default();
        if jars.is_empty() {
            return Err(AppError::msg("JDBC: add the driver's jar file (the path on the computer that runs it)"));
        }
        let mut props = Map::new();
        for line in cfg.options.get("properties").map(String::as_str).unwrap_or("").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) if !k.trim().is_empty() => {
                    props.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
                }
                _ => return Err(AppError::msg(format!("JDBC: driver property {line:?} isn't key=value"))),
            }
        }
        let encoding = opt(cfg, "encoding");
        if let Some(e) = &encoding {
            // Java charset names: letters, digits and . _ : - +
            if e.len() > 40 || !e.chars().all(|c| c.is_ascii_alphanumeric() || ".:_-+".contains(c)) {
                return Err(AppError::msg(format!("JDBC: {e:?} isn't a text encoding name (for example MS932 or EUC-JP)")));
            }
        }
        Ok(Settings { url, jars, driver_class: opt(cfg, "driverClass"), java: opt(cfg, "javaPath"), schema: opt(cfg, "schema"), props, encoding })
    }
}

// -------------------------------------------------------------------------------------------------
// Driver jars on this computer: pinned when the user saves the connection

/// The jar files `jars` names on this computer (a folder: every `*.jar` in it), sorted.
fn local_jar_files(jars: &[String]) -> AppResult<Vec<PathBuf>> {
    let mut out = Vec::new();
    for j in jars {
        let p = super::ssh::expand_home(j);
        if p.is_dir() {
            for e in std::fs::read_dir(&p)?.flatten() {
                let f = e.path();
                if f.is_file() && f.extension().is_some_and(|x| x.eq_ignore_ascii_case("jar")) {
                    out.push(f);
                }
            }
        } else if p.is_file() {
            out.push(p);
        } else {
            return Err(AppError::msg(format!("JDBC: driver jar not found: {}", p.display())));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn hashes_of(files: &[PathBuf]) -> AppResult<Map<String, Value>> {
    let mut m = Map::new();
    for f in files {
        let bytes = std::fs::read(f).map_err(|e| AppError::msg(format!("JDBC: can't read {}: {e}", f.display())))?;
        m.insert(f.display().to_string(), json!(hex::encode(sha2::Sha256::digest(&bytes))));
    }
    Ok(m)
}

/// When the user saves a JDBC connection that runs on this computer: remember the sha-256 of
/// every driver jar (`options.jarHashes`). A jar is code DevCat runs, so one that changes later
/// (an agent writing into that folder, say) isn't run until the user saves the connection again.
pub fn pin_jars(cfg: &mut ConnectionConfig) -> AppResult<()> {
    cfg.options.remove("jarHashes");
    if cfg.kind != ConnectionKind::Jdbc {
        return Ok(());
    }
    // the database list and the URL's {db} go together
    databases_of(cfg)?;
    if cfg.ssh.as_ref().is_some_and(|s| s.enabled) {
        return Ok(());
    }
    let settings = Settings::of(cfg)?;
    let pinned = hashes_of(&local_jar_files(&settings.jars)?)?;
    cfg.options.insert("jarHashes".into(), Value::Object(pinned).to_string());
    Ok(())
}

/// The jar files to load: exactly the pinned ones, unchanged. A connection not saved yet (the
/// dialog's Test) has no pins and uses what it names.
fn pinned_jars(cfg: &ConnectionConfig, settings: &Settings) -> AppResult<Vec<String>> {
    let files = local_jar_files(&settings.jars)?;
    if let Some(saved) = cfg.options.get("jarHashes").filter(|h| !h.trim().is_empty()) {
        let saved: Map<String, Value> = serde_json::from_str(saved).map_err(|_| AppError::msg("JDBC: the saved jar fingerprints are damaged; save the connection again"))?;
        let now = hashes_of(&files)?;
        if now != saved {
            let changed: Vec<String> = now
                .iter()
                .filter(|(k, v)| saved.get(*k) != Some(*v))
                .map(|(k, _)| k.clone())
                .chain(saved.keys().filter(|k| !now.contains_key(*k)).cloned())
                .collect();
            return Err(AppError::msg(format!(
                "JDBC: the driver jars changed since this connection was saved ({}). DevCat runs them as code, so it won't until you open the connection and save it again.",
                changed.join(", ")
            )));
        }
    }
    Ok(files.into_iter().map(|f| f.display().to_string()).collect())
}

/// Whether `sql` (one statement, or a whole script) only reads, judged strictly enough for any
/// database behind JDBC. DevCat can't know each one's quoting and comment rules, and many drivers
/// run several statements from one call, so anything it can't read for sure is refused: `$`
/// and `\` (dollar quoting, backslash escapes), `` ` `` and `[`, a `;` between statements, and words that write or end a
/// transaction anywhere outside '…' / "…" (comments are not taken out: refusing more is fine).
pub fn read_only_sql(sql: &str) -> bool {
    let sql = sql.trim().trim_end_matches(|c: char| c == ';' || c.is_whitespace());
    // `…` and […] are names in MySQL / SQL Server, and a ' inside one would throw off the scan
    if sql.is_empty() || sql.contains(['$', '\\', '`', '[']) {
        return false;
    }
    let mut words_src = String::with_capacity(sql.len());
    let mut quote = None;
    for c in sql.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None => words_src.push(c.to_ascii_uppercase()),
        }
    }
    if quote.is_some() || words_src.contains(';') {
        return false;
    }
    let words: Vec<&str> = words_src.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    const FIRST: &[&str] = &["SELECT", "WITH", "VALUES", "TABLE", "SHOW", "EXPLAIN", "DESCRIBE", "DESC"];
    const NEVER: &[&str] = &[
        "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "REPLACE", "CREATE", "DROP", "ALTER", "TRUNCATE", "RENAME", "GRANT",
        "REVOKE", "INTO", "COMMIT", "ROLLBACK", "SAVEPOINT", "BEGIN", "END", "START", "CALL", "EXEC", "EXECUTE", "DO", "SET",
        "RESET", "LOCK", "UNLOCK", "COPY", "LOAD", "UNLOAD", "IMPORT", "EXPORT", "ATTACH", "DETACH", "DECLARE", "GO", "USE",
        "KILL", "SHUTDOWN", "ANALYZE", "VACUUM", "REFRESH", "COMMENT", "PREPARE", "DEALLOCATE", "OUTFILE", "DUMPFILE", "GET",
        "PUT", "UPDATING", "SHUTDOWN", "RUNSCRIPT", "SCRIPT", "BACKUP", "CHECKPOINT",
    ];
    words.first().is_some_and(|w| FIRST.contains(w)) && !words.iter().any(|w| NEVER.contains(w))
}

// -------------------------------------------------------------------------------------------------
// Starting the helper

/// The helper's stdin and stdout, and what keeps the process (or the SSH session) alive.
struct Launched {
    stdin: Box<dyn AsyncWrite + Send + Unpin>,
    stdout: Box<dyn AsyncRead + Send + Unpin>,
    stderr: Arc<parking_lot::Mutex<String>>,
    keep: Keep,
    /// Where it runs, for messages: "this computer" or the SSH host.
    place: String,
}

enum Keep {
    Local(#[allow(dead_code)] tokio::process::Child),
    Remote(#[allow(dead_code)] Arc<super::ssh::SshSession>, #[allow(dead_code)] tokio::task::JoinHandle<()>),
}

/// Keep the last few KB of the helper's stderr, for errors.
fn push_tail(buf: &parking_lot::Mutex<String>, bytes: &[u8]) {
    let mut b = buf.lock();
    b.push_str(&String::from_utf8_lossy(bytes));
    if b.len() > 8192 {
        let cut = b.len() - 8192;
        let cut = (cut..b.len()).find(|i| b.is_char_boundary(*i)).unwrap_or(b.len());
        b.drain(..cut);
    }
}

/// `java` in each JDK under `<base>/<vendor>/<version>` (as Windows installers lay them out),
/// newest version first.
#[cfg_attr(not(windows), allow(dead_code))]
fn vendor_javas(base: &Path, exe: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for vendor in ["Eclipse Adoptium", "Java", "Microsoft", "Zulu", "Amazon Corretto", "BellSoft"] {
        let mut homes: Vec<PathBuf> = std::fs::read_dir(base.join(vendor)).into_iter().flatten().flatten().map(|e| e.path()).collect();
        homes.sort();
        out.extend(homes.iter().rev().map(|h| h.join("bin").join(exe)));
    }
    out
}

/// Java programs to try on this computer, best first.
fn java_candidates(java: Option<&str>) -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    let from_home = |home: &Path| home.join("bin").join(exe);
    if let Some(j) = java {
        let p = super::ssh::expand_home(j);
        return vec![if p.is_dir() { from_home(&p) } else { p }];
    }
    let mut out = Vec::new();
    if let Some(h) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
        out.push(from_home(Path::new(&h)));
    }
    #[cfg(target_os = "macos")]
    if let Ok(o) = std::process::Command::new("/usr/libexec/java_home").stderr(std::process::Stdio::null()).output()
        && o.status.success()
    {
        let home = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !home.is_empty() {
            out.push(from_home(Path::new(&home)));
        }
    }
    // A Mac app started from the Dock doesn't get the shell's PATH or JAVA_HOME: where package
    // managers put a JDK that `java_home` doesn't list.
    #[cfg(target_os = "macos")]
    for p in ["/opt/homebrew/opt/openjdk/bin/java", "/usr/local/opt/openjdk/bin/java", "/opt/homebrew/opt/openjdk@21/bin/java", "/opt/homebrew/opt/openjdk@17/bin/java"] {
        out.push(PathBuf::from(p));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".sdkman/candidates/java/current/bin").join(exe));
    }
    // JDK installers on Windows: the newest version folder of each vendor
    #[cfg(windows)]
    if let Ok(pf) = std::env::var("ProgramFiles") {
        out.extend(vendor_javas(Path::new(&pf), exe));
    }
    // Java that comes with database tools people already have.
    #[cfg(target_os = "macos")]
    for app in ["DBeaver", "DBeaverEE", "DBeaverLite"] {
        out.push(PathBuf::from(format!("/Applications/{app}.app/Contents/Eclipse/jre/Contents/Home/bin/java")));
    }
    #[cfg(target_os = "macos")]
    for app in ["DataGrip", "IntelliJ IDEA", "IntelliJ IDEA CE", "IntelliJ IDEA Ultimate"] {
        out.push(PathBuf::from(format!("/Applications/{app}.app/Contents/jbr/Contents/Home/bin/java")));
    }
    #[cfg(windows)]
    for base in [std::env::var("ProgramFiles").ok(), std::env::var("LOCALAPPDATA").ok().map(|l| format!("{l}\\Programs"))].into_iter().flatten() {
        out.push(PathBuf::from(format!("{base}\\DBeaver\\jre\\bin\\java.exe")));
        out.push(PathBuf::from(format!("{base}\\DBeaverEE\\jre\\bin\\java.exe")));
    }
    #[cfg(target_os = "linux")]
    for p in ["/usr/share/dbeaver-ce/jre/bin/java", "/opt/dbeaver/jre/bin/java", "/snap/dbeaver-ce/current/usr/share/dbeaver-ce/jre/bin/java"] {
        out.push(PathBuf::from(p));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(exe);
            // macOS's /usr/bin/java is a stub that only says no Java is installed
            if cfg!(target_os = "macos") && p == Path::new("/usr/bin/java") {
                continue;
            }
            out.push(p);
        }
    }
    out
}

fn find_java(java: Option<&str>) -> AppResult<PathBuf> {
    let candidates = java_candidates(java);
    if let Some(p) = candidates.iter().find(|p| p.is_file()) {
        return Ok(p.clone());
    }
    Err(AppError::msg(match java {
        Some(j) => format!("JDBC: no Java at {j}"),
        None => "JDBC needs Java 11 or newer on this computer, and none was found. Install one (for example Eclipse Temurin), set its folder as the connection's Java path, or turn on SSH to run the driver on a computer that has Java.".into(),
    }))
}

async fn launch_local(s: &Settings) -> AppResult<Launched> {
    let java = find_java(s.java.as_deref())?;
    let jar = local_helper()?;
    let mut cmd = tokio::process::Command::new(&java);
    cmd.arg("-Xss4m").arg("-jar").arg(&jar);
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("JDBC: couldn't start {}: {e}", java.display())))?;
    let stdin = child.stdin.take().ok_or_else(|| AppError::msg("JDBC: no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| AppError::msg("JDBC: no stdout"))?;
    let mut err = child.stderr.take().ok_or_else(|| AppError::msg("JDBC: no stderr"))?;
    let stderr = Arc::new(parking_lot::Mutex::new(String::new()));
    let tail = stderr.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        while let Ok(n) = err.read(&mut buf).await {
            if n == 0 {
                break;
            }
            push_tail(&tail, &buf[..n]);
        }
    });
    Ok(Launched { stdin: Box::new(stdin), stdout: Box::new(stdout), stderr, keep: Keep::Local(child), place: "this computer".into() })
}

/// The shell an SSH server runs commands with: OpenSSH on Windows uses cmd.exe unless set to
/// PowerShell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Posix,
    Cmd,
    /// PowerShell as the SSH server's shell
    Pwsh,
}

/// One command over SSH, with `input` on its stdin: (exit status, stdout, stderr).
async fn ssh_run(session: &super::ssh::SshSession, command: &str, input: Option<&[u8]>) -> AppResult<(Option<u32>, String, String)> {
    use russh::ChannelMsg;
    let mut ch = session.channel_open_session().await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    ch.exec(true, command).await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    if let Some(data) = input {
        ch.data(data).await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    }
    ch.eof().await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    let (mut out, mut err, mut status) = (Vec::new(), Vec::new(), None);
    let read = async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), read).await.map_err(|_| AppError::msg(format!("SSH: {command:?} didn't finish")))?;
    Ok((status, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned()))
}

async fn detect_shell(session: &super::ssh::SshSession) -> AppResult<Shell> {
    let (_, out, _) = ssh_run(session, "echo %OS%", None).await?;
    if out.contains("Windows_NT") {
        return Ok(Shell::Cmd);
    }
    let (status, out, _) = ssh_run(session, "uname -s", None).await?;
    if status == Some(0) && !out.trim().is_empty() {
        return Ok(Shell::Posix);
    }
    Ok(Shell::Pwsh)
}

/// `'…'` for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `-EncodedCommand` text for PowerShell: the same from cmd.exe and from PowerShell itself,
/// with no quoting rules in the way.
fn ps_encoded(script: &str) -> String {
    use base64::Engine;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!("powershell -NoProfile -NonInteractive -EncodedCommand {}", base64::engine::general_purpose::STANDARD.encode(utf16))
}

/// Commands that put the helper on the SSH host (reading it from stdin; nothing if it's
/// there already) and run it.
pub fn remote_commands(shell: Shell, name: &str, java: Option<&str>) -> (String, String) {
    let size = HELPER_JAR.len();
    match shell {
        Shell::Posix => {
            // in sh whatever the login shell is (fish, tcsh); a part file of its own, kept only when
            // all of it arrived (a dropped connection must not leave half a jar behind)
            let install = format!(
                "exec sh -c {}",
                sh_quote(&format!(
                    "d=\"$HOME/.devcat/jdbc\"; f=\"$d/{name}\"; p=\"$f.$$.part\"; \
                     if [ \"$(wc -c < \"$f\" 2>/dev/null | tr -d ' ')\" = \"{size}\" ]; then cat > /dev/null; echo have; \
                     else mkdir -p \"$d\" && cat > \"$p\" && [ \"$(wc -c < \"$p\" | tr -d ' ')\" = \"{size}\" ] && mv -f \"$p\" \"$f\" && echo wrote; rm -f \"$p\"; fi"
                ))
            );
            // `~/…` as the host's home (quoted, it wouldn't expand)
            let java = java
                .map(|j| match j.strip_prefix("~/") {
                    Some(rest) => format!("\"$HOME\"/{}", sh_quote(rest)),
                    None => sh_quote(j),
                })
                .unwrap_or_else(|| "java".into());
            // a login shell, for a PATH set in the profile
            let run = format!("exec sh -lc {}", sh_quote(&format!("exec {java} -Xss4m -jar \"$HOME/.devcat/jdbc/{name}\"")));
            (install, run)
        }
        Shell::Cmd | Shell::Pwsh => {
            let install = ps_encoded(&format!(
                "$d = Join-Path $env:USERPROFILE '.devcat\\jdbc'; $f = Join-Path $d '{name}'; \
                 if ((Test-Path $f) -and (Get-Item $f).Length -eq {size}) {{ 'have' }} else {{ \
                 New-Item -ItemType Directory -Force -Path $d | Out-Null; $p = $f + '.' + $PID + '.part'; \
                 $i = [Console]::OpenStandardInput(); $o = [IO.File]::Create($p); $i.CopyTo($o); $o.Close(); \
                 if ((Get-Item $p).Length -eq {size}) {{ Move-Item -Force $p $f; 'wrote' }} else {{ Remove-Item -Force $p }} }}"
            ));
            let java = java.map(|j| format!("\"{}\"", j.replace('"', ""))).unwrap_or_else(|| "java".into());
            let run = if shell == Shell::Cmd {
                format!("{java} -Xss4m -jar \"%USERPROFILE%\\.devcat\\jdbc\\{name}\"")
            } else {
                // single quotes: nothing in the path is expanded
                let java = java.trim_matches('"').replace('\'', "''");
                format!("& '{java}' -Xss4m -jar \"$env:USERPROFILE\\.devcat\\jdbc\\{name}\"")
            };
            (install, run)
        }
    }
}

async fn launch_remote(cfg: &ConnectionConfig, s: &Settings) -> AppResult<Launched> {
    use russh::ChannelMsg;
    let ssh = cfg.ssh.as_ref().ok_or_else(|| AppError::msg("SSH settings missing"))?;
    let session = Arc::new(super::ssh::connect_session(ssh).await?);
    let shell = detect_shell(&session).await?;
    let (install, run) = remote_commands(shell, &helper_name(), s.java.as_deref());
    let (status, out, err) = ssh_run(&session, &install, Some(HELPER_JAR)).await?;
    if !out.contains("have") && !out.contains("wrote") {
        return Err(AppError::msg(format!(
            "JDBC: couldn't copy the helper to {} (exit {}): {}",
            ssh.host,
            status.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
            err.trim()
        )));
    }
    let ch = session.channel_open_session().await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    ch.exec(true, run).await.map_err(|e| AppError::msg(format!("SSH: {e}")))?;
    let (mut read, write) = ch.split();
    let stdin = write.make_writer();
    let (mut pipe_in, pipe_out) = tokio::io::duplex(1 << 20);
    let stderr = Arc::new(parking_lot::Mutex::new(String::new()));
    let tail = stderr.clone();
    let pump = tokio::spawn(async move {
        while let Some(msg) = read.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if pipe_in.write_all(&data).await.is_err() {
                        break;
                    }
                }
                ChannelMsg::ExtendedData { data, .. } => push_tail(&tail, &data),
                ChannelMsg::ExitStatus { exit_status } => push_tail(&tail, format!("\n(exit {exit_status})").as_bytes()),
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
        // dropping `pipe_in` ends the helper's stdout
    });
    Ok(Launched { stdin: Box::new(stdin), stdout: Box::new(pipe_out), stderr, keep: Keep::Remote(session, pump), place: ssh.host.clone() })
}

/// A friendlier reason for a helper that stopped, from what it printed.
fn explain_exit(place: &str, stderr: &str) -> String {
    let s = stderr.trim();
    let lower = s.to_ascii_lowercase();
    if lower.contains("unsupportedclassversionerror") {
        return format!("JDBC: the Java on {place} is too old; the helper needs Java 11 or newer.");
    }
    if lower.contains("not recognized as") || lower.contains("command not found") || lower.contains("java: not found") || lower.contains("no such file") {
        return format!("JDBC: Java wasn't found on {place}. Install Java 11 or newer there, or set the connection's Java path.\n\n{s}");
    }
    if s.is_empty() {
        format!("JDBC: the helper on {place} stopped")
    } else {
        format!("JDBC: the helper on {place} stopped:\n{s}")
    }
}

// -------------------------------------------------------------------------------------------------
// The helper process

type Pending = Arc<parking_lot::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

struct Bridge {
    stdin: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    next: AtomicU64,
    /// Set (with the reason) once the helper stopped.
    gone: Arc<parking_lot::Mutex<Option<String>>>,
    closed: AtomicBool,
    _keep: Keep,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Bridge {
    /// Start the reader and wait for the helper's `ready` line.
    async fn start(l: Launched) -> AppResult<Self> {
        let pending: Pending = Arc::default();
        let gone: Arc<parking_lot::Mutex<Option<String>>> = Arc::default();
        let (ready_tx, ready_rx) = oneshot::channel::<Value>();
        let (p, g, stderr, place) = (pending.clone(), gone.clone(), l.stderr.clone(), l.place.clone());
        let stdout = l.stdout;
        let reader = tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                // a login banner or a driver that printed before System.out was redirected
                if !line.starts_with('{') {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                if v.get("ready").is_some() {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(v);
                    }
                    continue;
                }
                let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                let Some(tx) = p.lock().remove(&id) else { continue };
                let res = if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(v.get("error").and_then(Value::as_str).unwrap_or("JDBC error").to_string())
                };
                let _ = tx.send(res);
            }
            // stdout ended: the helper is gone. Give stderr a moment to arrive.
            tokio::time::sleep(Duration::from_millis(150)).await;
            let why = explain_exit(&place, &stderr.lock());
            *g.lock() = Some(why.clone());
            for (_, tx) in p.lock().drain() {
                let _ = tx.send(Err(why.clone()));
            }
        });
        let bridge = Bridge {
            stdin: tokio::sync::Mutex::new(l.stdin),
            pending,
            next: AtomicU64::new(1),
            gone: gone.clone(),
            closed: AtomicBool::new(false),
            _keep: l.keep,
            reader,
        };
        let ready = match tokio::time::timeout(Duration::from_secs(60), ready_rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                // the reader ended before `ready`
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Err(AppError::msg(gone.lock().clone().unwrap_or_else(|| explain_exit(&l.place, &l.stderr.lock()))));
            }
            Err(_) => return Err(AppError::msg(format!("JDBC: the helper on {} didn't start within a minute", l.place))),
        };
        let protocol = ready.get("protocol").and_then(Value::as_u64).unwrap_or(0);
        if protocol != PROTOCOL {
            return Err(AppError::msg(format!("JDBC: the helper on {} speaks protocol {protocol}, DevCat needs {PROTOCOL}", l.place)));
        }
        Ok(bridge)
    }

    async fn call(&self, op: &str, mut req: Value) -> AppResult<Value> {
        if let Some(why) = self.gone.lock().clone() {
            return Err(AppError::msg(why));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        if let Some(m) = req.as_object_mut() {
            m.insert("id".into(), json!(id));
            m.insert("op".into(), json!(op));
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        // the reader sets `gone` before it fails what's pending: look again now that ours is in
        if let Some(why) = self.gone.lock().clone() {
            self.pending.lock().remove(&id);
            return Err(AppError::msg(why));
        }
        let mut line = serde_json::to_vec(&req)?;
        line.push(b'\n');
        {
            let mut w = self.stdin.lock().await;
            let wrote = async {
                w.write_all(&line).await?;
                w.flush().await
            }
            .await;
            if let Err(e) = wrote {
                self.pending.lock().remove(&id);
                return Err(AppError::msg(self.gone.lock().clone().unwrap_or_else(|| format!("JDBC: couldn't reach the helper: {e}"))));
            }
        }
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(AppError::msg(e)),
            Err(_) => Err(AppError::msg(self.gone.lock().clone().unwrap_or_else(|| "JDBC: the helper stopped".into()))),
        }
    }

    fn is_gone(&self) -> bool {
        self.gone.lock().is_some() || self.closed.load(Ordering::SeqCst)
    }

    /// Stop using this helper (its database connection was lost): the next call starts another.
    async fn retire(&self, why: &str) {
        self.gone.lock().get_or_insert_with(|| why.to_string());
        self.close().await;
    }

    async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut w = self.stdin.lock().await;
        let _ = w.write_all(b"{\"op\":\"close\"}\n").await;
        let _ = w.flush().await;
        let _ = w.shutdown().await;
        drop(w);
        // let it close its database connection before the process goes (dropping the bridge kills
        // it): a killed helper leaves the session on the server, and some databases lose writes
        let until = Instant::now() + Duration::from_secs(5);
        while !self.reader.is_finished() && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

// -------------------------------------------------------------------------------------------------
// What the helper answers

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Info {
    product: Option<String>,
    product_version: Option<String>,
    driver: Option<String>,
    driver_version: Option<String>,
    quote: Option<String>,
    catalogs: bool,
    schemas: bool,
    catalog: Option<String>,
    schema: Option<String>,
    table_types: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JCol {
    name: String,
    data_type: i32,
    type_name: Option<String>,
    size: Option<i64>,
    digits: Option<i64>,
    nullable: bool,
    default: Option<String>,
    remarks: Option<String>,
    ordinal: u32,
    auto_increment: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JPk {
    name: Option<String>,
    columns: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JIndex {
    name: String,
    unique: bool,
    columns: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JFk {
    name: String,
    ref_schema: Option<String>,
    ref_table: String,
    columns: Vec<String>,
    ref_columns: Vec<String>,
    on_update: Option<String>,
    on_delete: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JStructure {
    name: String,
    schema: Option<String>,
    remarks: Option<String>,
    columns: Vec<JCol>,
    primary_key: JPk,
    indexes: Vec<JIndex>,
    foreign_keys: Vec<JFk>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JTable {
    name: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    remarks: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JResult {
    columns: Option<Vec<ColumnInfoIn>>,
    rows: Vec<Vec<Value>>,
    update_count: Option<i64>,
    truncated: bool,
    message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ColumnInfoIn {
    name: String,
    #[serde(rename = "type")]
    type_name: Option<String>,
}

// java.sql.Types
const CHAR_TYPES: &[i32] = &[1, 12, -1, -15, -9, -16, -2, -3]; // CHAR VARCHAR LONGVARCHAR NCHAR NVARCHAR LONGNVARCHAR BINARY VARBINARY
const DECIMAL_TYPES: &[i32] = &[2, 3]; // NUMERIC DECIMAL

/// `VARCHAR(20)`, `DECIMAL(10,2)`: the type as people write it, from the driver's parts.
fn format_type(c: &JCol) -> String {
    let name = c.type_name.clone().unwrap_or_else(|| "?".into());
    if name.contains('(') {
        return name;
    }
    match c.size {
        Some(n) if n > 0 && n < 1 << 30 && CHAR_TYPES.contains(&c.data_type) => format!("{name}({n})"),
        Some(p) if p > 0 && DECIMAL_TYPES.contains(&c.data_type) => match c.digits {
            Some(s) if s > 0 => format!("{name}({p},{s})"),
            _ => format!("{name}({p})"),
        },
        _ => name,
    }
}

/// Which kind of tree node a table type ("TABLE", "BASE TABLE", "VIEW", "SYSTEM TABLE", …) is.
fn node_kind(table_type: &str) -> NodeKind {
    let t = table_type.to_ascii_uppercase();
    if t.contains("MATERIALIZED") {
        NodeKind::MaterializedView
    } else if t.contains("VIEW") {
        NodeKind::View
    } else {
        NodeKind::Table
    }
}

/// The folder a table type gets in the tree: "TABLE" → "Tables", "SYSTEM TABLE" → "System tables".
fn folder_label(table_type: &str) -> String {
    let t = table_type.trim().to_ascii_lowercase();
    let t = if t == "base table" { "table".to_string() } else { t };
    let mut s: String = t.chars().take(1).flat_map(char::to_uppercase).chain(t.chars().skip(1)).collect();
    s.push_str(if s.ends_with('s') { "es" } else { "s" });
    s
}

/// Table types shown in the tree: not temporary ones (they're per session, and usually empty).
fn shown_types(types: &[String]) -> Vec<String> {
    types.iter().filter(|t| !t.to_ascii_uppercase().contains("TEMPORARY")).cloned().collect()
}

/// Types that are real tables (for the schema snapshot: the ER diagram and agents).
fn table_types(types: &[String]) -> Vec<String> {
    types
        .iter()
        .filter(|t| {
            let u = t.to_ascii_uppercase();
            u.contains("TABLE") && !u.contains("SYSTEM") && !u.contains("TEMPORARY")
        })
        .cloned()
        .collect()
}

fn nz(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

// -------------------------------------------------------------------------------------------------
// The driver

/// One database: one helper (and its database connection). [`JdbcDriver`] holds one per database.
pub struct JdbcOne {
    /// The helper in use; replaced when it stops or loses its database connection.
    bridge: parking_lot::Mutex<Arc<Bridge>>,
    reconnecting: tokio::sync::Mutex<()>,
    cfg: ConnectionConfig,
    /// Primary keys by table, for paging and grid edits (asked once per table).
    keys: parking_lot::Mutex<HashMap<TableRef, Vec<String>>>,
    info: Info,
    settings: Settings,
    read_only: bool,
    default_catalog: Option<String>,
}

impl JdbcOne {
    pub async fn connect(cfg: &ConnectionConfig) -> AppResult<Self> {
        let settings = Settings::of(cfg)?;
        let (bridge, info) = Self::open(cfg, &settings).await?;
        Ok(JdbcOne {
            bridge: parking_lot::Mutex::new(Arc::new(bridge)),
            reconnecting: tokio::sync::Mutex::new(()),
            cfg: cfg.clone(),
            keys: parking_lot::Mutex::new(HashMap::new()),
            info,
            settings,
            read_only: cfg.read_only,
            default_catalog: cfg.database.clone().filter(|d| !d.trim().is_empty()),
        })
    }

    /// Start a helper and connect it to the database.
    async fn open(cfg: &ConnectionConfig, settings: &Settings) -> AppResult<(Bridge, Info)> {
        let remote = cfg.ssh.as_ref().is_some_and(|s| s.enabled);
        // jars on this computer must be the ones the user saved (see `pinned_jars`)
        let jars = if remote { settings.jars.clone() } else { pinned_jars(cfg, settings)? };
        let launched = if remote { launch_remote(cfg, settings).await? } else { launch_local(settings).await? };
        let bridge = Bridge::start(launched).await?;
        let req = json!({
            "jars": jars,
            "driverClass": settings.driver_class,
            "url": settings.url,
            "user": cfg.user,
            "password": cfg.password.clone().unwrap_or_default(),
            "props": settings.props,
            "readOnly": cfg.read_only,
            "encoding": settings.encoding,
        });
        let info = match tokio::time::timeout(Duration::from_secs(90), bridge.call("connect", req)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                bridge.close().await;
                return Err(e);
            }
            Err(_) => {
                bridge.close().await;
                return Err(AppError::msg("JDBC: the driver didn't connect within 90 seconds"));
            }
        };
        Ok((bridge, serde_json::from_value(info)?))
    }

    /// The helper to send to: the current one, or a new one if it stopped (SSH dropped, the
    /// computer slept, the database closed an idle connection).
    async fn live(&self) -> AppResult<Arc<Bridge>> {
        let b = self.bridge.lock().clone();
        if !b.is_gone() {
            return Ok(b);
        }
        let _one = self.reconnecting.lock().await;
        let b = self.bridge.lock().clone();
        if !b.is_gone() {
            return Ok(b);
        }
        let (fresh, _) = Self::open(&self.cfg, &self.settings).await?;
        let fresh = Arc::new(fresh);
        *self.bridge.lock() = fresh.clone();
        Ok(fresh)
    }

    async fn call(&self, op: &str, req: Value) -> AppResult<Value> {
        let b = self.live().await?;
        let r = b.call(op, req).await;
        // SQLState class 08: the database connection is gone; the next call reconnects
        if let Err(e) = &r
            && e.to_string().contains("[SQLState 08")
        {
            b.retire("JDBC: the database connection was lost").await;
        }
        r
    }

    /// An identifier quoted the way the driver says (`getIdentifierQuoteString`); bare when it
    /// has none.
    fn q(&self, ident: &str) -> String {
        match self.info.quote.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => format!("{q}{}{q}", ident.replace(q, &format!("{q}{q}"))),
            // a name that isn't a plain word goes in standard quotes: never into SQL as it is
            None if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => ident.to_string(),
            None => format!("\"{}\"", ident.replace('"', "\"\"")),
        }
    }

    /// `schema.table`; the catalog is set on the session instead (`catalog` of the request).
    fn qualified(&self, t: &TableRef) -> String {
        match t.schema.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => format!("{}.{}", self.q(s), self.q(&t.name)),
            None => self.q(&t.name),
        }
    }

    fn catalog_of(&self, db: Option<&str>) -> Option<String> {
        db.filter(|d| !d.is_empty()).map(String::from).or_else(|| self.default_catalog.clone())
    }

    async fn exec_one(&self, sql: &str, catalog: Option<&str>, schema: Option<&str>, max_rows: usize, skip: u64) -> AppResult<Vec<JResult>> {
        let v = self
            .call("exec", json!({ "sql": sql, "catalog": catalog, "schema": schema, "maxRows": max_rows, "skip": skip }))
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    async fn structure(&self, t: &TableRef) -> AppResult<JStructure> {
        let v = self
            .call("structure", json!({ "catalog": self.catalog_of(t.database.as_deref()), "schema": t.schema, "table": t.name }))
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    fn to_structure(&self, catalog: Option<String>, schema: Option<String>, s: JStructure) -> TableStructure {
        let pk = &s.primary_key.columns;
        let mut columns: Vec<ColumnDef> = s
            .columns
            .iter()
            .map(|c| ColumnDef {
                name: c.name.clone(),
                data_type: format_type(c),
                nullable: c.nullable,
                default: c.default.clone().filter(|d| !d.is_empty()),
                is_primary_key: pk.contains(&c.name),
                auto_increment: c.auto_increment,
                comment: c.remarks.clone().filter(|r| !r.is_empty()),
                ordinal: c.ordinal,
            })
            .collect();
        columns.sort_by_key(|c| c.ordinal);
        let mut indexes: Vec<IndexDef> = s
            .indexes
            .iter()
            .map(|i| IndexDef {
                name: i.name.clone(),
                columns: i.columns.clone(),
                unique: i.unique,
                primary: !pk.is_empty() && (s.primary_key.name.as_deref() == Some(i.name.as_str()) || (i.unique && &i.columns == pk)),
                method: None,
            })
            .collect();
        if !pk.is_empty() && !indexes.iter().any(|i| i.primary) {
            indexes.insert(
                0,
                IndexDef { name: s.primary_key.name.clone().unwrap_or_else(|| "PRIMARY KEY".into()), columns: pk.clone(), unique: true, primary: true, method: None },
            );
        }
        TableStructure {
            table: Some(TableRef { database: catalog, schema: s.schema.clone().or(schema), name: s.name.clone() }),
            columns,
            indexes,
            foreign_keys: s
                .foreign_keys
                .iter()
                .map(|f| ForeignKeyDef {
                    name: f.name.clone(),
                    columns: f.columns.clone(),
                    ref_schema: f.ref_schema.clone(),
                    ref_table: f.ref_table.clone(),
                    ref_columns: f.ref_columns.clone(),
                    on_update: f.on_update.clone(),
                    on_delete: f.on_delete.clone(),
                })
                .collect(),
            comment: s.remarks.filter(|r| !r.is_empty()),
        }
    }

    fn to_results(statement: &str, results: Vec<JResult>, elapsed: u64) -> Vec<QueryResult> {
        let mut out: Vec<QueryResult> = results
            .into_iter()
            .map(|r| QueryResult {
                statement: statement.to_string(),
                columns: r
                    .columns
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| ColumnInfo { name: c.name, type_name: c.type_name.unwrap_or_default() })
                    .collect(),
                rows: r.rows,
                rows_affected: r.update_count.and_then(|n| u64::try_from(n).ok()),
                elapsed_ms: elapsed,
                truncated: r.truncated,
                message: r.message,
            })
            .collect();
        if out.is_empty() {
            out.push(QueryResult { statement: statement.to_string(), elapsed_ms: elapsed, ..Default::default() });
        }
        out
    }

    /// Parameterized INSERT / UPDATE / DELETE for a grid edit, quoted the driver's way.
    fn change_sql(&self, t: &TableRef, change: &RowChange) -> AppResult<(String, Vec<Value>, bool)> {
        let qn = self.qualified(t);
        let mut params = Vec::new();
        let where_of = |key: &Map<String, Value>, params: &mut Vec<Value>| -> AppResult<String> {
            if key.is_empty() {
                return Err(AppError::msg("Cannot identify the row: the table has no primary key"));
            }
            Ok(key
                .iter()
                .map(|(k, v)| {
                    if v.is_null() {
                        format!("{} IS NULL", self.q(k))
                    } else {
                        params.push(v.clone());
                        format!("{} = ?", self.q(k))
                    }
                })
                .collect::<Vec<_>>()
                .join(" AND "))
        };
        Ok(match change {
            RowChange::Insert { values } if values.is_empty() => (format!("INSERT INTO {qn} DEFAULT VALUES"), params, false),
            RowChange::Insert { values } => {
                let cols: Vec<String> = values.keys().map(|k| self.q(k)).collect();
                params.extend(values.values().cloned());
                (format!("INSERT INTO {qn} ({}) VALUES ({})", cols.join(", "), vec!["?"; values.len()].join(", ")), params, false)
            }
            RowChange::Update { key, values } => {
                if values.is_empty() {
                    return Err(AppError::msg("Update without any changed column"));
                }
                let sets: Vec<String> = values.keys().map(|k| format!("{} = ?", self.q(k))).collect();
                params.extend(values.values().cloned());
                let w = where_of(key, &mut params)?;
                (format!("UPDATE {qn} SET {} WHERE {w}", sets.join(", ")), params, true)
            }
            RowChange::Delete { key } => {
                let w = where_of(key, &mut params)?;
                (format!("DELETE FROM {qn} WHERE {w}"), params, true)
            }
        })
    }

    async fn list<T: for<'de> Deserialize<'de>>(&self, op: &str, req: Value) -> AppResult<Vec<T>> {
        Ok(serde_json::from_value(self.call(op, req).await?)?)
    }
}

#[async_trait]
impl Driver for JdbcOne {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Jdbc
    }

    fn dialect(&self) -> Dialect {
        D
    }

    async fn server_version(&self) -> AppResult<String> {
        if self.call("ping", json!({})).await? == Value::Bool(false) {
            let b = self.bridge.lock().clone();
            b.retire("JDBC: the database connection was lost").await;
            return Err(AppError::msg("JDBC: the database connection was lost; try again to reconnect"));
        }
        let i = &self.info;
        let product = [i.product.as_deref(), i.product_version.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let driver = [i.driver.as_deref(), i.driver_version.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        Ok(match (product.is_empty(), driver.is_empty()) {
            (false, false) => format!("{product} (JDBC: {driver})"),
            (false, true) => product,
            (true, false) => format!("JDBC: {driver}"),
            (true, true) => "JDBC".into(),
        })
    }

    /// Tree paths are always `[catalog, schema, folder, table, part]`, with `""` for a level the
    /// database doesn't have; the tree just skips those levels.
    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>> {
        let node = |path: Vec<String>, label: String, kind: NodeKind, has_children: bool, detail: Option<String>, table: Option<TableRef>| TreeNode {
            path,
            label,
            kind,
            has_children,
            detail,
            table,
        };
        let folders = |cat: &str, sch: &str| -> Vec<TreeNode> {
            let mut out: Vec<TreeNode> = shown_types(&self.info.table_types)
                .iter()
                .map(|t| node(vec![cat.into(), sch.into(), format!("t:{t}")], folder_label(t), NodeKind::Folder, true, None, None))
                .collect();
            if out.is_empty() {
                out.push(node(vec![cat.into(), sch.into(), "t:".into()], "Tables".into(), NodeKind::Folder, true, None, None));
            }
            out.push(node(vec![cat.into(), sch.into(), "procedures".into()], "Procedures".into(), NodeKind::Folder, true, None, None));
            out
        };
        let schema_nodes = |cat: &str, schemas: Vec<String>| -> Vec<TreeNode> {
            schemas.into_iter().map(|s| node(vec![cat.into(), s.clone()], s, NodeKind::Schema, true, None, None)).collect()
        };
        match path {
            [] => {
                if self.info.catalogs {
                    let cats = self.list_databases().await?;
                    if !cats.is_empty() {
                        return Ok(cats.into_iter().map(|c| node(vec![c.clone()], c, NodeKind::Database, true, None, None)).collect());
                    }
                }
                if self.info.schemas {
                    let schemas: Vec<String> = self.list("schemas", json!({})).await?;
                    if !schemas.is_empty() {
                        return Ok(schema_nodes("", schemas));
                    }
                }
                Ok(folders("", ""))
            }
            [cat] => {
                if self.info.schemas {
                    let schemas: Vec<String> = self.list("schemas", json!({ "catalog": nz(cat) })).await?;
                    if !schemas.is_empty() {
                        return Ok(schema_nodes(cat, schemas));
                    }
                }
                Ok(folders(cat, ""))
            }
            [cat, sch] => Ok(folders(cat, sch)),
            [cat, sch, folder] => {
                let tref = |name: &str| TableRef { database: nz(cat), schema: nz(sch), name: name.to_string() };
                if folder == "procedures" {
                    #[derive(Deserialize)]
                    struct P {
                        name: String,
                        remarks: Option<String>,
                    }
                    let procs: Vec<P> = self.list("procedures", json!({ "catalog": nz(cat), "schema": nz(sch) })).await?;
                    return Ok(procs
                        .into_iter()
                        .map(|p| {
                            let mut path = path.to_vec();
                            path.push(p.name.clone());
                            node(path, p.name, NodeKind::Procedure, false, p.remarks.filter(|r| !r.is_empty()), None)
                        })
                        .collect());
                }
                let Some(ty) = folder.strip_prefix("t:") else { return Ok(vec![]) };
                let types = if ty.is_empty() { Value::Null } else { json!([ty]) };
                let tables: Vec<JTable> = self.list("tables", json!({ "catalog": nz(cat), "schema": nz(sch), "types": types })).await?;
                Ok(tables
                    .into_iter()
                    .map(|t| {
                        let kind = node_kind(t.kind.as_deref().unwrap_or(ty));
                        let expandable = kind == NodeKind::Table;
                        let mut p = path.to_vec();
                        p.push(t.name.clone());
                        node(p, t.name.clone(), kind, expandable, t.remarks.filter(|r| !r.is_empty()), Some(tref(&t.name)))
                    })
                    .collect())
            }
            [_, _, _, _] => Ok([("columns", "Columns"), ("indexes", "Indexes"), ("foreign_keys", "Foreign Keys")]
                .iter()
                .map(|(id, label)| {
                    let mut p = path.to_vec();
                    p.push(id.to_string());
                    node(p, label.to_string(), NodeKind::Folder, true, None, None)
                })
                .collect()),
            [cat, sch, _, table, sub] => {
                let t = self.to_structure(nz(cat), nz(sch), self.structure(&TableRef { database: nz(cat), schema: nz(sch), name: table.clone() }).await?);
                let child = |name: &str| {
                    let mut p = path.to_vec();
                    p.push(name.to_string());
                    p
                };
                Ok(match sub.as_str() {
                    "columns" => t
                        .columns
                        .iter()
                        .map(|c| {
                            let mut detail = c.data_type.clone();
                            if c.is_primary_key {
                                detail.push_str(" PK");
                            }
                            if !c.nullable {
                                detail.push_str(" NOT NULL");
                            }
                            node(child(&c.name), c.name.clone(), NodeKind::Column, false, Some(detail), None)
                        })
                        .collect(),
                    "indexes" => t
                        .indexes
                        .iter()
                        .map(|i| {
                            let detail = format!("{}({})", if i.primary { "PRIMARY " } else if i.unique { "UNIQUE " } else { "" }, i.columns.join(", "));
                            node(child(&i.name), i.name.clone(), NodeKind::Index, false, Some(detail), None)
                        })
                        .collect(),
                    "foreign_keys" => t
                        .foreign_keys
                        .iter()
                        .map(|f| {
                            let target = [f.ref_schema.as_deref(), Some(f.ref_table.as_str())].into_iter().flatten().collect::<Vec<_>>().join(".");
                            let detail = format!("({}) → {target}({})", f.columns.join(", "), f.ref_columns.join(", "));
                            node(child(&f.name), f.name.clone(), NodeKind::ForeignKey, false, Some(detail), None)
                        })
                        .collect(),
                    _ => vec![],
                })
            }
            _ => Ok(vec![]),
        }
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        if !self.info.catalogs {
            return Ok(vec![]);
        }
        let mut cats: Vec<String> = self.list("catalogs", json!({})).await?;
        cats.retain(|c| !c.is_empty());
        Ok(cats)
    }

    async fn execute(&self, script: &str, opts: &ExecOptions) -> AppResult<Vec<QueryResult>> {
        let stmts = sc::split_script(D, script);
        if stmts.is_empty() {
            return Ok(vec![]);
        }
        if self.read_only
            && let Some(i) = stmts.iter().position(|s| !read_only_sql(&s.sql))
        {
            return Err(sc::read_only_error(i + 1));
        }
        let catalog = self.catalog_of(opts.database.as_deref());
        let schema = opts.schema.clone().filter(|s| !s.is_empty()).or_else(|| self.settings.schema.clone());
        let max_rows = opts.max_rows.unwrap_or(1000).max(1);
        let mut out = Vec::new();
        for (i, st) in stmts.iter().enumerate() {
            let started = Instant::now();
            match self.exec_one(&st.sql, catalog.as_deref(), schema.as_deref(), max_rows, 0).await {
                Ok(r) => out.extend(Self::to_results(&st.sql, r, started.elapsed().as_millis() as u64)),
                Err(e) => return Err(sc::statement_error(i + 1, stmts.len(), st.line(script), &st.sql, e)),
            }
        }
        Ok(out)
    }

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage> {
        let started = Instant::now();
        let filter = req.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
        if let Some(f) = filter
            && (sc::split_statements(D, f).len() > 1 || (self.read_only && !read_only_sql(&format!("SELECT 1 FROM t WHERE {f}"))))
        {
            return Err(AppError::msg("The filter must be a single read-only SQL expression"));
        }
        let qn = self.qualified(&req.table);
        let catalog = self.catalog_of(req.table.database.as_deref());
        let cached = self.keys.lock().get(&req.table).cloned();
        let pk = match cached {
            Some(pk) => pk,
            None => {
                let pk = self.structure(&req.table).await.map(|s| s.primary_key.columns).unwrap_or_default();
                self.keys.lock().insert(req.table.clone(), pk.clone());
                pk
            }
        };
        let where_sql = sc::where_clause(filter);
        let order: Vec<String> = if req.sort.is_empty() {
            pk.iter().map(|c| self.q(c)).collect()
        } else {
            req.sort.iter().map(|s| format!("{}{}", self.q(&s.column), if s.desc { " DESC" } else { " ASC" })).collect()
        };
        let order_sql = if order.is_empty() { String::new() } else { format!(" ORDER BY {}", order.join(", ")) };
        // Paging without LIMIT / OFFSET, which not every database has: the helper skips `offset`
        // rows and stops after `limit` (with `setMaxRows`).
        let sql = format!("SELECT * FROM {qn}{where_sql}{order_sql}");
        let limit = if req.limit == 0 { 1000 } else { req.limit };
        let r = self.exec_one(&sql, catalog.as_deref(), None, limit as usize, req.offset).await?;
        let mut result = Self::to_results(&sql, r, 0).into_iter().next().unwrap_or_default();
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        let count = format!("SELECT COUNT(*) FROM {qn}{where_sql}");
        let total = self
            .exec_one(&count, catalog.as_deref(), None, 1, 0)
            .await
            .ok()
            .and_then(|r| r.into_iter().next())
            .and_then(|r| r.rows.into_iter().next())
            .and_then(|row| row.into_iter().next())
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        Ok(TableDataPage { result, total, primary_key: pk })
    }

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure> {
        let s = self.structure(table).await?;
        if s.columns.is_empty() {
            return Err(AppError::msg(format!("Object {} not found", self.qualified(table))));
        }
        Ok(self.to_structure(self.catalog_of(table.database.as_deref()), table.schema.clone(), s))
    }

    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64> {
        if self.read_only {
            return Err(AppError::msg("Connection is read-only"));
        }
        if changes.is_empty() {
            return Ok(0);
        }
        let mut statements = Vec::with_capacity(changes.len());
        for c in changes {
            let (sql, params, expect_one) = self.change_sql(table, c)?;
            statements.push(json!({ "sql": sql, "params": params, "expectOne": expect_one }));
        }
        let n = self
            .call("batch", json!({ "catalog": self.catalog_of(table.database.as_deref()), "statements": statements }))
            .await?;
        Ok(n.as_u64().unwrap_or(0))
    }

    /// A `CREATE TABLE` put together from what the driver reports (JDBC can't ask the database
    /// for its own DDL); views and other objects have none.
    async fn object_ddl(&self, table: &TableRef, kind: &NodeKind) -> AppResult<String> {
        if *kind != NodeKind::Table {
            return Err(AppError::Unsupported("the definition of a view or routine over JDBC".into()));
        }
        let t = self.table_structure(table).await?;
        let mut lines: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                let mut l = format!("  {} {}", self.q(&c.name), c.data_type);
                if let Some(d) = &c.default {
                    l.push_str(&format!(" DEFAULT {d}"));
                }
                if !c.nullable {
                    l.push_str(" NOT NULL");
                }
                l
            })
            .collect();
        let pk: Vec<String> = t.columns.iter().filter(|c| c.is_primary_key).map(|c| self.q(&c.name)).collect();
        if !pk.is_empty() {
            lines.push(format!("  PRIMARY KEY ({})", pk.join(", ")));
        }
        for f in &t.foreign_keys {
            let target = match &f.ref_schema {
                Some(s) if !s.is_empty() => format!("{}.{}", self.q(s), self.q(&f.ref_table)),
                _ => self.q(&f.ref_table),
            };
            let cols = |c: &[String]| c.iter().map(|c| self.q(c)).collect::<Vec<_>>().join(", ");
            let mut l = format!("  CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {target} ({})", self.q(&f.name), cols(&f.columns), cols(&f.ref_columns));
            if let Some(a) = &f.on_delete {
                l.push_str(&format!(" ON DELETE {a}"));
            }
            if let Some(a) = &f.on_update {
                l.push_str(&format!(" ON UPDATE {a}"));
            }
            lines.push(l);
        }
        let mut sql = format!("-- put together from the JDBC driver's metadata\nCREATE TABLE {} (\n{}\n);", self.qualified(table), lines.join(",\n"));
        for i in t.indexes.iter().filter(|i| !i.primary) {
            sql.push_str(&format!(
                "\nCREATE {}INDEX {} ON {} ({});",
                if i.unique { "UNIQUE " } else { "" },
                self.q(&i.name),
                self.qualified(table),
                i.columns.iter().map(|c| self.q(c)).collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(sql)
    }

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot> {
        let catalog = self.catalog_of(database);
        // Without a schema, the session's own (not every schema: some databases list hundreds
        // of system ones).
        let schema = schema
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| self.settings.schema.clone())
            .or_else(|| self.info.schema.clone().filter(|_| self.info.schemas));
        let types = table_types(&self.info.table_types);
        let types = if types.is_empty() { Value::Null } else { json!(types) };
        #[derive(Deserialize)]
        struct Snap {
            tables: Vec<JStructure>,
        }
        let snap: Snap = serde_json::from_value(
            self.call("snapshot", json!({ "catalog": catalog, "schema": schema, "types": types, "limit": 1000 })).await?,
        )?;
        Ok(SchemaSnapshot {
            tables: snap.tables.into_iter().map(|t| self.to_structure(catalog.clone(), schema.clone(), t)).collect(),
            views: vec![],
        })
    }

    async fn cancel(&self) -> AppResult<()> {
        // the helper running the query, not a new one
        let b = self.bridge.lock().clone();
        b.call("cancel", json!({})).await.map(|_| ())
    }

    async fn close(&self) {
        let b = self.bridge.lock().clone();
        b.close().await;
    }
}

// -------------------------------------------------------------------------------------------------
// Several databases on one server

/// The databases a connection lists (`options.databases`, separated by `,` `;` or lines), for
/// drivers that can't list a server's databases themselves (SQLBase's): the URL names the one to
/// use with `{db}`. Empty: the URL names one database, as usual.
pub fn databases_of(cfg: &ConnectionConfig) -> AppResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for d in cfg.options.get("databases").map(String::as_str).unwrap_or("").split([',', ';', '\n']).map(str::trim) {
        if d.is_empty() || out.iter().any(|o| o == d) {
            continue;
        }
        // it goes into the URL: no characters that would end the name or start a property
        if d.len() > 64 || !d.chars().all(|c| c.is_alphanumeric() || "_-$#@".contains(c)) {
            return Err(AppError::msg(format!("JDBC: {d:?} isn't a database name DevCat can put in the URL")));
        }
        out.push(d.to_string());
    }
    let url = cfg.url.as_deref().unwrap_or("");
    match (out.is_empty(), url.contains(DB_SLOT)) {
        (true, true) => Err(AppError::msg("JDBC: the URL has {db}: list the databases to use in its place")),
        (false, false) => Err(AppError::msg("JDBC: with several databases, put {db} in the URL where the database name goes")),
        _ => Ok(out),
    }
}

const DB_SLOT: &str = "{db}";

/// A JDBC connection: one database, or several on the same server (`databases_of`), each with its
/// own helper, started the first time it's used. With several, the tree's first level and
/// `TableRef::database` are the database; each one is browsed as a database without catalogs.
pub struct JdbcDriver {
    cfg: ConnectionConfig,
    /// empty: the URL's one database, kept under ""
    dbs: Vec<String>,
    default_db: String,
    /// per database, connected once; a failed connect leaves it empty, to try again
    open: parking_lot::Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Arc<JdbcOne>>>>>,
}

impl JdbcDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> AppResult<Self> {
        let dbs = databases_of(cfg)?;
        // the catalog field names the default database when there are several
        let default_db = match cfg.database.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) if dbs.iter().any(|x| x == d) => d.to_string(),
            Some(d) if !dbs.is_empty() => return Err(AppError::msg(format!("JDBC: the default database {d:?} isn't in the list of databases"))),
            _ => dbs.first().cloned().unwrap_or_default(),
        };
        let d = JdbcDriver { cfg: cfg.clone(), dbs, default_db, open: parking_lot::Mutex::new(HashMap::new()) };
        // connect now, so a wrong URL, password or Java shows when the connection opens
        d.one(None).await?;
        Ok(d)
    }

    fn several(&self) -> bool {
        !self.dbs.is_empty()
    }

    /// The database a request is for: the one it names, else the default.
    fn pick(&self, db: Option<&str>) -> AppResult<String> {
        if !self.several() {
            return Ok(String::new());
        }
        match db.map(str::trim).filter(|d| !d.is_empty()) {
            None => Ok(self.default_db.clone()),
            Some(d) if self.dbs.iter().any(|x| x == d) => Ok(d.to_string()),
            Some(d) => Err(AppError::msg(format!("JDBC: {d:?} isn't one of this connection's databases ({})", self.dbs.join(", ")))),
        }
    }

    async fn one(&self, db: Option<&str>) -> AppResult<Arc<JdbcOne>> {
        let db = self.pick(db)?;
        // one slot per database: a slow database doesn't hold up the others
        let slot = self.open.lock().entry(db.clone()).or_default().clone();
        slot.get_or_try_init(|| async {
            let mut cfg = self.cfg.clone();
            if self.several() {
                cfg.url = cfg.url.map(|u| u.replace(DB_SLOT, &db));
                cfg.database = None;
            }
            JdbcOne::connect(&cfg).await.map(Arc::new)
        })
        .await
        .cloned()
    }

    /// A table as its database's helper sees it: without the database.
    fn inner(&self, t: &TableRef) -> TableRef {
        if self.several() { TableRef { database: None, ..t.clone() } } else { t.clone() }
    }

    /// A result from a database's helper, named with its database again.
    fn outer(&self, db: &str, t: Option<TableRef>) -> Option<TableRef> {
        if self.several() { t.map(|t| TableRef { database: Some(db.to_string()), ..t }) } else { t }
    }
}

#[async_trait]
impl Driver for JdbcDriver {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Jdbc
    }

    fn dialect(&self) -> Dialect {
        D
    }

    async fn server_version(&self) -> AppResult<String> {
        self.one(None).await?.server_version().await
    }

    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>> {
        if !self.several() {
            return self.one(None).await?.children(path).await;
        }
        let Some((db, rest)) = path.split_first() else {
            return Ok(self
                .dbs
                .iter()
                .map(|d| TreeNode { path: vec![d.clone()], label: d.clone(), kind: NodeKind::Database, has_children: true, detail: None, table: None })
                .collect());
        };
        let one = self.one(Some(db)).await?;
        // the database takes the catalog's place in the path
        let mut inner = vec![String::new()];
        inner.extend_from_slice(rest);
        let mut nodes = one.children(&inner).await?;
        for n in &mut nodes {
            if let Some(first) = n.path.first_mut() {
                *first = db.clone();
            }
            n.table = self.outer(db, n.table.take());
        }
        Ok(nodes)
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        if self.several() { Ok(self.dbs.clone()) } else { self.one(None).await?.list_databases().await }
    }

    async fn execute(&self, script: &str, opts: &ExecOptions) -> AppResult<Vec<QueryResult>> {
        if !self.several() {
            return self.one(None).await?.execute(script, opts).await;
        }
        let one = self.one(opts.database.as_deref()).await?;
        one.execute(script, &ExecOptions { database: None, ..opts.clone() }).await
    }

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage> {
        let one = self.one(req.table.database.as_deref()).await?;
        one.table_data(&TableDataRequest { table: self.inner(&req.table), ..req.clone() }).await
    }

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure> {
        let db = self.pick(table.database.as_deref())?;
        let mut s = self.one(Some(&db)).await?.table_structure(&self.inner(table)).await?;
        s.table = self.outer(&db, s.table.take());
        Ok(s)
    }

    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64> {
        self.one(table.database.as_deref()).await?.apply_changes(&self.inner(table), changes).await
    }

    async fn object_ddl(&self, table: &TableRef, kind: &NodeKind) -> AppResult<String> {
        self.one(table.database.as_deref()).await?.object_ddl(&self.inner(table), kind).await
    }

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot> {
        if !self.several() {
            return self.one(None).await?.schema_snapshot(database, schema).await;
        }
        let db = self.pick(database)?;
        let mut snap = self.one(Some(&db)).await?.schema_snapshot(None, schema).await?;
        for t in &mut snap.tables {
            t.table = self.outer(&db, t.table.take());
        }
        for (v, _) in &mut snap.views {
            v.database = Some(db.clone());
        }
        Ok(snap)
    }

    async fn cancel(&self) -> AppResult<()> {
        let open: Vec<Arc<JdbcOne>> = self.open.lock().values().filter_map(|s| s.get().cloned()).collect();
        let mut r = Ok(());
        for o in open {
            if let Err(e) = o.cancel().await {
                r = Err(e);
            }
        }
        r
    }

    async fn close(&self) {
        let open: Vec<Arc<JdbcOne>> = self.open.lock().drain().filter_map(|(_, s)| s.get().cloned()).collect();
        for o in open {
            o.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(v: Value) -> ConnectionConfig {
        let mut base = json!({ "id": "j", "name": "j", "kind": "jdbc", "url": "jdbc:h2:mem:x", "options": { "driverJars": "/a.jar" } });
        if let (Some(b), Some(o)) = (base.as_object_mut(), v.as_object()) {
            for (k, v) in o {
                b.insert(k.clone(), v.clone());
            }
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn several_databases_setting() {
        let dbs = |url: &str, d: &str| databases_of(&cfg(json!({ "url": url, "options": { "driverJars": "/a.jar", "databases": d } })));
        assert_eq!(dbs("jdbc:sqlbase://h:2155/{db}", " OFFICE, SALES;\nOFFICE\n").unwrap(), vec!["OFFICE", "SALES"]);
        assert_eq!(dbs("jdbc:sqlbase://h:2155/OFFICE", "").unwrap(), Vec::<String>::new());
        assert!(dbs("jdbc:sqlbase://h:2155/OFFICE", "A").unwrap_err().to_string().contains("{db}"));
        assert!(dbs("jdbc:sqlbase://h:2155/{db}", "").unwrap_err().to_string().contains("list the databases"));
        // nothing that would change the URL around the name
        for bad in ["A/B", "A;user=x", "A?x=1", "A B", "A:1", "A&b"] {
            assert!(dbs("jdbc:x://h/{db}", bad).is_err(), "{bad}");
        }
        assert_eq!(dbs("jdbc:x://h/{db}", "受注DB").unwrap(), vec!["受注DB"]);
    }

    #[tokio::test]
    async fn h2_several_databases() {
        let Some(mut cfg) = h2() else {
            eprintln!("skipping: set DEVCAT_TEST_JAVA and DEVCAT_TEST_H2_JAR");
            return;
        };
        let url = cfg.url.clone().unwrap();
        cfg.url = Some(url.replace("/db;", "/{db};"));
        cfg.options.insert("databases".into(), "A, B".into());
        cfg.database = Some("B".into());
        let d = JdbcDriver::connect(&cfg).await.unwrap();
        let on = |db: &str| ExecOptions { database: Some(db.into()), schema: None, max_rows: Some(100) };
        d.execute("CREATE TABLE ONLY_A(ID INT PRIMARY KEY, V VARCHAR(10)); INSERT INTO ONLY_A VALUES (1, 'x')", &on("A")).await.unwrap();
        // B is another database: it doesn't have A's table; no database means the default, B
        assert!(d.execute("SELECT * FROM ONLY_A", &on("B")).await.is_err());
        assert!(d.execute("SELECT * FROM ONLY_A", &opts()).await.is_err());
        assert!(d.execute("SELECT 1", &on("C")).await.unwrap_err().to_string().contains("isn't one of"));
        assert_eq!(d.list_databases().await.unwrap(), vec!["A", "B"]);

        let top = d.children(&[]).await.unwrap();
        assert_eq!(top.iter().map(|n| (n.label.as_str(), n.kind.clone())).collect::<Vec<_>>(), vec![("A", NodeKind::Database), ("B", NodeKind::Database)]);
        // walk A down to its table: paths keep the database first, tables name it
        let mut path = vec!["A".to_string()];
        let table = loop {
            let kids = d.children(&path).await.unwrap();
            assert!(kids.iter().all(|k| k.path[0] == "A"), "{kids:?}");
            if let Some(t) = kids.iter().find(|k| k.label == "ONLY_A") {
                break t.table.clone().unwrap();
            }
            let next = kids.iter().find(|k| k.label == "PUBLIC" || k.path.last().is_some_and(|p| p.starts_with("t:") && k.label.contains("Table"))).expect("a way down");
            path = next.path.clone();
        };
        assert_eq!(table.database.as_deref(), Some("A"));
        let page = d.table_data(&TableDataRequest { table: table.clone(), offset: 0, limit: 10, filter: None, sort: vec![] }).await.unwrap();
        assert_eq!(page.result.rows, vec![vec![json!(1), json!("x")]]);
        assert_eq!(page.primary_key, vec!["ID"]);
        let s = d.table_structure(&table).await.unwrap();
        assert_eq!(s.table.unwrap().database.as_deref(), Some("A"));
        let snap = d.schema_snapshot(Some("A"), Some("PUBLIC")).await.unwrap();
        assert!(snap.tables.iter().any(|t| t.table.as_ref().is_some_and(|r| r.name == "ONLY_A" && r.database.as_deref() == Some("A"))));
        d.close().await;
    }

    #[test]
    fn text_encoding_setting() {
        let enc = |e: &str| Settings::of(&cfg(json!({ "options": { "driverJars": "/a.jar", "encoding": e } }))).map(|s| s.encoding);
        assert_eq!(enc(" MS932 ").unwrap().as_deref(), Some("MS932"));
        assert_eq!(enc("").unwrap(), None);
        assert_eq!(enc("EUC-JP").unwrap().as_deref(), Some("EUC-JP"));
        assert!(enc("MS932 -jar x").is_err() && enc("a\"b").is_err());
    }

    #[test]
    fn settings_from_config() {
        let s = Settings::of(&cfg(json!({ "options": {
            "driverJars": " /a.jar ;\n~/b.jar\n\n", "driverClass": " x.Driver ", "properties": "# note\nssl = true\nk=v=w\n", "schema": ""
        }})))
        .unwrap();
        assert_eq!(s.jars, vec!["/a.jar", "~/b.jar"]);
        assert_eq!(s.driver_class.as_deref(), Some("x.Driver"));
        assert_eq!(s.schema, None);
        assert_eq!(s.props, json!({ "ssl": "true", "k": "v=w" }).as_object().unwrap().clone());
        assert!(Settings::of(&cfg(json!({ "url": "postgres://x" }))).is_err(), "not a JDBC URL");
        assert!(Settings::of(&cfg(json!({ "options": {} }))).is_err(), "no driver jar");
        assert!(Settings::of(&cfg(json!({ "options": { "driverJars": "/a.jar", "properties": "nokey" } }))).is_err());
    }

    #[test]
    fn types_and_folders() {
        let c = |t: &str, dt: i32, size: Option<i64>, digits: Option<i64>| JCol { type_name: Some(t.into()), data_type: dt, size, digits, ..Default::default() };
        assert_eq!(format_type(&c("VARCHAR", 12, Some(20), None)), "VARCHAR(20)");
        assert_eq!(format_type(&c("DECIMAL", 3, Some(10), Some(2))), "DECIMAL(10,2)");
        assert_eq!(format_type(&c("NUMERIC", 2, Some(8), Some(0))), "NUMERIC(8)");
        assert_eq!(format_type(&c("INTEGER", 4, Some(10), Some(0))), "INTEGER");
        assert_eq!(format_type(&c("CLOB", 2005, Some(i32::MAX as i64), None)), "CLOB");
        assert_eq!(format_type(&c("nvarchar(max)", -9, Some(i32::MAX as i64), None)), "nvarchar(max)");
        assert_eq!(folder_label("TABLE"), "Tables");
        assert_eq!(folder_label("BASE TABLE"), "Tables");
        assert_eq!(folder_label("SYSTEM TABLE"), "System tables");
        assert_eq!(folder_label("VIEW"), "Views");
        assert_eq!(folder_label("ALIAS"), "Aliases");
        assert_eq!(node_kind("VIEW"), NodeKind::View);
        assert_eq!(node_kind("MATERIALIZED VIEW"), NodeKind::MaterializedView);
        assert_eq!(node_kind("SYSTEM TABLE"), NodeKind::Table);
        let types: Vec<String> = ["BASE TABLE", "GLOBAL TEMPORARY", "SYSTEM TABLE", "VIEW"].map(String::from).to_vec();
        assert_eq!(shown_types(&types), vec!["BASE TABLE", "SYSTEM TABLE", "VIEW"]);
        assert_eq!(table_types(&types), vec!["BASE TABLE"]);
    }

    #[test]
    fn remote_commands_per_shell() {
        let (install, run) = remote_commands(Shell::Posix, "devcat-jdbc-abc.jar", None);
        assert!(install.starts_with("exec sh -c '") && install.contains("$HOME/.devcat/jdbc") && install.contains(&format!("= \"{}\" ]", HELPER_JAR.len())), "{install}");
        assert_eq!(run, "exec sh -lc 'exec java -Xss4m -jar \"$HOME/.devcat/jdbc/devcat-jdbc-abc.jar\"'");
        let (_, run) = remote_commands(Shell::Posix, "x.jar", Some("/opt/my java/bin/java"));
        assert_eq!(run, "exec sh -lc 'exec '\\''/opt/my java/bin/java'\\'' -Xss4m -jar \"$HOME/.devcat/jdbc/x.jar\"'");
        let (install, run) = remote_commands(Shell::Cmd, "x.jar", None);
        assert!(install.starts_with("powershell -NoProfile -NonInteractive -EncodedCommand "), "{install}");
        assert_eq!(run, "java -Xss4m -jar \"%USERPROFILE%\\.devcat\\jdbc\\x.jar\"");
        let (_, run) = remote_commands(Shell::Pwsh, "x.jar", Some("C:\\Java\\bin\\java.exe"));
        assert_eq!(run, "& 'C:\\Java\\bin\\java.exe' -Xss4m -jar \"$env:USERPROFILE\\.devcat\\jdbc\\x.jar\"");
        let (_, run) = remote_commands(Shell::Pwsh, "x.jar", Some("C:\\J$x\\java.exe"));
        assert!(run.starts_with("& 'C:\\J$x\\java.exe'"), "no $ expansion: {run}");
        let (_, run) = remote_commands(Shell::Posix, "x.jar", Some("~/jdk/bin/java"));
        assert!(run.contains("exec \"$HOME\"/'\\''jdk/bin/java'\\'' -Xss4m"), "~ is the host's home: {run}");
        // the encoded script is UTF-16LE base64 of what PowerShell runs
        use base64::Engine;
        let b64 = install.rsplit(' ').next().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let script = String::from_utf16(&bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap();
        assert!(script.contains("'x.jar'") && script.contains("OpenStandardInput"), "{script}");
    }

    #[test]
    fn finds_vendor_jdks_newest_first() {
        let base = std::env::temp_dir().join(format!("devcat-jdks-{}", uuid::Uuid::new_v4()));
        for d in ["Eclipse Adoptium/jdk-17.0.9", "Eclipse Adoptium/jdk-21.0.5", "Zulu/zulu-11"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let found = vendor_javas(&base, "java.exe");
        let rel: Vec<String> = found.iter().map(|p| p.strip_prefix(&base).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(rel, ["Eclipse Adoptium/jdk-21.0.5/bin/java.exe", "Eclipse Adoptium/jdk-17.0.9/bin/java.exe", "Zulu/zulu-11/bin/java.exe"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Read-only over JDBC can't trust any one database's lexing: what's ambiguous is refused.
    #[test]
    fn jdbc_read_only_is_strict() {
        for ok in [
            "SELECT * FROM t",
            "select a, 'it''s; delete' AS x from t where b = \"Update\";",
            "WITH c AS (SELECT 1 AS n) SELECT n FROM c",
            "SELECT COUNT(*) FROM SYSADM.SYSTABLES",
            "EXPLAIN SELECT 1",
        ] {
            assert!(read_only_sql(ok), "{ok}");
        }
        for bad in [
            // the review's bypasses: dollar quoting, a hidden COMMIT, T-SQL without `;`
            "SELECT $$'$$; DELETE FROM t; COMMIT; SELECT ''",
            "SELECT $$'$$; DROP TABLE t; SELECT ''",
            "SELECT 1 DELETE FROM t COMMIT",
            "BEGIN DELETE FROM t END",
            // backslash escapes (MySQL): '\'' is one string there, two here
            "SELECT '\\'' ; DELETE FROM t; ''",
            // `--x` isn't a comment in MySQL
            "SELECT 1 --x; DELETE FROM t",
            "SELECT 1; SELECT 2",
            "SELECT `x'` ; DELETE FROM t; SELECT ''",
            "SELECT [x'] DELETE FROM t SELECT ''",
            "SELECT * INTO new_t FROM t",
            "SELECT 1 FROM t FOR UPDATE",
            "CALL p()",
            "SET SCHEMA x",
            "UPDATE t SET a = 1",
            "SELECT 'open",
            "",
        ] {
            assert!(!read_only_sql(bad), "{bad}");
        }
        // what agents and a read-only phone are checked with
        assert!(crate::remote::readonly::allowed(Some(ConnectionKind::Jdbc), "SELECT 1; SELECT 2"));
        assert!(!crate::remote::readonly::allowed(Some(ConnectionKind::Jdbc), "SELECT $$'$$; DELETE FROM t; COMMIT; SELECT ''"));
    }

    /// Jars are pinned when the connection is saved; a changed or added one isn't run.
    #[test]
    fn changed_jars_are_refused() {
        let dir = std::env::temp_dir().join(format!("devcat-jars-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("driver.jar"), b"one").unwrap();
        let mut c = cfg(json!({ "options": { "driverJars": dir.display().to_string() } }));
        pin_jars(&mut c).unwrap();
        let s = Settings::of(&c).unwrap();
        assert_eq!(pinned_jars(&c, &s).unwrap(), vec![dir.join("driver.jar").display().to_string()]);
        std::fs::write(dir.join("driver.jar"), b"two").unwrap();
        assert!(pinned_jars(&c, &s).unwrap_err().to_string().contains("changed since this connection was saved"));
        std::fs::write(dir.join("driver.jar"), b"one").unwrap();
        std::fs::write(dir.join("extra.jar"), b"evil").unwrap();
        assert!(pinned_jars(&c, &s).unwrap_err().to_string().contains("extra.jar"), "an added jar in the folder");
        // saving again trusts what's there now; unsaved (no pins) uses what it names
        pin_jars(&mut c).unwrap();
        assert_eq!(pinned_jars(&c, &s).unwrap().len(), 2);
        c.options.remove("jarHashes");
        assert_eq!(pinned_jars(&c, &s).unwrap().len(), 2);
        // over SSH the jars are the host's: nothing to pin here
        c.ssh = Some(crate::models::SshConfig { enabled: true, host: "h".into(), ..Default::default() });
        pin_jars(&mut c).unwrap();
        assert!(!c.options.contains_key("jarHashes"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn helper_is_embedded() {
        assert!(HELPER_JAR.starts_with(b"PK"), "a jar is a zip");
        assert!(helper_name().starts_with("devcat-jdbc-") && helper_name().ends_with(".jar"));
    }

    #[test]
    fn friendly_exit_reasons() {
        assert!(explain_exit("db1", "'java' is not recognized as an internal or external command").contains("Java wasn't found on db1"));
        assert!(explain_exit("db1", "sh: java: command not found").contains("Java wasn't found"));
        assert!(explain_exit("this computer", "java.lang.UnsupportedClassVersionError: DevcatJdbc").contains("too old"));
        assert_eq!(explain_exit("x", ""), "JDBC: the helper on x stopped");
    }

    /// Java and a JDBC driver jar to test with: `DEVCAT_TEST_JAVA` (a java program or a Java
    /// home) and `DEVCAT_TEST_H2_JAR` (the H2 database jar, which needs no server). Skipped
    /// (passes) without them.
    fn h2() -> Option<ConnectionConfig> {
        let java = std::env::var("DEVCAT_TEST_JAVA").ok()?;
        let jar = std::env::var("DEVCAT_TEST_H2_JAR").ok()?;
        let dir = std::env::temp_dir().join(format!("devcat-jdbc-test-{}", uuid::Uuid::new_v4()));
        init(dir.clone());
        Some(cfg(json!({
            "url": format!("jdbc:h2:{}/db;MODE=Regular", dir.join("h2").display()),
            "user": "sa",
            "options": { "driverJars": jar, "javaPath": java },
        })))
    }

    fn opts() -> ExecOptions {
        ExecOptions { database: None, schema: None, max_rows: Some(100) }
    }

    #[tokio::test]
    async fn h2_roundtrip() {
        let Some(cfg) = h2() else {
            eprintln!("skipping: set DEVCAT_TEST_JAVA and DEVCAT_TEST_H2_JAR");
            return;
        };
        let d = JdbcDriver::connect(&cfg).await.unwrap();
        assert!(d.server_version().await.unwrap().starts_with("H2 "));
        let r = d
            .execute(
                "CREATE SCHEMA app; CREATE TABLE app.authors(id INT PRIMARY KEY, name VARCHAR(40) NOT NULL);
                 CREATE TABLE app.books(id INT PRIMARY KEY, author_id INT REFERENCES app.authors(id), title VARCHAR(100), price DECIMAL(8,2), cover VARBINARY(8));
                 CREATE INDEX books_title ON app.books(title);
                 INSERT INTO app.authors VALUES (1, 'Ann'), (2, 'Bo');
                 INSERT INTO app.books VALUES (1, 1, 'One; with semicolon', 9.50, X'00ff'), (2, 1, 'Two', NULL, NULL), (3, 2, 'Three', 1, NULL);
                 SELECT * FROM app.books ORDER BY id",
                &opts(),
            )
            .await
            .unwrap();
        let last = r.last().unwrap();
        assert_eq!(last.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["ID", "AUTHOR_ID", "TITLE", "PRICE", "COVER"]);
        assert_eq!(last.rows[0], vec![json!(1), json!(1), json!("One; with semicolon"), json!("9.50"), json!("\\x00ff")]);
        assert_eq!(r.iter().filter_map(|r| r.rows_affected).collect::<Vec<_>>(), vec![0, 0, 0, 0, 2, 3]);

        // an error names the statement
        let e = d.execute("SELECT 1; SELECT * FROM nope", &opts()).await.unwrap_err().to_string();
        assert!(e.contains("statement 2 of 2") && e.contains("NOPE"), "{e}");

        // the tree: catalog → schema → folders → tables → columns
        let cats = d.children(&[]).await.unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].kind, NodeKind::Database);
        let cat = cats[0].label.clone();
        let schemas = d.children(&cats[0].path).await.unwrap();
        let app = schemas.iter().find(|s| s.label == "APP").expect("schema APP");
        let folders = d.children(&app.path).await.unwrap();
        let tables_folder = folders.iter().find(|f| f.label == "Tables").expect("Tables folder");
        let tables = d.children(&tables_folder.path).await.unwrap();
        assert_eq!(tables.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), ["AUTHORS", "BOOKS"]);
        let books = tables[1].table.clone().unwrap();
        assert_eq!(books, TableRef { database: Some(cat.clone()), schema: Some("APP".into()), name: "BOOKS".into() });
        let parts = d.children(&tables[1].path).await.unwrap();
        let cols = d.children(&parts[0].path).await.unwrap();
        assert_eq!(cols[0].detail.as_deref(), Some("INTEGER PK NOT NULL"));
        let fks = d.children(&parts[2].path).await.unwrap();
        assert_eq!(fks.len(), 1);
        assert!(fks[0].detail.as_deref().unwrap().contains("APP.AUTHORS(ID)"), "{:?}", fks[0].detail);

        // structure
        let s = d.table_structure(&books).await.unwrap();
        assert_eq!(s.columns.iter().map(|c| c.data_type.as_str()).collect::<Vec<_>>(), ["INTEGER", "INTEGER", "CHARACTER VARYING(100)", "DECIMAL(8,2)", "BINARY VARYING(8)"]);
        assert!(s.columns[0].is_primary_key && !s.columns[0].nullable);
        assert!(s.indexes.iter().any(|i| i.primary && i.columns == ["ID"]));
        assert!(s.indexes.iter().any(|i| !i.primary && i.columns == ["TITLE"]));
        assert_eq!(s.foreign_keys[0].ref_table, "AUTHORS");
        let ddl = d.object_ddl(&books, &NodeKind::Table).await.unwrap();
        assert!(ddl.contains("CREATE TABLE \"APP\".\"BOOKS\"") && ddl.contains("PRIMARY KEY (\"ID\")") && ddl.contains("REFERENCES \"APP\".\"AUTHORS\""), "{ddl}");

        // paging, sorting, filters
        let page = |offset, limit, filter: Option<&str>, desc| TableDataRequest {
            table: books.clone(),
            offset,
            limit,
            filter: filter.map(String::from),
            sort: if desc { vec![super::super::SortSpec { column: "ID".into(), desc: true }] } else { vec![] },
        };
        let p = d.table_data(&page(1, 1, None, false)).await.unwrap();
        assert_eq!((p.total, p.primary_key.clone(), p.result.rows.len()), (Some(3), vec!["ID".to_string()], 1));
        assert_eq!(p.result.rows[0][0], json!(2));
        assert!(p.result.truncated, "more rows after this page");
        let p = d.table_data(&page(0, 10, None, true)).await.unwrap();
        assert_eq!(p.result.rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), [json!(3), json!(2), json!(1)]);
        let p = d.table_data(&page(0, 10, Some("AUTHOR_ID = 1"), false)).await.unwrap();
        assert_eq!(p.total, Some(2));
        assert!(d.table_data(&page(0, 10, Some("1=1; DELETE FROM app.books"), false)).await.is_err());

        // grid edits: one transaction, all or nothing
        let key = |id: i64| json!({ "ID": id }).as_object().unwrap().clone();
        let vals = |v: Value| v.as_object().unwrap().clone();
        let n = d
            .apply_changes(
                &books,
                &[
                    RowChange::Update { key: key(2), values: vals(json!({ "TITLE": "Two (2nd ed.)", "PRICE": 12.25 })) },
                    RowChange::Insert { values: vals(json!({ "ID": 4, "AUTHOR_ID": 2, "TITLE": "Four", "COVER": "\\x0102" })) },
                    RowChange::Delete { key: key(3) },
                ],
            )
            .await
            .unwrap();
        assert_eq!(n, 3);
        let rows = d.execute("SELECT ID, TITLE, PRICE, COVER FROM app.books ORDER BY ID", &opts()).await.unwrap().remove(0).rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec![json!(2), json!("Two (2nd ed.)"), json!("12.25"), Value::Null]);
        assert_eq!(rows[2], vec![json!(4), json!("Four"), Value::Null, json!("\\x0102")]);
        let bad = d.apply_changes(&books, &[RowChange::Delete { key: key(1) }, RowChange::Update { key: key(99), values: vals(json!({ "TITLE": "x" })) }]).await;
        assert!(bad.unwrap_err().to_string().contains("affected 0 rows"));
        assert_eq!(d.execute("SELECT COUNT(*) FROM app.books", &opts()).await.unwrap()[0].rows[0][0], json!(3), "rolled back");

        // the schema snapshot (ER diagram, agents)
        let snap = d.schema_snapshot(None, Some("APP")).await.unwrap();
        assert_eq!(snap.tables.iter().map(|t| t.table.as_ref().unwrap().name.as_str()).collect::<Vec<_>>(), ["AUTHORS", "BOOKS"]);
        assert_eq!(snap.tables[1].foreign_keys.len(), 1);
        d.close().await;

        // a read-only session: refused by its words, and undone if the words lie
        let mut ro = cfg.clone();
        ro.read_only = true;
        let r = JdbcOne::connect(&ro).await.unwrap();
        assert!(r.execute("DELETE FROM app.books", &opts()).await.is_err());
        assert!(r.apply_changes(&books, &[RowChange::Delete { key: key(1) }]).await.is_err());
        assert_eq!(r.execute("SELECT COUNT(*) FROM app.books", &opts()).await.unwrap()[0].rows[0][0], json!(3));
        // past the word check (a function that writes would do the same): rolled back after it ran
        let sneaky = r.exec_one("DELETE FROM app.books", None, None, 10, 0).await;
        let _ = sneaky;
        assert_eq!(r.execute("SELECT COUNT(*) FROM app.books", &opts()).await.unwrap()[0].rows[0][0], json!(3), "the helper rolled it back");
        r.close().await;
    }

    /// The helper on "another computer" over SSH: a throwaway sshd on 127.0.0.1 that lets this
    /// user in with a key. Env: `DEVCAT_TEST_JDBC_SSH_PORT`, `DEVCAT_TEST_JDBC_SSH_KEY`, plus the
    /// two above (`DEVCAT_TEST_JAVA` must then be the java program itself, as the host sees it).
    /// Copies the helper to `~/.devcat/jdbc/` of this user, as it would on a real host.
    #[tokio::test]
    async fn h2_over_ssh() {
        let (Some(mut cfg), Ok(port), Ok(key)) = (h2(), std::env::var("DEVCAT_TEST_JDBC_SSH_PORT"), std::env::var("DEVCAT_TEST_JDBC_SSH_KEY")) else {
            eprintln!("skipping: set DEVCAT_TEST_JDBC_SSH_PORT and DEVCAT_TEST_JDBC_SSH_KEY");
            return;
        };
        // host keys go to a test file, not the user's DevCat data folder
        crate::db::ssh::init(std::env::temp_dir().join(format!("devcat-jdbc-ssh-{}", uuid::Uuid::new_v4())));
        cfg.ssh = Some(crate::models::SshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: port.parse().unwrap(),
            user: std::env::var("USER").unwrap(),
            auth: "key".into(),
            key_path: Some(key),
            password: None,
        });
        let d = JdbcDriver::connect(&cfg).await.unwrap();
        assert!(d.server_version().await.unwrap().starts_with("H2 "));
        let r = d.execute("CREATE TABLE t(id INT PRIMARY KEY, s VARCHAR(10)); INSERT INTO t VALUES (1, 'über'); SELECT * FROM t", &opts()).await.unwrap();
        assert_eq!(r.last().unwrap().rows, vec![vec![json!(1), json!("über")]]);
        let cancel_ok = d.cancel().await;
        assert!(cancel_ok.is_ok(), "{cancel_ok:?}");
        d.close().await;
        // a second session finds the helper there already
        let d = JdbcDriver::connect(&cfg).await.unwrap();
        assert_eq!(d.execute("SELECT s FROM t", &opts()).await.unwrap()[0].rows[0][0], json!("über"));
        d.close().await;
        // no Java there
        let mut bad = cfg.clone();
        bad.options.insert("javaPath".into(), "/nonexistent/java".into());
        let e = JdbcDriver::connect(&bad).await.err().unwrap().to_string();
        assert!(e.contains("Java wasn't found on 127.0.0.1"), "{e}");
    }

    #[tokio::test]
    async fn h2_cancel_and_errors() {
        let Some(cfg) = h2() else { return };
        let d = Arc::new(JdbcOne::connect(&cfg).await.unwrap());
        // a slow query, cancelled from the side
        let run = {
            let d = d.clone();
            tokio::spawn(async move { d.execute("SELECT COUNT(*) FROM SYSTEM_RANGE(1, 1000000000) a, SYSTEM_RANGE(1, 1000) b", &opts()).await })
        };
        tokio::time::sleep(Duration::from_millis(800)).await;
        d.cancel().await.unwrap();
        let r = tokio::time::timeout(Duration::from_secs(20), run).await.expect("cancel stops it").unwrap();
        assert!(r.is_err(), "cancelled");
        // the session still works afterwards
        assert_eq!(d.execute("SELECT 1", &opts()).await.unwrap()[0].rows[0][0], json!(1));

        // a schema the session switched to itself (USE / SET SCHEMA) doesn't fool a later request
        d.execute("CREATE SCHEMA a; CREATE SCHEMA b; CREATE TABLE a.t(v VARCHAR(5)); CREATE TABLE b.t(v VARCHAR(5)); INSERT INTO a.t VALUES ('A'); INSERT INTO b.t VALUES ('B')", &opts()).await.unwrap();
        let in_a = ExecOptions { schema: Some("A".into()), ..opts() };
        assert_eq!(d.execute("SELECT v FROM t", &in_a).await.unwrap()[0].rows[0][0], json!("A"));
        d.execute("SET SCHEMA B", &opts()).await.unwrap();
        assert_eq!(d.execute("SELECT v FROM t", &in_a).await.unwrap()[0].rows[0][0], json!("A"), "back in A");

        // the helper dies: the next call starts another one
        let old = d.bridge.lock().clone();
        old.close().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(d.execute("SELECT v FROM a.t", &opts()).await.unwrap()[0].rows[0][0], json!("A"), "reconnected");
        assert!(!Arc::ptr_eq(&old, &d.bridge.lock().clone()));
        d.close().await;

        // a jar that isn't there, a URL no driver takes
        let mut bad = cfg.clone();
        bad.options.insert("driverJars".into(), "/nonexistent/driver.jar".into());
        let e = JdbcDriver::connect(&bad).await.err().unwrap().to_string();
        assert!(e.contains("driver jar not found"), "{e}");
        let mut bad = cfg.clone();
        bad.url = Some("jdbc:nobody://x".into());
        let e = JdbcDriver::connect(&bad).await.err().unwrap().to_string();
        assert!(e.contains("no driver in the jars accepts"), "{e}");
        let mut bad = cfg.clone();
        bad.options.insert("javaPath".into(), "/nonexistent/java".into());
        let e = JdbcDriver::connect(&bad).await.err().unwrap().to_string();
        assert!(e.contains("no Java at"), "{e}");
    }
}
