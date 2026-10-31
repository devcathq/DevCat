//! Docker connections: containers, images, volumes and networks of a Docker engine, through the
//! `docker` command line (`--format '{{json .}}'`). The CLI already knows the user's contexts and
//! remote engines (`ssh://`, `tcp://` with TLS), so a connection is a context, or a host that
//! overrides it. Logs stream from `docker logs -f`; a container's shell is `docker exec -it` in a
//! terminal tab.
use crate::claude::locate;
use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::state::St;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Lists and inspects.
const QUICK: Duration = Duration::from_secs(30);
/// Start / stop / restart / remove (stop waits up to 10 s for the container).
const SLOW: Duration = Duration::from_secs(90);

/// The `docker` program: on the PATH DevCat gives tools, else where Docker Desktop, OrbStack and
/// Rancher Desktop put it (a Mac app doesn't get the shell's PATH).
async fn docker_bin() -> AppResult<PathBuf> {
    static FOUND: tokio::sync::OnceCell<Option<PathBuf>> = tokio::sync::OnceCell::const_new();
    FOUND
        .get_or_init(|| async {
            if let Some(p) = locate::find_program("docker").await {
                return Some(p);
            }
            let home = dirs::home_dir().unwrap_or_default();
            let mut places = vec![home.join(".docker/bin/docker"), home.join(".orbstack/bin/docker"), home.join(".rd/bin/docker")];
            if cfg!(windows) {
                places = vec![PathBuf::from(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe"), home.join(r".rd\bin\docker.exe")];
            } else if cfg!(target_os = "macos") {
                places.push(PathBuf::from("/Applications/Docker.app/Contents/Resources/bin/docker"));
            }
            places.into_iter().find(|p| p.is_file())
        })
        .await
        .clone()
        .ok_or_else(|| AppError::msg("Docker's command line (`docker`) wasn't found. Install Docker Desktop (or the Docker CLI) and try again."))
}

/// Which engine a command talks to: a host (`-H`, overrides the context), else a context, else
/// the CLI's current context.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct Engine {
    pub host: String,
    pub context: String,
}

impl Engine {
    pub fn of(cfg: &ConnectionConfig) -> Engine {
        Engine { host: cfg.host.trim().to_string(), context: cfg.options.get("context").map(|c| c.trim().to_string()).unwrap_or_default() }
    }

    /// The global flags that pick it.
    fn args(&self) -> Vec<String> {
        if !self.host.is_empty() {
            vec!["-H".into(), self.host.clone()]
        } else if !self.context.is_empty() {
            vec!["--context".into(), self.context.clone()]
        } else {
            vec![]
        }
    }
}

async fn command(engine: &Engine, args: &[&str]) -> AppResult<tokio::process::Command> {
    let bin = docker_bin().await?;
    // the PATH tools get, and no console window on Windows
    let mut cmd = locate::tool_command(&bin.to_string_lossy()).await;
    cmd.args(engine.args()).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    Ok(cmd)
}

/// Run `docker args` and return its output, or what went wrong in words.
async fn run(engine: &Engine, args: &[&str], limit: Duration) -> AppResult<String> {
    let mut cmd = command(engine, args).await?;
    let out = tokio::time::timeout(limit, cmd.output())
        .await
        .map_err(|_| AppError::msg(format!("`docker {}` took longer than {} s", args.first().unwrap_or(&""), limit.as_secs())))??;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(AppError::msg(explain(&String::from_utf8_lossy(&out.stderr))))
}

/// The CLI's error, with a hint when the engine isn't reachable.
fn explain(stderr: &str) -> String {
    let msg = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    let msg = msg.strip_prefix("Error response from daemon: ").unwrap_or(&msg).to_string();
    let down = ["daemon is running", "failed to connect to the docker API", "Cannot connect to the Docker daemon", "The system cannot find the file specified"];
    if down.iter().any(|d| msg.contains(d)) {
        return format!("Docker isn't running, or this connection can't reach it. Start Docker Desktop (or the Docker service) and try again.\n\n{msg}");
    }
    if msg.starts_with("error during connect") {
        return format!("Can't reach the Docker engine. Check the connection's host or context, and that Docker is running there.\n\n{msg}");
    }
    if msg.is_empty() { "docker failed without saying why".into() } else { msg }
}

/// One JSON object per line (`--format '{{json .}}'`).
fn json_lines(out: &str) -> Vec<Value> {
    out.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect()
}

fn s(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `a=1,b=2` labels: the value of `key`.
fn label(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=').map(String::from)).filter(|v| !v.is_empty())
}

/// The engine of a saved connection (its VPN or tunnel brought up first, if it has one).
async fn config(state: &St, conn_id: &str) -> AppResult<Engine> {
    let cfg = state.store.get(conn_id)?;
    crate::network::ensure_for(&cfg).await?;
    Ok(Engine::of(&cfg))
}

/// The engine of a connection that may change things: a read-only one only looks.
async fn writable(state: &St, conn_id: &str) -> AppResult<Engine> {
    if state.store.get(conn_id)?.read_only {
        return Err(AppError::msg("This connection is read-only"));
    }
    config(state, conn_id).await
}

/// A container, image, volume or network to hand docker: never something it would take for a flag.
fn object(id: &str) -> AppResult<&str> {
    if id.is_empty() || id.starts_with('-') {
        return Err(AppError::msg(format!("Not a Docker object: {id:?}")));
    }
    Ok(id)
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub command: String,
    /// running, exited, paused, created, restarting, dead
    pub state: String,
    /// "Up 2 hours", "Exited (0) 3 days ago"
    pub status: String,
    pub ports: String,
    pub created: String,
    /// The Compose project and service it belongs to.
    pub project: Option<String>,
    pub service: Option<String>,
}

fn container(v: &Value) -> Container {
    let labels = s(v, "Labels");
    Container {
        id: s(v, "ID"),
        name: s(v, "Names"),
        image: s(v, "Image"),
        command: s(v, "Command").trim_matches('"').to_string(),
        state: s(v, "State"),
        status: s(v, "Status"),
        ports: s(v, "Ports"),
        created: s(v, "RunningFor"),
        project: label(&labels, "com.docker.compose.project"),
        service: label(&labels, "com.docker.compose.service"),
    }
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub id: String,
    pub cpu: String,
    pub memory: String,
    pub memory_percent: String,
    pub net: String,
    pub block: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: String,
    pub created: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub mountpoint: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub name: String,
    pub endpoint: String,
    pub current: bool,
}

/// The engine, for "Test connection".
pub async fn test(cfg: &ConnectionConfig) -> AppResult<String> {
    let v: Value = serde_json::from_str(&run(&Engine::of(cfg), &["version", "--format", "{{json .}}"], QUICK).await?).unwrap_or_default();
    let server = &v["Server"];
    if server.is_null() {
        return Err(AppError::msg("Docker isn't running, or this connection can't reach it."));
    }
    Ok(format!("Docker {} ({}/{})", s(server, "Version"), s(server, "Os"), s(server, "Arch")))
}

/// The contexts in the user's Docker config, for the connection form.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_contexts() -> AppResult<Vec<Context>> {
    let out = run(&Engine::default(), &["context", "ls", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out)
        .iter()
        .map(|v| Context { name: s(v, "Name"), endpoint: s(v, "DockerEndpoint"), current: v["Current"].as_bool().unwrap_or(false) })
        .collect())
}

/// The engine in a few words: version, OS, CPUs, memory, how many containers and images.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_info(state: St, conn_id: String) -> AppResult<Value> {
    let engine = config(&state, &conn_id).await?;
    let v: Value = serde_json::from_str(&run(&engine, &["info", "--format", "{{json .}}"], QUICK).await?).map_err(|e| AppError::msg(e.to_string()))?;
    if let Some(e) = v["ServerErrors"].as_array().and_then(|a| a.first()).and_then(Value::as_str) {
        return Err(AppError::msg(explain(e)));
    }
    Ok(serde_json::json!({
        "version": s(&v, "ServerVersion"),
        "os": s(&v, "OperatingSystem"),
        "cpus": v["NCPU"],
        "memory": v["MemTotal"],
        "containers": v["Containers"],
        "running": v["ContainersRunning"],
        "images": v["Images"],
        "name": s(&v, "Name"),
    }))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_containers(state: St, conn_id: String) -> AppResult<Vec<Container>> {
    let engine = config(&state, &conn_id).await?;
    // `{{json .}}` names .Size, which on its own makes docker work out every container's disk use
    let out = run(&engine, &["ps", "--all", "--no-trunc", "--size=false", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out).iter().map(container).collect())
}

/// CPU and memory of the running containers (one sample).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_stats(state: St, conn_id: String) -> AppResult<Vec<Stats>> {
    let engine = config(&state, &conn_id).await?;
    let out = run(&engine, &["stats", "--no-stream", "--no-trunc", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out)
        .iter()
        .map(|v| Stats { id: s(v, "ID"), cpu: s(v, "CPUPerc"), memory: s(v, "MemUsage"), memory_percent: s(v, "MemPerc"), net: s(v, "NetIO"), block: s(v, "BlockIO") })
        .collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_images(state: St, conn_id: String) -> AppResult<Vec<Image>> {
    let engine = config(&state, &conn_id).await?;
    let out = run(&engine, &["image", "ls", "--no-trunc", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out)
        .iter()
        .map(|v| Image { id: s(v, "ID"), repository: s(v, "Repository"), tag: s(v, "Tag"), size: s(v, "Size"), created: s(v, "CreatedSince") })
        .collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_volumes(state: St, conn_id: String) -> AppResult<Vec<Volume>> {
    let engine = config(&state, &conn_id).await?;
    let out = run(&engine, &["volume", "ls", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out)
        .iter()
        .map(|v| Volume { name: s(v, "Name"), driver: s(v, "Driver"), scope: s(v, "Scope"), mountpoint: s(v, "Mountpoint") })
        .collect())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_networks(state: St, conn_id: String) -> AppResult<Vec<Network>> {
    let engine = config(&state, &conn_id).await?;
    let out = run(&engine, &["network", "ls", "--no-trunc", "--format", "{{json .}}"], QUICK).await?;
    Ok(json_lines(&out).iter().map(|v| Network { id: s(v, "ID"), name: s(v, "Name"), driver: s(v, "Driver"), scope: s(v, "Scope") }).collect())
}

/// Start, stop, restart, pause, unpause, kill or remove a container (remove forces a running one).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_container_action(state: St, conn_id: String, id: String, action: String) -> AppResult<()> {
    let id = object(&id)?;
    let engine = writable(&state, &conn_id).await?;
    let args: Vec<&str> = match action.as_str() {
        "start" | "stop" | "restart" | "pause" | "unpause" | "kill" => vec![action.as_str(), id],
        "remove" => vec!["rm", "--force", id],
        other => return Err(AppError::msg(format!("Unknown container action: {other}"))),
    };
    run(&engine, &args, SLOW).await.map(|_| ())
}

/// A container's last `tail` log lines at once (for agents; the app streams them instead): what
/// it wrote to stdout, then what it wrote to stderr. `since`: e.g. "10m" or an RFC 3339 time.
pub async fn docker_logs_tail(state: St, conn_id: String, id: String, tail: u32, timestamps: bool, since: Option<String>) -> AppResult<String> {
    let engine = config(&state, &conn_id).await?;
    let tail = tail.to_string();
    let mut args = vec!["logs", "--tail", &tail];
    if timestamps {
        args.push("--timestamps");
    }
    let since = since.filter(|s| !s.trim().is_empty() && !s.starts_with('-'));
    if let Some(s) = &since {
        args.extend(["--since", s.as_str()]);
    }
    args.push(object(&id)?);
    let mut cmd = command(&engine, &args).await?;
    let out = tokio::time::timeout(SLOW, cmd.output()).await.map_err(|_| AppError::msg(format!("`docker logs` took longer than {} s", SLOW.as_secs())))??;
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(AppError::msg(explain(&stderr)));
    }
    Ok(match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (_, true) => stdout.into_owned(),
        (true, false) => stderr.into_owned(),
        (false, false) => format!("{stdout}\n--- stderr ---\n{stderr}"),
    })
}

/// Remove an image, volume or network (`force` for an image still tagged or used by a stopped container).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_remove(state: St, conn_id: String, kind: String, id: String, force: Option<bool>) -> AppResult<()> {
    let id = object(&id)?;
    let engine = writable(&state, &conn_id).await?;
    let mut args: Vec<&str> = match kind.as_str() {
        "image" => vec!["image", "rm"],
        "volume" => vec!["volume", "rm"],
        "network" => vec!["network", "rm"],
        other => return Err(AppError::msg(format!("Can't remove a {other}"))),
    };
    if force == Some(true) && kind == "image" {
        args.push("--force");
    }
    args.push(id);
    run(&engine, &args, SLOW).await.map(|_| ())
}

/// `docker inspect` of a container, image, volume or network, as JSON.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_inspect(state: St, conn_id: String, kind: String, id: String) -> AppResult<Value> {
    if !["container", "image", "volume", "network"].contains(&kind.as_str()) {
        return Err(AppError::msg(format!("Can't inspect a {kind}")));
    }
    let engine = config(&state, &conn_id).await?;
    let out = run(&engine, &[kind.as_str(), "inspect", object(&id)?], QUICK).await?;
    let v: Value = serde_json::from_str(&out).map_err(|e| AppError::msg(e.to_string()))?;
    Ok(v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v))
}

/// What a terminal tab runs for a shell in a container: `docker exec -it` with bash if it has
/// it, else sh.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_shell_command(state: St, conn_id: String, id: String) -> AppResult<ShellCommand> {
    let id = object(&id)?;
    // a shell can change anything in the container
    let engine = writable(&state, &conn_id).await?;
    let mut args = engine.args();
    args.extend(["exec", "-it", id, "sh", "-c", "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi"].map(String::from));
    Ok(ShellCommand { program: docker_bin().await?.to_string_lossy().into_owned(), args })
}

/// Log streams by id; dropping one stops `docker logs -f`.
static LOGS: Lazy<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> = Lazy::new(Default::default);

/// Follow a container's logs (the last `tail` lines first). Lines come as `docker-log` events
/// ({ streamId, lines, done, error }), a few at a time.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_logs_start(state: St, conn_id: String, id: String, stream_id: String, tail: Option<u32>, timestamps: Option<bool>) -> AppResult<()> {
    let engine = config(&state, &conn_id).await?;
    let tail = tail.unwrap_or(500).to_string();
    let mut args = vec!["logs", "--follow", "--tail", &tail];
    if timestamps == Some(true) {
        args.push("--timestamps");
    }
    args.push(object(&id)?);
    let mut child = command(&engine, &args).await?.spawn().map_err(|e| AppError::msg(format!("Couldn't start docker logs: {e}")))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let sid = stream_id.clone();
    let task = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        // the program writes to both: stdout and stderr lines, as they come
        for pipe in [out.map(|o| Box::new(o) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), err.map(|e| Box::new(e) as _)].into_iter().flatten() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        // a batch goes out 120 ms after its first line (or at 500 lines), however fast they come
        let mut batch = Vec::new();
        let mut due = tokio::time::Instant::now();
        loop {
            tokio::select! {
                line = rx.recv() => match line {
                    Some(l) => {
                        if batch.is_empty() {
                            due = tokio::time::Instant::now() + Duration::from_millis(120);
                        }
                        batch.push(l);
                        if batch.len() < 500 {
                            continue;
                        }
                    }
                    None => break,
                },
                _ = tokio::time::sleep_until(due), if !batch.is_empty() => {}
            }
            crate::events::emit("docker-log", serde_json::json!({ "streamId": sid, "lines": std::mem::take(&mut batch), "done": false }));
        }
        let error = match child.wait().await {
            Ok(s) if !s.success() => batch.last().map(|l| explain(l)),
            _ => None,
        };
        crate::events::emit("docker-log", serde_json::json!({ "streamId": sid, "lines": batch, "done": true, "error": error }));
        LOGS.lock().remove(&sid);
    });
    if let Some(old) = LOGS.lock().insert(stream_id, task) {
        old.abort();
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn docker_logs_stop(stream_id: String) -> AppResult<()> {
    if let Some(t) = LOGS.lock().remove(&stream_id) {
        t.abort();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_from_the_cli() {
        let out = r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2026-10-23 09:00:00 +0700 WIB","ID":"4f2a9c","Image":"postgres:16","Labels":"com.docker.compose.project=shop,com.docker.compose.project.config_files=a.yml,b.yml,com.docker.compose.service=db","Names":"shop-db-1","Ports":"0.0.0.0:5432->5432/tcp","RunningFor":"2 hours ago","State":"running","Status":"Up 2 hours"}
not json
{"ID":"9b1","Image":"redis","Labels":"","Names":"cache","State":"exited","Status":"Exited (0) 3 days ago"}"#;
        let list: Vec<Container> = json_lines(out).iter().map(container).collect();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].project.as_deref(), Some("shop"));
        assert_eq!(list[0].service.as_deref(), Some("db"));
        assert_eq!((list[0].name.as_str(), list[0].state.as_str(), list[0].ports.as_str()), ("shop-db-1", "running", "0.0.0.0:5432->5432/tcp"));
        assert_eq!(list[0].command, "docker-entrypoint.s…");
        assert_eq!((list[1].project.clone(), list[1].status.as_str()), (None, "Exited (0) 3 days ago"));
    }

    #[test]
    fn the_engine_is_the_host_else_the_context() {
        let mut e = Engine::default();
        assert!(e.args().is_empty());
        e.context = "desktop-linux".into();
        assert_eq!(e.args(), ["--context", "desktop-linux"]);
        e.host = "ssh://me@server".into();
        assert_eq!(e.args(), ["-H", "ssh://me@server"]);
    }

    #[test]
    fn errors_say_when_docker_is_down() {
        let e = explain("failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine; check if the path is correct and if the daemon is running");
        assert!(e.starts_with("Docker isn't running"), "{e}");
        let e = explain("error during connect: Get \"http://127.0.0.1:2/v1.55/info\": dial tcp 127.0.0.1:2: connectex: No connection could be made because the target machine actively refused it.");
        assert!(e.starts_with("Can't reach the Docker engine"), "{e}");
        assert_eq!(explain("Error response from daemon: No such container: x\n"), "No such container: x");
        assert!(object("--volumes").is_err() && object("").is_err());
        assert_eq!(object("redis:7-alpine").unwrap(), "redis:7-alpine");
        assert_eq!(label("a=1,b=2", "b"), Some("2".into()));
        assert_eq!(label("ab=1", "a"), None);
    }
}
