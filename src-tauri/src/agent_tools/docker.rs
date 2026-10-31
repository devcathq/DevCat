//! Docker tools over the user's DevCat Docker connections (an engine: a context, or a host that
//! overrides it): list containers, images, volumes and networks with their state, inspect one,
//! read a container's logs, start / stop / restart containers, and remove things. Through the
//! `docker` command line, like the app. Secret-looking environment variables are hidden from
//! `inspect`. Changes need the connection's AI access at Full over MCP; the DevCat Agent asks
//! first. Removing can't be undone, so `docker_remove` is the DevCat Agent's only, confirmed by
//! typing the name (`Tool::confirm`).
use super::*;
use crate::infra::docker as dk;

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>) {
    out.push(Arc::new(DockerList(s.clone())));
    out.push(Arc::new(DockerInspect(s.clone())));
    out.push(Arc::new(DockerLogs(s.clone())));
    out.push(Arc::new(DockerAction(s.clone())));
    if !s.mcp {
        out.push(Arc::new(DockerRemove(s.clone())));
    }
}

fn docker_conn(scope: &Scope, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    conn_of(scope, input, ConnectionKind::Docker)
}

/// One JSON object per line: compact for the model, every field kept.
fn lines<T: serde::Serialize>(items: &[T], what: &str) -> String {
    if items.is_empty() {
        return format!("No {what}.");
    }
    let mut out = vec![format!("{} {what}:", items.len())];
    out.extend(items.iter().map(|i| serde_json::to_string(i).unwrap_or_default()));
    out.join("\n")
}

// ---------- docker_list

struct DockerList(Arc<Scope>);

#[async_trait]
impl Tool for DockerList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docker_list".into(),
            description: "List what a DevCat Docker connection's engine has: `what` = containers (default: every container with its image, state, status, ports and Compose project / service), stats (CPU, memory, network and disk use of running containers), images, volumes, networks, or info (the engine's version and resources).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Docker connection name or id" },
                    "what": { "type": "string", "enum": ["containers", "stats", "images", "volumes", "networks", "info"] }
                },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {}", conn_summary(input), arg(input, "what").unwrap_or_else(|| "containers".into()))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(100)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match docker_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let (st, id) = (St::get(), c.id.clone());
        let res = match arg(&input, "what").as_deref().unwrap_or("containers") {
            "containers" => dk::docker_containers(st, id).await.map(|v| lines(&v, "containers")),
            "stats" => dk::docker_stats(st, id).await.map(|v| lines(&v, "running containers")),
            "images" => dk::docker_images(st, id).await.map(|v| lines(&v, "images")),
            "volumes" => dk::docker_volumes(st, id).await.map(|v| lines(&v, "volumes")),
            "networks" => dk::docker_networks(st, id).await.map(|v| lines(&v, "networks")),
            "info" => dk::docker_info(st, id).await.map(|v| pretty(&v)),
            other => return ToolOutput::err(format!("Unknown `what` \"{other}\": containers, stats, images, volumes, networks or info.")),
        };
        match res {
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- docker_inspect

struct DockerInspect(Arc<Scope>);

#[async_trait]
impl Tool for DockerInspect {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docker_inspect".into(),
            description: "Inspect one container, image, volume or network of a DevCat Docker connection (`docker inspect` as JSON: config, mounts, networks, health, restart policy...). Values of secret-looking environment variables (PASSWORD, TOKEN, SECRET, KEY...) are hidden.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Docker connection name or id" },
                    "kind": { "type": "string", "enum": ["container", "image", "volume", "network"], "description": "Default container" },
                    "id": { "type": "string", "description": "Name or id" }
                },
                "required": ["connection", "id"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {} {}", conn_summary(input), arg(input, "kind").unwrap_or_else(|| "container".into()), arg(input, "id").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match docker_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(id) = arg(&input, "id") else { return ToolOutput::err("Missing `id`: the container's (image's, ...) name or id.") };
        match dk::docker_inspect(St::get(), c.id.clone(), arg(&input, "kind").unwrap_or_else(|| "container".into()), id).await {
            Ok(mut v) => {
                redact_secrets(&mut v);
                ToolOutput::ok(pretty(&v))
            }
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- docker_logs

struct DockerLogs(Arc<Scope>);

#[async_trait]
impl Tool for DockerLogs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docker_logs".into(),
            description: "Read a container's recent logs from a DevCat Docker connection: the last `tail` lines (default 200), optionally only `since` a time (\"10m\", \"2h\", an RFC 3339 time) and with timestamps.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Docker connection name or id" },
                    "container": { "type": "string", "description": "Container name or id" },
                    "tail": { "type": "integer" },
                    "since": { "type": "string" },
                    "timestamps": { "type": "boolean" }
                },
                "required": ["connection", "container"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {}", conn_summary(input), arg(input, "container").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(100)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match docker_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(id) = arg(&input, "container") else { return ToolOutput::err("Missing `container`.") };
        let tail = input.get("tail").and_then(Value::as_u64).unwrap_or(200).clamp(1, 5000) as u32;
        match dk::docker_logs_tail(St::get(), c.id.clone(), id, tail, arg_bool(&input, "timestamps").unwrap_or(false), arg(&input, "since")).await {
            Ok(text) if text.trim().is_empty() => ToolOutput::ok("(no log lines)"),
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- docker_action

struct DockerAction(Arc<Scope>);

#[async_trait]
impl Tool for DockerAction {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docker_action".into(),
            description: "Start, stop, restart, pause, unpause or kill a container on a DevCat Docker connection's engine. Removing a container, image, volume or network is `docker_remove`, in the DevCat Agent only.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Docker connection name or id" },
                    "action": { "type": "string", "enum": ["start", "stop", "restart", "pause", "unpause", "kill"] },
                    "container": { "type": "string", "description": "Name or id" }
                },
                "required": ["connection", "action", "container"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {} {}", conn_summary(input), arg(input, "action").unwrap_or_default(), arg(input, "container").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(100)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match docker_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let (Some(action), Some(id)) = (arg(&input, "action"), arg(&input, "container")) else { return ToolOutput::err("Missing `action` or `container`.") };
        if !["start", "stop", "restart", "pause", "unpause", "kill"].contains(&action.as_str()) {
            return ToolOutput::err(format!("Unknown `action` {action}: start, stop, restart, pause, unpause or kill (removing is `docker_remove`, DevCat Agent only)."));
        }
        match dk::docker_container_action(St::get(), c.id.clone(), id.clone(), action.clone()).await {
            Ok(()) => ToolOutput::ok(format!("Done: {action} {id}.")),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- docker_remove

struct DockerRemove(Arc<Scope>);

#[async_trait]
impl Tool for DockerRemove {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docker_remove".into(),
            description: "Remove a container (stopped or not), an image, a volume (with its data) or a network on a DevCat Docker connection's engine (`force` for an image still in use). It can't be undone: only when the user asked to remove that one, say which first. DevCat asks the user to confirm by typing its name or id.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Docker connection name or id" },
                    "kind": { "type": "string", "enum": ["container", "image", "volume", "network"], "description": "Default container" },
                    "id": { "type": "string", "description": "Name or id" },
                    "force": { "type": "boolean" }
                },
                "required": ["connection", "id"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: remove {} {}", conn_summary(input), arg(input, "kind").unwrap_or_else(|| "container".into()), arg(input, "id").unwrap_or_default())
    }
    fn confirm(&self, input: &Value) -> Option<String> {
        Some(arg(input, "id").unwrap_or_else(|| "(no id)".into()))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(100)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        if self.0.mcp {
            return ToolOutput::err("Docker objects are removed from the DevCat Agent in DevCat only, where the user confirms it.");
        }
        let c = match docker_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(id) = arg(&input, "id") else { return ToolOutput::err("Missing `id`.") };
        let kind = arg(&input, "kind").unwrap_or_else(|| "container".into());
        let res = match kind.as_str() {
            "container" => dk::docker_container_action(St::get(), c.id.clone(), id.clone(), "remove".into()).await,
            _ => dk::docker_remove(St::get(), c.id.clone(), kind.clone(), id.clone(), arg_bool(&input, "force")).await,
        };
        match res {
            Ok(()) => ToolOutput::ok(format!("Removed {kind} {id}.")),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_hides_secret_environment() {
        let mut v = json!([{ "Config": { "Env": ["PATH=/usr/bin", "POSTGRES_PASSWORD=hunter2", "API_TOKEN=abc"], "Labels": { "db.password": "x", "app": "web" } } }]);
        redact_secrets(&mut v);
        assert_eq!(v[0]["Config"]["Env"], json!(["PATH=/usr/bin", "POSTGRES_PASSWORD=<hidden>", "API_TOKEN=<hidden>"]));
        assert_eq!(v[0]["Config"]["Labels"], json!({ "db.password": "<hidden>", "app": "web" }));
    }
}
