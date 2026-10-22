//! Debug builds only: a remote-access host with made-up data, for testing DevCat Mobile end to
//! end without touching real connections (and for its screenshots). The real server and
//! protocol (remote::server), real terminals (claude::pty) and real agent status (notify::hooks);
//! only the databases / VPNs / clusters are fake. Pairing is approved automatically.
//! Start it with `cargo run --example remotedemo -- [port] [data dir]`.
use crate::remote::server::{Backend, Client, Host, PairRequest};
use crate::remote::store::RemoteStore;
use devcat_remote_proto::{HostInfo, PairLink};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn iso(mins_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(mins_ago)).to_rfc3339()
}

const FIRST: [&str; 12] = ["Ava", "Noah", "Mia", "Liam", "Zoe", "Ethan", "Iris", "Leo", "Nora", "Omar", "Ines", "Kai"];
const LAST: [&str; 8] = ["Tan", "Silva", "Novak", "Haddad", "Kim", "Moreau", "Rossi", "Santos"];
const STATUS: [&str; 6] = ["delivered", "shipped", "paid", "pending", "delivered", "refunded"];
const COUNTRY: [&str; 6] = ["ID", "SG", "DE", "US", "NL", "JP"];

fn orders() -> Vec<Value> {
    let mut seed: u64 = 7;
    let mut rnd = || {
        seed = seed * 16807 % 2_147_483_647;
        seed as f64 / 2_147_483_647.0
    };
    (0..240)
        .map(|i: i64| {
            let f = FIRST[(rnd() * 12.0) as usize];
            let l = LAST[(rnd() * 8.0) as usize];
            let status = STATUS[(rnd() * 6.0) as usize];
            let items = 1 + (rnd() * 6.0) as i64;
            let total = format!("{:.2}", items as f64 * (9.0 + rnd() * 70.0));
            let country = COUNTRY[(rnd() * 6.0) as usize];
            json!([10480 - i, format!("{f} {l}"), format!("{f}.{l}@example.com").to_lowercase(), status, total, items, country, status != "pending", iso(i * 37)])
        })
        .collect()
}

fn order_cols() -> Value {
    json!([
        { "name": "id", "typeName": "int8" }, { "name": "customer", "typeName": "text" }, { "name": "email", "typeName": "text" },
        { "name": "status", "typeName": "text" }, { "name": "total", "typeName": "numeric" }, { "name": "items", "typeName": "int4" },
        { "name": "country", "typeName": "text" }, { "name": "paid", "typeName": "bool" }, { "name": "created_at", "typeName": "timestamptz" }
    ])
}

fn result(statement: &str, columns: Value, rows: Vec<Value>, ms: u64) -> Value {
    json!({ "statement": statement, "columns": columns, "rows": rows, "elapsedMs": ms, "truncated": false, "rowsAffected": null, "message": null })
}

fn conn(id: &str, name: &str, kind: &str, host: &str, project: &str) -> Value {
    json!({ "id": id, "name": name, "kind": kind, "host": host, "port": null, "user": "app", "projectId": project, "options": {}, "readOnly": false })
}

fn connections() -> Value {
    json!([
        conn("c-pg", "shop-db", "postgres", "db.acme.internal", "p-shop"),
        conn("c-redis", "shop-cache", "redis", "cache.acme.internal", "p-shop"),
        conn("c-k8s", "prod-cluster", "kubernetes", "", "p-shop"),
        conn("c-mysql", "ledger", "mysql", "ledger.acme.internal", "p-pay"),
        conn("c-mongo", "events-store", "mongodb", "mongo.acme.internal", "p-pay"),
    ])
}

fn node(path: &[String], label: &str, kind: &str, has_children: bool, detail: Option<&str>, table: Option<Value>) -> Value {
    let mut p = path.to_vec();
    p.push(label.to_string());
    json!({ "path": p, "label": label, "kind": kind, "hasChildren": has_children, "detail": detail, "table": table })
}

fn children(conn_id: &str, path: &[String]) -> Value {
    let t = |name: &str| Some(json!({ "database": "shop", "schema": "public", "name": name }));
    match (conn_id, path.len()) {
        ("c-pg", 0) => json!([node(path, "shop", "database", true, None, None), node(path, "analytics", "database", true, None, None)]),
        ("c-pg", 1) => json!([node(path, "public", "schema", true, None, None), node(path, "billing", "schema", true, None, None)]),
        ("c-pg", 2) => json!([node(path, "Tables", "folder", true, Some("6"), None), node(path, "Views", "folder", true, Some("2"), None)]),
        ("c-pg", 3) if path[2] == "Tables" => json!([
            node(path, "customers", "table", false, Some("18K"), t("customers")),
            node(path, "order_items", "table", false, Some("96K"), t("order_items")),
            node(path, "orders", "table", false, Some("240"), t("orders")),
            node(path, "products", "table", false, Some("1.2K"), t("products")),
            node(path, "refunds", "table", false, Some("312"), t("refunds")),
            node(path, "shipments", "table", false, Some("41K"), t("shipments")),
        ]),
        ("c-pg", 3) => json!([node(path, "daily_revenue", "view", false, None, t("daily_revenue"))]),
        ("c-redis", 0) => json!([node(path, "db0", "database", true, Some("1,204 keys"), None), node(path, "db1", "database", true, Some("12 keys"), None)]),
        ("c-redis", _) => {
            let db = path[0].trim_start_matches("db").to_string();
            let key = |k: &str, ty: &str| {
                let mut p = path.to_vec();
                p.push(k.to_string());
                json!({ "path": p, "label": k, "kind": "key", "hasChildren": false, "detail": ty, "table": { "database": db, "schema": null, "name": format!("{}{k}", path[1..].concat()) } })
            };
            if path.len() == 1 {
                {
                let group = |g: &str, n: &str| {
                    let mut p = path.to_vec();
                    p.push(format!("{g}:"));
                    json!({ "path": p, "label": g, "kind": "keyGroup", "hasChildren": true, "detail": n, "table": null })
                };
                json!([group("cart", "380"), group("session", "611"), group("user", "212"), key("feature-flags", "hash")])
            }
            } else {
                json!([key("1001", "hash"), key("1002", "hash"), key("1002:token", "string"), key("queue:mail", "list"), key("leaderboard", "zset")])
            }
        }
        ("c-mongo", 0) => json!([node(path, "events", "database", true, None, None)]),
        ("c-mongo", 1) => json!([node(path, "Collections", "folder", true, Some("2"), None)]),
        ("c-mongo", 2) => json!([
            node(path, "clicks", "collection", false, Some("2.1M"), Some(json!({ "database": "events", "schema": null, "name": "clicks" }))),
            node(path, "payments", "collection", false, Some("88K"), Some(json!({ "database": "events", "schema": null, "name": "payments" }))),
        ]),
        ("c-mysql", 0) => json!([node(path, "ledger", "database", true, None, None)]),
        ("c-mysql", 1) => json!([node(path, "Tables", "folder", true, Some("2"), None)]),
        ("c-mysql", 2) => json!([node(path, "entries", "table", false, Some("1.4M"), Some(json!({ "database": "ledger", "schema": null, "name": "entries" })))]),
        _ => json!([]),
    }
}

fn table_data(req: &Value) -> Value {
    let name = req["table"]["name"].as_str().unwrap_or("");
    let offset = req["offset"].as_u64().unwrap_or(0) as usize;
    let limit = req["limit"].as_u64().unwrap_or(50) as usize;
    if name != "orders" {
        return json!({ "result": result("", json!([{ "name": "id", "typeName": "int8" }, { "name": "name", "typeName": "text" }]), vec![json!([1, "example"]), json!([2, "sample"])], 3), "total": 2, "primaryKey": ["id"] });
    }
    let mut rows = orders();
    if let Some(f) = req["filter"].as_str().filter(|f| !f.is_empty()) {
        let f = f.to_lowercase();
        // Understands `status = 'x'` and plain words.
        let needle = f.split('\'').nth(1).unwrap_or(&f).to_string();
        rows.retain(|r| r.as_array().unwrap().iter().any(|c| c.to_string().to_lowercase().contains(&needle)));
    }
    let total = rows.len();
    json!({ "result": result("", order_cols(), rows.into_iter().skip(offset).take(limit).collect(), 4), "total": total, "primaryKey": ["id"] })
}

fn structure() -> Value {
    let cols: Vec<Value> = order_cols()
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let n = c["name"].as_str().unwrap();
            json!({ "name": n, "dataType": c["typeName"], "nullable": !["id", "customer", "status", "total"].contains(&n), "isPrimaryKey": n == "id", "autoIncrement": n == "id", "ordinal": i + 1 })
        })
        .collect();
    json!({ "table": { "database": "shop", "schema": "public", "name": "orders" }, "columns": cols, "indexes": [], "foreignKeys": [] })
}

fn execute(script: &str, conn_id: &str) -> Value {
    if conn_id == "c-redis" {
        return json!([result(script.trim(), json!([{ "name": "result", "typeName": "" }]), vec![json!(["OK"])], 1)]);
    }
    let lower = script.to_lowercase();
    if lower.trim_start().starts_with("select") {
        let mut by: Vec<(String, i64, f64)> = Vec::new();
        for r in orders() {
            let c = r[6].as_str().unwrap().to_string();
            let t: f64 = r[4].as_str().unwrap().parse().unwrap();
            match by.iter_mut().find(|x| x.0 == c) {
                Some(e) => {
                    e.1 += 1;
                    e.2 += t;
                }
                None => by.push((c, 1, t)),
            }
        }
        by.sort_by(|a, b| b.2.total_cmp(&a.2));
        let rows = by.into_iter().map(|(c, n, s)| json!([c, n, format!("{s:.2}"), format!("{:.2}", s / n as f64)])).collect();
        return json!([result(script.trim(), json!([{ "name": "country", "typeName": "text" }, { "name": "orders", "typeName": "int8" }, { "name": "revenue", "typeName": "numeric" }, { "name": "avg_order", "typeName": "numeric" }]), rows, 12)]);
    }
    json!([{ "statement": script.trim(), "columns": [], "rows": [], "rowsAffected": 1, "elapsedMs": 9, "truncated": false, "message": null }])
}

fn redis_key(key: &str) -> Value {
    match key {
        k if k.ends_with(":token") => json!({ "key": k, "type": "string", "ttl": 7140, "value": "{\"sub\":1002,\"scope\":\"checkout\",\"exp\":1767225600}", "length": 52 }),
        k if k.contains("queue") => json!({ "key": k, "type": "list", "ttl": -1, "value": ["welcome:1002", "receipt:10480", "receipt:10479"], "length": 3 }),
        k if k.contains("leaderboard") => json!({ "key": k, "type": "zset", "ttl": -1, "value": [["ava", 1820], ["noah", 1504], ["iris", 990]], "length": 3 }),
        "feature-flags" => json!({ "key": key, "type": "hash", "ttl": -1, "value": { "new_checkout": "on", "black_friday_banner": "off" }, "length": 2 }),
        k => json!({ "key": k, "type": "hash", "ttl": -1, "value": { "name": "Ava Tan", "email": "ava.tan@example.com", "plan": "pro", "visits": "42" }, "length": 4 }),
    }
}

fn pod(name: &str, ns: &str, app: &str, phase: &str, waiting: Option<&str>, restarts: i64, age: i64) -> Value {
    let state = match waiting {
        Some(r) => json!({ "waiting": { "reason": r } }),
        None => json!({ "running": {} }),
    };
    json!({
        "kind": "Pod",
        "metadata": { "name": name, "namespace": ns, "creationTimestamp": iso(age), "labels": { "app": app } },
        "spec": { "containers": [{ "name": app, "image": format!("ghcr.io/acme/{app}:1.4.2") }] },
        "status": { "phase": phase, "containerStatuses": [{ "name": app, "ready": phase == "Running" && waiting.is_none(), "restartCount": restarts, "state": state }] }
    })
}

fn pods() -> Vec<Value> {
    vec![
        pod("storefront-7f9c6d8b5-2xkqp", "shop", "storefront", "Running", None, 0, 3 * 1440),
        pod("storefront-7f9c6d8b5-h7wzn", "shop", "storefront", "Running", None, 0, 3 * 1440),
        pod("checkout-api-5b6d9f7c4-8lmzv", "shop", "checkout-api", "Running", None, 0, 26 * 60),
        pod("search-indexer-6c8b7d9f-kq9xw", "shop", "search-indexer", "Running", Some("CrashLoopBackOff"), 12, 9 * 1440),
        pod("image-resizer-54f7b9c8d-zr5tp", "shop", "image-resizer", "Pending", Some("ContainerCreating"), 0, 1),
        pod("shop-db-1", "database", "shop-db", "Running", None, 0, 21 * 1440),
        pod("redis-0", "database", "redis", "Running", None, 0, 40 * 1440),
        pod("payments-worker-6d5c4b7f8-9jxlk", "payments", "payments-worker", "Running", None, 0, 5 * 60),
    ]
}

fn deployments() -> Value {
    let d = |name: &str, ns: &str, want: i64, ready: i64| json!({ "kind": "Deployment", "metadata": { "name": name, "namespace": ns, "creationTimestamp": iso(30 * 1440) }, "spec": { "replicas": want }, "status": { "readyReplicas": ready } });
    json!([d("storefront", "shop", 3, 3), d("checkout-api", "shop", 2, 2), d("search-indexer", "shop", 1, 0), d("image-resizer", "shop", 1, 0), d("payments-worker", "payments", 2, 2)])
}

const ESC: &str = "\x1b[";

fn claude_screen(question: bool) -> String {
    let c = |code: &str, s: &str| format!("{ESC}{code}m{s}{ESC}0m");
    let mut l = vec![
        String::new(),
        format!("{} {}  {}", c("38;5;209", "✻"), c("1", "Claude Code"), c("2", "~/acme/storefront")),
        String::new(),
        format!("{} The checkout total is wrong with a coupon. Fix it.", c("2", ">")),
        String::new(),
        format!("{} I'll look at how the cart total is computed.", c("38;5;114", "⏺")),
        format!("{} {}(src/cart/total.ts)", c("38;5;114", "⏺"), c("1", "Read")),
        "  ⎿  Read 48 lines".into(),
        format!("{} {}(src/cart/total.ts)", c("38;5;114", "⏺"), c("1", "Update")),
        "  ⎿  Updated with 3 additions and 2 removals".into(),
        String::new(),
    ];
    if question {
        l.extend([
            format!("{} {}(pnpm test cart)", c("38;5;114", "⏺"), c("1", "Bash")),
            String::new(),
            " Bash command".into(),
            "   pnpm test cart".into(),
            "   Run the cart tests".into(),
            String::new(),
            " Do you want to proceed?".into(),
            format!(" {}", c("38;5;209", "❯ 1. Yes")),
            "   2. Yes, and don't ask again for pnpm test".into(),
            "   3. No, and tell Claude what to do differently (esc)".into(),
        ]);
    } else {
        l.extend([
            format!("{} Fixed: coupons apply to the pre-tax subtotal; all 14 cart tests pass.", c("38;5;114", "⏺")),
            String::new(),
            c("2", &format!("╭{}╮", "─".repeat(50))),
            format!("{} >{}{}", c("2", "│"), " ".repeat(48), c("2", "│")),
            c("2", &format!("╰{}╯", "─".repeat(50))),
        ]);
    }
    l.join("\r\n")
}

struct Demo {
    host: Mutex<Option<Arc<Host>>>,
    awake: Mutex<bool>,
    seq: AtomicU64,
    net: Mutex<Vec<(String, String)>>,
    notify: Mutex<Value>,
    dir: std::path::PathBuf,
}

impl Demo {
    fn broadcast(&self, event: &str, payload: Value) {
        if let Some(h) = self.host.lock().as_ref() {
            h.broadcast(event, &payload);
        }
    }

    fn net_status(&self) -> Value {
        Value::Array(
            self.net
                .lock()
                .iter()
                .map(|(id, st)| json!({ "id": id, "state": st, "since": iso(42), "detail": if st == "connected" { Some("10.8.0.14 via utun6") } else { None }, "error": null, "localPorts": [] }))
                .collect(),
        )
    }

    async fn open_pty(self: &Arc<Self>, cwd: &str, script: &str, origin: &str, cols: u16, rows: u16) -> Result<crate::claude::pty::PtyInfo, Value> {
        let me = self.clone();
        let sink: crate::claude::runner::Sink = Arc::new(move |event, payload| me.broadcast(event, payload));
        let file = self.dir.join(format!("screen-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&file, script).map_err(|e| json!(e.to_string()))?;
        let cmd = format!("cat '{}'; exec cat", file.display());
        let id = crate::claude::pty::open(sink, cwd, Some("sh".into()), vec!["-c".into(), cmd], vec![], cols, rows, origin).await.map_err(|e| json!(e.to_string()))?;
        crate::claude::pty::info(&id).map_err(|e| json!(e.to_string()))
    }
}

/// The pty list says "sh"; show what the demo pretends to run.
fn pretend(mut v: Value) -> Value {
    if let Some(a) = v.as_array_mut() {
        for t in a {
            if t["program"] == "sh" {
                t["program"] = json!("claude");
                t["agent"] = json!(true);
            }
        }
    } else if v["program"] == "sh" {
        v["program"] = json!("claude");
        v["agent"] = json!(true);
    }
    v
}

#[async_trait::async_trait]
impl Backend for Arc<Demo> {
    async fn call(&self, client: &Arc<Client>, cmd: &str, args: Value) -> Result<Value, Value> {
        if client.readonly() && crate::remote::dispatch::WRITES.contains(&cmd) {
            return Err(json!("This phone has read-only access."));
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Ok(match cmd {
            "host_info" => json!(self.host_info()),
            "agents" => json!({ "terminals": pretend(json!(crate::claude::pty::list())), "agents": crate::notify::hooks::all(), "chats": crate::agent::list() }),
            // DevCat Agent chats are real (see `run`): the app's own commands.
            "agent_open" | "agent_detach" | "agent_send" | "agent_attach" | "agent_stop" | "agent_approve" | "agent_answer" | "agent_close" | "agent_sessions" | "agent_models" => {
                return crate::remote::dispatch::call(client, cmd, &args).await;
            }
            "pty_open" => {
                let cwd = s("cwd").replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy());
                let cwd = if std::path::Path::new(&cwd).is_dir() { cwd } else { std::env::temp_dir().to_string_lossy().into_owned() };
                let cols = args["cols"].as_u64().unwrap_or(60) as u16;
                let info = self.open_pty(&cwd, &claude_screen(false), &format!("phone:{}", client.device.name), cols, args["rows"].as_u64().unwrap_or(30) as u16).await?;
                client.attach(&info.id);
                let id = info.id.clone();
                let (status, _) = crate::notify::hooks::apply(&id, &json!({ "hook_event_name": "UserPromptSubmit", "cwd": s("cwd") }), crate::remote::server::now_ms());
                self.broadcast("agent-status", json!(status));
                pretend(json!(info))
            }
            "pty_attach" => {
                let id = s("id");
                client.attach(&id);
                use base64::Engine;
                let info = crate::claude::pty::info(&id).map_err(|e| json!(e.to_string()))?;
                let data = crate::claude::pty::scrollback(&id).map_err(|e| json!(e.to_string()))?;
                json!({ "info": pretend(json!(info)), "data": base64::engine::general_purpose::STANDARD.encode(data) })
            }
            "pty_detach" => {
                client.detach(&s("id"));
                Value::Null
            }
            "pty_write" => {
                crate::claude::pty::pty_write(s("id"), s("data")).await.map_err(|e| json!(e.to_string()))?;
                Value::Null
            }
            "pty_resize" => {
                let _ = crate::claude::pty::pty_resize(s("id"), args["cols"].as_u64().unwrap_or(60) as u16, args["rows"].as_u64().unwrap_or(30) as u16).await;
                Value::Null
            }
            "pty_close" => {
                crate::claude::pty::pty_close(s("id")).await.map_err(|e| json!(e.to_string()))?;
                Value::Null
            }
            "claude_projects" => {
                let p = |n: &str| self.dir.join("projects/acme").join(n).to_string_lossy().into_owned();
                json!([
                    { "cwd": p("storefront"), "sessionCount": 14, "lastActive": iso(20) },
                    { "cwd": p("api-gateway"), "sessionCount": 6, "lastActive": iso(300) },
                    { "cwd": p("ui"), "sessionCount": 9, "lastActive": iso(1500) },
                ])
            }
            "claude_sessions" => json!([
                { "id": "s1", "title": "Fix checkout total with coupons", "messageCount": 24, "gitBranch": "main", "costUsd": 0.42, "updatedAt": iso(120) },
                { "id": "s2", "title": "Add CSV export to orders", "messageCount": 61, "gitBranch": "feat/export", "costUsd": 1.1, "updatedAt": iso(1500) },
                { "id": "s3", "title": "Upgrade to React 19", "messageCount": 132, "gitBranch": "chore/react19", "costUsd": 3.25, "updatedAt": iso(4300) },
            ]),
            "claude_transcript" => json!([
                { "type": "user", "message": { "role": "user", "content": "The checkout total is off by one cent when a coupon applies. Fix it." } },
                { "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "I'll look at how totals are computed in the cart." }, { "type": "tool_use", "name": "Read", "input": { "file_path": "src/cart/total.ts" } }] } },
                { "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "name": "Edit", "input": { "file_path": "src/cart/total.ts" } }, { "type": "tool_use", "name": "Bash", "input": { "command": "pnpm test cart" } }] } },
                { "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": "Fixed: coupons now apply to the pre-tax subtotal. All 14 cart tests pass." }] } },
                { "type": "user", "message": { "role": "user", "content": "Great, commit it." } },
                { "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": "git commit -am \"Apply coupons before tax\"" } }] } },
            ]),
            "ai_providers_list" => json!(crate::ai::ai_providers_list().await.map_err(|e| json!(e.to_string()))?),
            "home_dir" => json!(self.dir.join("projects").to_string_lossy()),
            "fs_list" => json!(crate::claude::files::fs_list(s("dir"), args["showIgnored"].as_bool()).await.map_err(|e| json!(e.to_string()))?),
            "fs_preview" => json!(crate::claude::files::fs_preview(s("path")).await.map_err(|e| json!(e.to_string()))?),
            "fs_write_text" => json!(
                crate::claude::files::fs_write_text(s("path"), s("content"), args["expectedVersion"].as_str().map(str::to_string), args["lineEnding"].as_str().map(str::to_string))
                    .await
                    .map_err(|e| json!(e.to_string()))?
            ),
            "fs_download" => crate::remote::transfer::read_chunk(s("path"), args["offset"].as_u64().unwrap_or(0), args["len"].as_u64().unwrap_or(1 << 20) as usize).await.map_err(|e| json!(e))?,
            "fs_upload" | "fs_upload_cancel" => {
                let target = crate::remote::transfer::upload_target(&s("dir"), &s("name")).map_err(|e| json!(e))?;
                if cmd == "fs_upload_cancel" {
                    crate::remote::transfer::cancel(&target);
                    Value::Null
                } else {
                    let (offset, done, overwrite) = (args["offset"].as_u64().unwrap_or(0), args["done"].as_bool().unwrap_or(false), args["overwrite"].as_bool().unwrap_or(false));
                    crate::remote::transfer::write_chunk(target, offset, s("data"), done, overwrite).await.map_err(|e| json!(e))?
                }
            }
            "keep_awake_status" => json!({ "on": *self.awake.lock() }),
            "keep_awake_set" => {
                *self.awake.lock() = args["on"].as_bool().unwrap_or(false);
                json!({ "on": *self.awake.lock() })
            }
            "list_connections" => connections(),
            "list_projects" => json!([{ "id": "p-shop", "name": "Acme Shop", "order": 0 }, { "id": "p-pay", "name": "Payments", "order": 1 }]),
            "open_connection_ids" => json!(["c-pg"]),
            "db_children" => children(&s("connId"), &serde_json::from_value::<Vec<String>>(args["path"].clone()).unwrap_or_default()),
            "db_table_data" => table_data(&args["req"]),
            "db_table_structure" => structure(),
            "db_execute" => execute(&s("script"), &s("connId")),
            "db_apply_changes" => json!(1),
            "db_get_key" => redis_key(&s("key")),
            "db_set_key" | "db_delete_key" | "db_cancel" => Value::Null,
            "history_list" => json!([{ "id": "h1", "connectionId": "c-pg", "database": "shop", "query": "SELECT country, count(*) FROM orders GROUP BY 1", "executedAt": iso(12), "elapsedMs": 38, "success": true }]),
            "saved_queries_list" => json!([{ "id": "q1", "name": "Revenue by country", "connectionId": "c-pg", "query": "SELECT country, sum(total) FROM orders GROUP BY 1 ORDER BY 2 DESC;", "updatedAt": iso(3000) }]),
            "net_list" => json!([
                { "id": "n-ovpn", "name": "office-vpn", "kind": "openvpn", "autoConnect": false, "forwards": [], "options": {} },
                { "id": "n-ssh", "name": "bastion", "kind": "ssh", "autoConnect": false, "forwards": [], "options": {} },
                { "id": "n-wg", "name": "staging-wg", "kind": "wireguard", "autoConnect": false, "forwards": [], "options": {} },
            ]),
            "net_status" => self.net_status(),
            "net_logs" => json!(["Initialization Sequence Completed", "net_route_v4_add: 10.0.0.0/8 via 10.8.0.1"]),
            "net_traffic" => json!((0..60).map(|i| json!({ "id": s("id"), "at": crate::remote::server::now_ms() - (60 - i) * 1000, "rxRate": 40_000 + (i * 7919 % 90_000), "txRate": 9_000 + (i * 3571 % 20_000), "rxTotal": 0, "txTotal": 0 })).collect::<Vec<_>>()),
            "net_connect" | "net_disconnect" => {
                let id = s("id");
                let st = if cmd == "net_connect" { "connected" } else { "disconnected" };
                {
                    let mut n = self.net.lock();
                    match n.iter_mut().find(|x| x.0 == id) {
                        Some(x) => x.1 = st.into(),
                        None => n.push((id.clone(), st.into())),
                    }
                }
                self.broadcast("net-status", json!({ "id": id, "state": st, "since": iso(0), "detail": if st == "connected" { Some("10.40.0.2 via utun7") } else { None }, "error": null, "localPorts": [] }));
                Value::Null
            }
            "k8s_connect" => json!({ "version": "v1.33.4", "context": "acme-prod" }),
            "k8s_namespaces" => json!(["database", "default", "payments", "shop"]),
            "k8s_list" => {
                let ns = args["namespace"].as_str().map(str::to_string);
                match s("kind").as_str() {
                    "pods" => json!(pods().into_iter().filter(|p| ns.as_deref().is_none_or(|n| p["metadata"]["namespace"] == n)).collect::<Vec<_>>()),
                    "deployments" => deployments(),
                    "nodes" => json!(["node-a", "node-b", "node-c"].map(|n| json!({ "kind": "Node", "metadata": { "name": n, "creationTimestamp": iso(90 * 1440), "labels": { "node-role.kubernetes.io/worker": "" } }, "status": { "conditions": [{ "type": "Ready", "status": "True" }], "nodeInfo": { "kubeletVersion": "v1.33.4" } } }))),
                    _ => json!([]),
                }
            }
            "k8s_get" => pods().into_iter().find(|p| p["metadata"]["name"] == s("name")).unwrap_or(Value::Null),
            "k8s_events_for" => json!([{ "type": "Warning", "reason": "BackOff", "message": "Back-off restarting failed container search-indexer", "count": 12, "lastTimestamp": iso(2) }]),
            "k8s_get_yaml" => json!(format!("apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\n  namespace: shop\nspec:\n  containers:\n  - name: app\n    image: ghcr.io/acme/app:1.4.2\n", s("name"))),
            "k8s_scale" | "k8s_restart" | "k8s_delete" => Value::Null,
            "k8s_logs_start" => {
                let id = format!("log-{}", self.seq.fetch_add(1, Ordering::Relaxed));
                client.own_stream(&id);
                let (me, sid, c) = (self.clone(), id.clone(), client.clone());
                tokio::spawn(async move {
                    let mut n = 0;
                    while c.owns_stream(&sid) && n < 400 {
                        let lines = if n == 0 {
                            vec!["starting search-indexer v2.4.1".to_string(), "connecting to redis cache:6379".into()]
                        } else if n % 5 == 4 {
                            vec![format!("ERROR dial tcp 10.0.4.12:6379: connect: connection refused (attempt {n})")]
                        } else {
                            vec![format!("indexed batch {n} (500 products) in 31ms")]
                        };
                        me.broadcast("k8s-log", json!({ "streamId": sid, "lines": lines }));
                        n += 1;
                        tokio::time::sleep(Duration::from_millis(900)).await;
                    }
                });
                json!(id)
            }
            "k8s_stream_stop" => {
                client.drop_stream(&s("streamId"));
                Value::Null
            }
            "notify_settings" => self.notify.lock().clone(),
            "notify_save" => {
                *self.notify.lock() = args["settings"].clone();
                self.notify.lock().clone()
            }
            "notify_test" => Value::Null,
            "audit" => json!([
                { "at": crate::remote::server::now_ms() - 120_000, "device": "", "deviceName": "", "action": "db_apply_changes", "detail": "[c-pg] orders: 1 change(s)", "ok": true },
                { "at": crate::remote::server::now_ms() - 600_000, "device": "", "deviceName": "", "action": "pty_open", "detail": "~/acme/api-gateway (claude)", "ok": true },
                { "at": crate::remote::server::now_ms() - 900_000, "device": "", "deviceName": "", "action": "net_connect", "detail": "n-wg — handshake timed out", "ok": false },
            ]),
            _ => return Err(json!(format!("'{cmd}' isn't available from the phone"))),
        })
    }

    async fn approve(&self, req: PairRequest) -> Option<String> {
        eprintln!("pairing request from {} ({}) — approving automatically (demo)", req.name, req.platform);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        Some("full".into())
    }

    fn host_info(&self) -> HostInfo {
        let port = self.host.lock().as_ref().map(|h| h.store.settings().port).unwrap_or(0);
        HostInfo {
            id: self.host.lock().as_ref().and_then(|h| h.endpoint_id()).unwrap_or_default(),
            name: "Demo MacBook".into(),
            os: "macos".into(),
            app_version: "0.4.0".into(),
            addrs: crate::remote::reachable_addrs(port, None),
            relay: demo_relay(),
        }
    }

    fn changed(&self) {}

    async fn disconnected(&self, client: &Arc<Client>) {
        for s in client.streams() {
            client.drop_stream(&s);
        }
    }
}

/// Made-up project folders for the Files screens.
fn sample_projects(root: &std::path::Path) -> std::io::Result<()> {
    let files: &[(&str, &str)] = &[
        ("storefront/README.md", "# Storefront\n\nThe Acme web shop: product pages, cart and checkout.\n\n## Run\n\n```sh\npnpm install\npnpm dev\n```\n\n- Cart totals: `src/cart/total.ts`\n- Payments: `src/checkout/pay.ts`\n"),
        ("storefront/package.json", "{\n  \"name\": \"storefront\",\n  \"private\": true,\n  \"scripts\": { \"dev\": \"vite\", \"test\": \"vitest\" }\n}\n"),
        ("storefront/src/cart/total.ts", "import { applyCoupon, withTax } from \"./pricing\";\n\nexport function cartTotal(cart: Cart): number {\n  const subtotal = sumLines(cart.lines);\n  const discount = applyCoupon(subtotal, cart.coupon);\n  // tax is charged on the discounted amount\n  return withTax(subtotal - discount);\n}\n"),
        ("storefront/src/cart/pricing.ts", "export const TAX_RATE = 0.11;\n\nexport function withTax(n: number) {\n  return Math.round(n * (1 + TAX_RATE) * 100) / 100;\n}\n"),
        ("storefront/src/checkout/pay.ts", "export async function pay(orderId: string) {\n  return fetch(`/api/orders/${orderId}/pay`, { method: \"POST\" });\n}\n"),
        ("storefront/data/orders.csv", "id,customer,total,status\n10480,Ava Santos,120.11,shipped\n10479,Liam Rossi,227.66,delivered\n10478,Omar Silva,289.32,paid\n"),
        ("api-gateway/README.md", "# API gateway\n\nRoutes `/api/*` to the services.\n"),
        ("api-gateway/main.go", "package main\n\nfunc main() {\n\tserve(\":8080\")\n}\n"),
        ("ui/README.md", "# UI kit\n\nShared React components.\n"),
    ];
    for (rel, text) in files {
        let p = root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, text)?;
    }
    Ok(())
}

/// Run the demo host until the process is killed. Prints the pairing link (valid 5 minutes,
/// a new one every 4 minutes).
pub async fn run(port: u16, dir: std::path::PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::notify::hooks::start().await.map_err(|e| e.to_string())?;
    let store = Arc::new(RemoteStore::new(&dir));
    let mut st = store.settings();
    st.port = port;
    st.enabled = true;
    store.save_settings(&st).map_err(|e| e.to_string())?;
    let demo = Arc::new(Demo {
        host: Mutex::new(None),
        awake: Mutex::new(true),
        seq: AtomicU64::new(1),
        net: Mutex::new(vec![("n-ovpn".into(), "connected".into()), ("n-ssh".into(), "disconnected".into()), ("n-wg".into(), "disconnected".into())]),
        notify: Mutex::new(json!({ "enabled": true, "server": "https://ntfy.sh", "topic": "devcat-demo7k2q9x4m", "onDone": true, "onInput": true, "minSeconds": 30, "onlyWhenAway": true })),
        dir: dir.clone(),
    });
    let host = Host::new(store, Arc::new(demo.clone()));
    *demo.host.lock() = Some(host.clone());
    // DevCat Agent: real chats (src/agent.rs) against the OpenAI-compatible model API in
    // DEVCAT_DEMO_AGENT_URL (a local mock will do); their events go to the phones.
    crate::ai::init(dir.clone());
    crate::agent::init(dir.clone());
    if let Ok(url) = std::env::var("DEVCAT_DEMO_AGENT_URL") {
        let providers = json!([{ "id": "demo-agent", "name": "DevCat Agent", "kind": "agent", "agent": { "protocol": "openai-chat", "baseUrl": url, "model": "demo-coder" } }]);
        std::fs::write(dir.join("ai_providers.json"), providers.to_string()).map_err(|e| e.to_string())?;
    }
    let h = host.clone();
    crate::events::add_tap(move |event, payload| {
        if matches!(event, "agent-event" | "agent-chat" | "agent-chat-gone") {
            h.broadcast(event, payload);
        }
    });
    // DEVCAT_DEMO_RELAY=public (or a relay URL) lets phones on other networks in.
    let relay = demo_relay();
    host.start(port, true, relay.as_deref()).await?;

    // Sample project folders (Files screens) and three agents working in them, as real terminals.
    let projects = dir.join("projects/acme");
    sample_projects(&projects).map_err(|e| e.to_string())?;
    let proj = |n: &str| projects.join(n).to_string_lossy().into_owned();
    let home = proj("storefront");
    let now = crate::remote::server::now_ms();
    let a = demo.open_pty(&home, &claude_screen(true), "desktop", 74, 34).await.map_err(|e| e.to_string())?;
    crate::notify::hooks::apply(&a.id, &json!({ "hook_event_name": "UserPromptSubmit", "cwd": proj("storefront") }), now - 200_000);
    crate::notify::hooks::apply(&a.id, &json!({ "hook_event_name": "Notification", "message": "Claude needs your permission to use Bash", "cwd": proj("storefront") }), now - 20_000);
    let b = demo.open_pty(&home, &claude_screen(false), "phone:Demo phone", 60, 30).await.map_err(|e| e.to_string())?;
    crate::notify::hooks::apply(&b.id, &json!({ "hook_event_name": "UserPromptSubmit", "cwd": proj("api-gateway") }), now - 192_000);
    let c = demo.open_pty(&home, &claude_screen(false), "desktop", 74, 30).await.map_err(|e| e.to_string())?;
    crate::notify::hooks::apply(&c.id, &json!({ "hook_event_name": "UserPromptSubmit", "cwd": proj("ui") }), now - 900_000);
    crate::notify::hooks::apply(&c.id, &json!({ "hook_event_name": "Stop", "cwd": proj("ui") }), now - 420_000);

    // A fresh code whenever the last one was used or is about to expire.
    let id = host.endpoint_id().unwrap_or_default();
    loop {
        if host.pairing_expires().is_none_or(|t| t - crate::remote::server::now_ms() < 60_000) {
            let (code, secret, _) = host.pair_start();
            let pl = PairLink { id: id.clone(), code, name: "Demo MacBook".into(), addrs: crate::remote::reachable_addrs(port, Some(&format!("127.0.0.1:{port}"))), relay: relay.clone() };
            let link = pl.to_url();
            if relay.is_some() {
                let page = std::env::var("DEVCAT_DEMO_PAGE").unwrap_or_else(|_| crate::remote::HOSTED_PAGE.into());
                let hosted = pl.to_hosted_url(&page);
                println!("HOSTED {hosted}");
                std::fs::write(dir.join("hosted-link.txt"), &hosted).map_err(|e| e.to_string())?;
            }
            let web = PairLink { addrs: vec![format!("127.0.0.1:{port}")], ..pl }.to_web_url(&devcat_remote_proto::web::b64url(&secret));
            println!("PAIR {link}\nWEB {web}");
            std::fs::write(dir.join("pair-link.txt"), &link).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("web-link.txt"), &web).map_err(|e| e.to_string())?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn demo_relay() -> Option<String> {
    std::env::var("DEVCAT_DEMO_RELAY").ok().filter(|r| !r.is_empty())
}
