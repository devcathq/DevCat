//! What a paired phone may call. This table is the allowlist: anything not listed here is
//! refused. Read-only phones get only the reads (and read-only queries). Secrets never go out:
//! `get_connection` (it returns passwords) and file writes aren't exposed at all.
use super::server::Client;
use crate::commands;
use crate::events::Owner;
use crate::state::St;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;

pub type CallResult = Result<Value, Value>;

fn arg<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, Value> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null)).map_err(|e| json!(format!("bad argument {name}: {e}")))
}

fn out<T: serde::Serialize>(r: crate::error::AppResult<T>) -> CallResult {
    r.map(|v| serde_json::to_value(v).unwrap_or(Value::Null)).map_err(|e| json!(e.to_string()))
}

/// Commands that change something (refused for read-only phones). `db_execute` is checked
/// statement by statement instead.
pub const WRITES: &[&str] = &[
    "db_apply_changes",
    "db_set_key",
    "db_delete_key",
    "net_connect",
    "net_disconnect",
    "k8s_delete",
    "k8s_scale",
    "k8s_restart",
    "k8s_cronjob_suspend",
    "pty_open",
    "pty_write",
    "pty_resize",
    "pty_close",
    "keep_awake_set",
    "close_connection",
    "notify_save",
    "notify_test",
    "fs_write_text",
    "fs_upload",
    "fs_upload_cancel",
    "paste_dir",
    "agent_send",
    "agent_attach",
    "agent_stop",
    "agent_approve",
    "agent_answer",
    "agent_close",
];

/// Images / PDFs bigger than this aren't sent to phones (base64 over a phone connection).
const PHONE_MEDIA_CAP: usize = 12 * 1024 * 1024;

/// Calls too frequent or too private for the audit log (keystrokes, live views).
pub const NOT_AUDITED: &[&str] = &["pty_write", "pty_resize", "pty_attach", "pty_detach", "net_traffic", "k8s_stream_stop", "agents", "home_dir", "agent_detach", "agent_models"];

/// A file transfer is many calls: log a download once (first piece) and an upload once (last piece).
pub fn audit_skip(cmd: &str, args: &Value) -> bool {
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    match cmd {
        "fs_download" => offset > 0,
        "fs_upload" => !args.get("done").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

pub async fn call(client: &Arc<Client>, cmd: &str, args: &Value) -> CallResult {
    if client.readonly() && WRITES.contains(&cmd) {
        return Err(json!(if crate::events::headless() {
            "This phone has read-only access. Change it on the computer: type `devices`, then `access <n> full` where `devcat serve` runs."
        } else {
            "This phone has read-only access. Change it on the computer: DevCat → Mobile → Paired phones."
        }));
    }
    let st = St::get;
    macro_rules! a {
        ($n:literal) => {
            arg(args, $n)?
        };
    }
    match cmd {
        // ------------------------------------------------ host / agents / terminals
        "host_info" => Ok(super::host_summary()),
        "audit" => Ok(json!(super::audit_for(&client.device.id, arg(args, "limit").unwrap_or(200)))),
        "notify_settings" => out(crate::notify::notify_settings().await),
        "notify_save" => out(crate::notify::notify_save(a!("settings")).await),
        "notify_test" => out(crate::notify::notify_test().await),
        "agents" => Ok(json!({
            "terminals": crate::claude::pty::list(),
            "agents": crate::notify::hooks::all(),
            "chats": crate::agent::list(),
        })),
        "pty_open" => {
            let cwd: String = a!("cwd");
            let program: Option<String> = a!("program");
            let args_: Vec<String> = arg(args, "args").unwrap_or_default();
            let cols: u16 = arg(args, "cols").unwrap_or(60);
            let rows: u16 = arg(args, "rows").unwrap_or(30);
            let provider: Option<String> = arg(args, "providerId").unwrap_or(None);
            let cwd = expand_home(&cwd);
            // Saved SSH sessions stay on the computer (their password is typed for them): AGENTS.md.
            if cwd.starts_with(&format!("{}{}", crate::claude::remote::SCHEME, crate::claude::remote::SESSION_PREFIX)) {
                return Err(json!("SSH sessions open from DevCat on your computer only."));
            }
            let (program, args_, env) = match provider.filter(|p| !p.is_empty()) {
                Some(pid) => {
                    let l = crate::ai::launch(&pid, args_).await.map_err(|e| json!(e.to_string()))?;
                    (Some(l.program), l.args, l.env)
                }
                None => (program, args_, vec![]),
            };
            let origin = format!("phone:{}", client.device.name);
            let sink = crate::events::sink();
            let id = crate::claude::pty::open(sink, &cwd, program, args_, env, cols, rows, &origin).await.map_err(|e| json!(e.to_string()))?;
            client.attach(&id);
            let info = crate::claude::pty::info(&id).map_err(|e| json!(e.to_string()))?;
            crate::events::emit("remote-pty-opened", &info);
            Ok(json!(info))
        }
        "pty_attach" => {
            let id: String = a!("id");
            client.attach(&id);
            let info = crate::claude::pty::info(&id).map_err(|e| json!(e.to_string()))?;
            let data = crate::claude::pty::scrollback(&id).map_err(|e| json!(e.to_string()))?;
            use base64::Engine;
            Ok(json!({ "info": info, "data": base64::engine::general_purpose::STANDARD.encode(data), "agent": crate::notify::hooks::get(&id) }))
        }
        "pty_detach" => {
            let id: String = a!("id");
            client.detach(&id);
            Ok(Value::Null)
        }
        "pty_write" => out(crate::claude::pty::pty_write(a!("id"), a!("data")).await),
        "pty_resize" => {
            // Only terminals the phone started: resizing a desktop terminal would garble its view there.
            let id: String = a!("id");
            let info = crate::claude::pty::info(&id).map_err(|e| json!(e.to_string()))?;
            if !info.origin.starts_with("phone:") {
                return Ok(Value::Null);
            }
            out(crate::claude::pty::pty_resize(id, a!("cols"), a!("rows")).await)
        }
        "pty_close" => out(crate::claude::pty::pty_close(a!("id")).await),
        "claude_projects" => out(crate::claude::sessions::claude_projects().await),
        // files (project folders; private places are refused, see paths.rs)
        "fs_list" => {
            let dir = expand_home(&arg::<String>(args, "dir")?);
            super::paths::check(&dir).map_err(|e| json!(e))?;
            out(crate::claude::files::fs_list(dir, arg(args, "showIgnored").unwrap_or(None)).await)
        }
        "fs_preview" => {
            let path = expand_home(&arg::<String>(args, "path")?);
            super::paths::check(&path).map_err(|e| json!(e))?;
            let mut p = crate::claude::files::fs_preview(path).await.map_err(|e| json!(e.to_string()))?;
            if p.data_base64.as_ref().is_some_and(|d| d.len() > PHONE_MEDIA_CAP) {
                p.data_base64 = None;
                p.kind = "too_large".into();
            }
            Ok(json!(p))
        }
        "fs_write_text" => {
            let path = expand_home(&arg::<String>(args, "path")?);
            super::paths::check(&path).map_err(|e| json!(e))?;
            out(crate::claude::files::fs_write_text(path, a!("content"), arg(args, "expectedVersion").unwrap_or(None), arg(args, "lineEnding").unwrap_or(None)).await)
        }
        "fs_download" => {
            let path = expand_home(&arg::<String>(args, "path")?);
            super::paths::check(&path).map_err(|e| json!(e))?;
            let len = arg::<Option<usize>>(args, "len").unwrap_or(None).unwrap_or(1024 * 1024);
            super::transfer::read_chunk(path, arg(args, "offset").unwrap_or(0), len).await.map_err(|e| json!(e))
        }
        "fs_upload" | "fs_upload_cancel" => {
            let dir = expand_home(&arg::<String>(args, "dir")?);
            let target = super::transfer::upload_target(&dir, &arg::<String>(args, "name")?).map_err(|e| json!(e))?;
            super::paths::check(&target.to_string_lossy()).map_err(|e| json!(e))?;
            if cmd == "fs_upload_cancel" {
                super::transfer::cancel(&target);
                return Ok(Value::Null);
            }
            let done = arg(args, "done").unwrap_or(false);
            super::transfer::write_chunk(target, arg(args, "offset").unwrap_or(0), a!("data"), done, arg(args, "overwrite").unwrap_or(false)).await.map_err(|e| json!(e))
        }
        "paste_dir" => super::transfer::paste_dir().map(|p| json!(p)).map_err(|e| json!(e)),
        "home_dir" => Ok(json!(dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))),
        "claude_sessions" => out(crate::claude::sessions::claude_sessions(a!("cwd")).await),
        "claude_transcript" => out(crate::claude::sessions::claude_transcript(a!("cwd"), a!("sessionId")).await),
        "ai_providers_list" => out(crate::ai::ai_providers_list().await),
        // DevCat Agent chats (src-tauri/src/agent.rs): the phone gets `agent-event`s of the chats it opened.
        "agent_open" => {
            let session_id: Option<String> = arg(args, "sessionId").unwrap_or(None);
            if client.readonly() && !session_id.as_deref().is_some_and(crate::agent::exists) {
                return Err(json!("This phone has read-only access: it can watch chats but not start one."));
            }
            let origin = format!("phone:{}", client.device.name);
            // Attach first: events sent while the chat opens reach the phone (it skips the ones
            // `open` returns, by `seq`).
            if let Some(id) = &session_id {
                client.attach(id);
            }
            let opened = match crate::agent::open(expand_home(&arg::<String>(args, "cwd")?), a!("providerId"), session_id.clone(), &origin, &client.device.id).await {
                Ok(o) => o,
                Err(e) => {
                    if let Some(id) = &session_id {
                        client.detach(id);
                    }
                    return Err(json!(e.to_string()));
                }
            };
            client.attach(&opened.session_id);
            out(Ok(opened))
        }
        "agent_detach" => {
            client.detach(&arg::<String>(args, "sessionId")?);
            Ok(Value::Null)
        }
        "agent_send" => {
            let session_id: String = a!("sessionId");
            let attachments: Vec<String> = arg(args, "attachments").unwrap_or_default();
            if !attachments.is_empty() {
                // files pasted on the phone are saved in this chat's attachments (agent_attach)
                let own = crate::agent::attachments_dir(&session_id).map_err(|e| json!(e.to_string()))?;
                for p in &attachments {
                    super::paths::check_within(p, &own).map_err(|e| json!(e))?;
                }
            }
            out(crate::agent::agent_send(session_id, a!("text"), a!("policy"), arg(args, "model").unwrap_or(None), Some(attachments), arg(args, "contextWindow").unwrap_or(None)).await)
        }
        "agent_attach" => out(crate::agent::agent_attach(a!("sessionId"), a!("name"), a!("data")).await),
        "agent_stop" => out(crate::agent::agent_stop(a!("sessionId")).await),
        "agent_approve" => out(crate::agent::agent_approve(a!("sessionId"), a!("toolUseId"), a!("decision")).await),
        "agent_answer" => out(crate::agent::agent_answer(a!("sessionId"), a!("toolUseId"), a!("answer")).await),
        "agent_close" => {
            // Only the phone that started the chat ends it: one in a desktop tab stays open there.
            let id: String = a!("sessionId");
            match crate::agent::owner(&id) {
                Some(o) if o == client.device.id => {}
                Some(o) if o == "desktop" => return Err(json!("This chat is open on the computer; close its tab there.")),
                Some(_) => return Err(json!("Another phone started this chat; it can end it there.")),
                None => {}
            }
            client.detach(&id);
            out(crate::agent::close(&id))
        }
        "agent_sessions" => out(crate::agent::agent_sessions(expand_home(&arg::<String>(args, "cwd")?)).await),
        "agent_models" => {
            let pid: String = a!("providerId");
            let (provider, _) = crate::ai::agent_config(&pid).map_err(|e| json!(e.to_string()))?;
            let settings = provider.agent.ok_or_else(|| json!("Not a DevCat Agent connection"))?;
            out(crate::ai::ai_agent_models(settings, Some(pid), None).await)
        }
        "keep_awake_status" => Ok(json!(crate::keep_awake::keep_awake_status())),
        "keep_awake_set" => out(crate::keep_awake::keep_awake_set(a!("on"), arg(args, "minutes").unwrap_or(None)).await),

        // ------------------------------------------------ connections & databases
        // Without SSH sessions: desktop only (AGENTS.md), and the app doesn't know the kind.
        "list_connections" => out(
            commands::connections::list_connections(st())
                .await
                .map(|l| l.into_iter().filter(|c| c.kind != crate::models::ConnectionKind::Ssh).collect::<Vec<_>>()),
        ),
        "list_projects" => out(commands::projects::list_projects(st()).await),
        "open_connection_ids" => out(commands::connections::open_connection_ids(st()).await),
        "close_connection" => out(commands::connections::close_connection(st(), a!("id")).await),
        "db_children" => out(commands::db::db_children(st(), a!("connId"), a!("path")).await),
        "db_list_databases" => out(commands::db::db_list_databases(st(), a!("connId")).await),
        "db_table_data" => {
            let req: crate::db::TableDataRequest = a!("req");
            if client.readonly() && req.filter.as_deref().is_some_and(|f| f.contains(';')) {
                return Err(json!("A filter can't contain ';' on a read-only phone"));
            }
            out(commands::db::db_table_data(st(), a!("connId"), req).await)
        }
        "db_table_structure" => out(commands::db::db_table_structure(st(), a!("connId"), a!("table")).await),
        "db_object_ddl" => out(commands::db::db_object_ddl(st(), a!("connId"), a!("table"), a!("kind")).await),
        "db_execute" => {
            let conn_id: String = a!("connId");
            let script: String = a!("script");
            if client.readonly() {
                let kind = st().store.load_all().ok().and_then(|all| all.into_iter().find(|c| c.id == conn_id)).map(|c| c.kind);
                if !super::readonly::allowed(kind, &script) {
                    return Err(json!("This phone has read-only access: only queries that read data can run."));
                }
            }
            let opts: crate::db::ExecOptions = arg(args, "opts").unwrap_or(crate::db::ExecOptions { database: None, schema: None, max_rows: None });
            out(commands::db::db_execute(st(), conn_id, script, opts, Some(false)).await)
        }
        "db_cancel" => out(commands::db::db_cancel(st(), a!("connId")).await),
        "db_apply_changes" => out(commands::db::db_apply_changes(st(), a!("connId"), a!("table"), a!("changes")).await),
        "db_get_key" => out(commands::db::db_get_key(st(), a!("connId"), a!("database"), a!("key")).await),
        "db_set_key" => out(commands::db::db_set_key(st(), a!("connId"), a!("database"), a!("value")).await),
        "db_delete_key" => out(commands::db::db_delete_key(st(), a!("connId"), a!("database"), a!("key")).await),
        "history_list" => out(commands::history::history_list(st(), arg(args, "connectionId").unwrap_or(None), arg(args, "search").unwrap_or(None), arg(args, "limit").unwrap_or(None)).await),
        "saved_queries_list" => out(commands::history::saved_queries_list(st()).await),

        // ------------------------------------------------ network
        "net_list" => out(crate::network::net_list().await),
        "net_status" => out(crate::network::net_status().await),
        "net_logs" => out(crate::network::net_logs(a!("id")).await),
        "net_traffic" => out(crate::network::net_traffic(a!("id")).await),
        "net_connect" => out(crate::network::net_connect(a!("id"), None).await),
        "net_disconnect" => out(crate::network::net_disconnect(a!("id")).await),

        // ------------------------------------------------ kubernetes
        "k8s_connect" => out(crate::infra::k8s::k8s_connect(Owner::phones(), st(), a!("connId")).await),
        "k8s_namespaces" => out(crate::infra::k8s::k8s_namespaces(st(), a!("connId")).await),
        "k8s_list" => out(crate::infra::k8s::k8s_list(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), arg(args, "labelSelector").unwrap_or(None)).await),
        "k8s_get" => out(crate::infra::k8s::k8s_get(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name")).await),
        "k8s_get_yaml" => out(crate::infra::k8s::k8s_get_yaml(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name")).await),
        "k8s_events_for" => out(crate::infra::k8s::k8s_events_for(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name")).await),
        "k8s_cluster_overview" => out(crate::infra::k8s::k8s_cluster_overview(st(), a!("connId")).await),
        "k8s_metrics" => out(crate::infra::k8s::k8s_metrics(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None)).await),
        "k8s_delete" => out(crate::infra::k8s::k8s_delete(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name"), arg(args, "force").unwrap_or(None)).await),
        "k8s_scale" => out(crate::infra::k8s::k8s_scale(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name"), a!("replicas")).await),
        "k8s_restart" => out(crate::infra::k8s::k8s_restart(st(), a!("connId"), a!("kind"), arg(args, "namespace").unwrap_or(None), a!("name")).await),
        "k8s_cronjob_suspend" => out(crate::infra::k8s::k8s_cronjob_suspend(st(), a!("connId"), a!("namespace"), a!("name"), a!("suspend")).await),
        "k8s_logs_start" => {
            let r = crate::infra::k8s::streams::k8s_logs_start(
                Owner::phones(),
                st(),
                a!("connId"),
                a!("namespace"),
                a!("pod"),
                arg(args, "container").unwrap_or(None),
                arg(args, "tailLines").unwrap_or(Some(500)),
                arg(args, "follow").unwrap_or(true),
                arg(args, "previous").unwrap_or(false),
                arg(args, "timestamps").unwrap_or(false),
                arg(args, "sinceSeconds").unwrap_or(None),
            )
            .await;
            if let Ok(id) = &r {
                client.own_stream(id);
            }
            out(r)
        }
        "k8s_watch_start" => {
            let r = crate::infra::k8s::streams::k8s_watch_start(
                Owner::phones(),
                st(),
                a!("connId"),
                a!("kind"),
                arg(args, "namespace").unwrap_or(None),
                arg(args, "labelSelector").unwrap_or(None),
            )
            .await;
            if let Ok(id) = &r {
                client.own_stream(id);
            }
            out(r)
        }
        "k8s_stream_stop" => {
            let id: String = a!("streamId");
            if !client.owns_stream(&id) {
                return Err(json!("Not a stream this phone started"));
            }
            client.drop_stream(&id);
            out(crate::infra::k8s::streams::k8s_stream_stop(id).await)
        }

        _ => Err(json!(format!("'{cmd}' isn't available from the phone"))),
    }
}

/// `~/x` → the home folder (paths typed on the phone).
fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/").or(if p == "~" { Some("") } else { None }), dirs::home_dir()) {
        (Some(rest), Some(h)) => h.join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

/// One line for the audit log: the call and its most telling arguments (never passwords).
pub fn audit_detail(cmd: &str, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let d = match cmd {
        "db_execute" => s("script").map(|q| {
            let q: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
            if q.chars().count() > 160 { format!("{}…", q.chars().take(160).collect::<String>()) } else { q }
        }),
        "db_apply_changes" => args.get("table").and_then(|t| t.get("name")).and_then(Value::as_str).map(|t| {
            let n = args.get("changes").and_then(Value::as_array).map_or(0, Vec::len);
            format!("{t}: {n} change(s)")
        }),
        "db_set_key" => args.get("value").and_then(|v| v.get("key")).and_then(Value::as_str).map(str::to_string),
        "db_delete_key" | "db_get_key" => s("key"),
        "pty_open" => s("cwd").map(|c| format!("{c} ({})", s("program").unwrap_or_else(|| "shell".into()))),
        "pty_close" => s("id"),
        "fs_preview" | "fs_write_text" | "fs_download" => s("path"),
        "fs_upload" | "fs_upload_cancel" => Some(format!("{}/{}", s("dir").unwrap_or_default(), s("name").unwrap_or_default())),
        "net_connect" | "net_disconnect" | "net_logs" => s("id"),
        "k8s_delete" | "k8s_scale" | "k8s_restart" | "k8s_get_yaml" | "k8s_events_for" => {
            let mut v = format!("{} {}", s("kind").unwrap_or_default(), s("name").unwrap_or_default());
            if let Some(r) = args.get("replicas").and_then(Value::as_i64) {
                v.push_str(&format!(" → {r}"));
            }
            Some(v)
        }
        "k8s_logs_start" => s("pod"),
        _ => None,
    };
    let conn = s("connId").map(|c| format!("[{c}] "));
    d.map(|d| format!("{}{d}", conn.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_lines() {
        let a = json!({ "connId": "c1", "script": "SELECT  *\n  FROM orders" });
        assert_eq!(audit_detail("db_execute", &a).unwrap(), "[c1] SELECT * FROM orders");
        let a = json!({ "connId": "c1", "table": { "name": "orders" }, "changes": [1, 2] });
        assert_eq!(audit_detail("db_apply_changes", &a).unwrap(), "[c1] orders: 2 change(s)");
        let a = json!({ "connId": "k", "kind": "Deployment", "name": "web", "replicas": 3 });
        assert_eq!(audit_detail("k8s_scale", &a).unwrap(), "[k] Deployment web → 3");
        assert_eq!(audit_detail("list_connections", &json!({})), None);
    }

    #[test]
    fn home_expansion() {
        let h = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~/Projects"), h.join("Projects").to_string_lossy());
        assert_eq!(expand_home("/tmp/x"), "/tmp/x");
    }
}
