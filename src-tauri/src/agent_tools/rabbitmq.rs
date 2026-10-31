//! RabbitMQ tools over the user's DevCat RabbitMQ connections (the management API): the broker's
//! overview and nodes, queues / exchanges / bindings / connections / channels / consumers /
//! vhosts / policies (the fields that matter, filtered and paged), peeking at a queue's messages
//! (they go back on the queue), publishing, declaring and binding. Users and permissions aren't
//! offered (they carry password hashes). Changes need the connection's AI access at Full over
//! MCP; the DevCat Agent asks first. Deleting or purging loses messages for good, so `rmq_delete`
//! is the DevCat Agent's only, confirmed by typing the name (`Tool::confirm`).
use super::*;
use crate::infra::rabbitmq as mq;

/// What `rmq_list` lists, and the fields it shows of each.
const RESOURCES: &[(&str, &[&str])] = &[
    (
        "queues",
        &[
            "name", "vhost", "type", "state", "messages", "messages_ready", "messages_unacknowledged", "consumers", "durable", "auto_delete", "policy",
            "arguments", "idle_since", "message_stats.publish_details.rate", "message_stats.deliver_get_details.rate", "message_stats.ack_details.rate",
        ],
    ),
    ("exchanges", &["name", "vhost", "type", "durable", "auto_delete", "internal", "arguments", "message_stats.publish_in_details.rate", "message_stats.publish_out_details.rate"]),
    ("bindings", &["source", "vhost", "destination", "destination_type", "routing_key", "arguments", "properties_key"]),
    ("connections", &["name", "user", "vhost", "state", "client_properties.connection_name", "client_properties.product", "peer_host", "channels", "connected_at"]),
    ("channels", &["name", "user", "vhost", "state", "consumer_count", "messages_unacknowledged", "prefetch_count", "connection_details.name"]),
    ("consumers", &["queue.name", "queue.vhost", "consumer_tag", "channel_details.connection_name", "ack_required", "prefetch_count", "active", "activity_status"]),
    ("vhosts", &["name", "messages", "messages_ready", "messages_unacknowledged", "tracing", "description"]),
    ("policies", &["name", "vhost", "pattern", "apply-to", "definition", "priority"]),
];

const OVERVIEW: &[&str] = &[
    "cluster_name", "rabbitmq_version", "erlang_version", "management_version", "node", "queue_totals", "object_totals",
    "message_stats.publish_details.rate", "message_stats.deliver_get_details.rate", "message_stats.ack_details.rate",
];

const NODE: &[&str] = &["name", "running", "type", "uptime", "mem_used", "mem_limit", "mem_alarm", "disk_free", "disk_free_limit", "disk_free_alarm", "fd_used", "fd_total", "partitions"];

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>) {
    out.push(Arc::new(RmqOverview(s.clone())));
    out.push(Arc::new(RmqList(s.clone())));
    out.push(Arc::new(RmqPeek(s.clone())));
    out.push(Arc::new(RmqPublish(s.clone())));
    out.push(Arc::new(RmqManage(s.clone())));
    if !s.mcp {
        out.push(Arc::new(RmqDelete(s.clone())));
    }
}

fn rmq_conn(scope: &Scope, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    conn_of(scope, input, ConnectionKind::Rabbitmq)
}

/// The vhost the model means, else the connection's, else "/".
fn vhost(c: &ConnectionConfig, input: &Value) -> String {
    arg(input, "vhost").or_else(|| c.database.clone().filter(|d| !d.trim().is_empty())).unwrap_or_else(|| "/".into())
}

fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.get(k))
}

/// Only `fields` of `v` (dotted paths flattened: "message_stats.publish_details.rate" →
/// "publish_rate"-style keys stay readable as the last two parts).
fn pick(v: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for f in fields {
        if let Some(x) = at(v, f).filter(|x| !x.is_null()) {
            let key = match f.rsplit_once('.') {
                Some((head, "rate")) => format!("{}_rate", head.rsplit('.').next().unwrap_or(head).trim_end_matches("_details")),
                _ => f.to_string(),
            };
            out.insert(key, x.clone());
        }
    }
    Value::Object(out)
}

// ---------- rmq_overview

struct RmqOverview(Arc<Scope>);

#[async_trait]
impl Tool for RmqOverview {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_overview".into(),
            description: "The state of a DevCat RabbitMQ connection's broker: version, message totals (ready / unacked), object counts, publish / deliver / ack rates, and each node's memory, disk and file descriptors with any alarm.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "connection": { "type": "string", "description": "RabbitMQ connection name or id" } },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        conn_summary(input)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match rmq_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let overview = match mq::rmq_overview(St::get(), c.id.clone()).await {
            Ok(v) => pick(&v, OVERVIEW),
            Err(e) => return ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        };
        let nodes = mq::rmq_nodes(St::get(), c.id.clone()).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let nodes: Vec<Value> = nodes.iter().map(|n| pick(n, NODE)).collect();
        ToolOutput::ok(pretty(&json!({ "overview": overview, "nodes": nodes })))
    }
}

// ---------- rmq_list

struct RmqList(Arc<Scope>);

#[async_trait]
impl Tool for RmqList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_list".into(),
            description: "List a DevCat RabbitMQ connection's queues (default), exchanges, bindings, connections, channels, consumers, vhosts or policies, with the fields that matter (queue depth, consumers, rates, ...). `vhost` limits it to one vhost (all when left out), `filter` keeps names containing it (`regex: true` for a regular expression), `sort` by a field (e.g. messages, with `desc`), 50 per `page`. With `name` (and `vhost`): that queue or exchange in full.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "RabbitMQ connection name or id" },
                    "resource": { "type": "string", "enum": RESOURCES.iter().map(|(r, _)| *r).collect::<Vec<_>>() },
                    "vhost": { "type": "string" },
                    "name": { "type": "string", "description": "One queue or exchange" },
                    "filter": { "type": "string" },
                    "regex": { "type": "boolean" },
                    "sort": { "type": "string" },
                    "desc": { "type": "boolean" },
                    "page": { "type": "integer" }
                },
                "required": ["connection"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        let r = arg(input, "resource").unwrap_or_else(|| "queues".into());
        format!("{}: {r}{}", conn_summary(input), arg(input, "name").or_else(|| arg(input, "filter")).map(|n| format!(" {n}")).unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match rmq_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let resource = arg(&input, "resource").unwrap_or_else(|| "queues".into());
        let Some((_, fields)) = RESOURCES.iter().find(|(r, _)| *r == resource) else {
            return ToolOutput::err(format!("Unknown resource \"{resource}\": {}.", RESOURCES.iter().map(|(r, _)| *r).collect::<Vec<_>>().join(", ")));
        };
        if let Some(name) = arg(&input, "name") {
            if !matches!(resource.as_str(), "queues" | "exchanges") {
                return ToolOutput::err("`name` reads one queue or exchange; for the rest use `filter`.");
            }
            return match mq::rmq_get(St::get(), c.id.clone(), resource.clone(), Some(vhost(&c, &input)), name).await {
                Ok(mut v) => {
                    // a queue's backing process internals are noise for the model
                    if let Some(m) = v.as_object_mut() {
                        m.remove("backing_queue_status");
                        m.remove("garbage_collection");
                    }
                    ToolOutput::ok(pretty(&v))
                }
                Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
            };
        }
        let page = input.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as u32;
        let res = mq::rmq_list_page(St::get(), c.id.clone(), resource.clone(), arg(&input, "vhost"), Some(page), Some(50), arg(&input, "filter"), arg_bool(&input, "regex"), arg(&input, "sort"), arg_bool(&input, "desc")).await;
        match res {
            Ok(p) => {
                let items: Vec<String> = p["items"].as_array().map(|a| a.iter().map(|i| pick(i, fields).to_string()).collect()).unwrap_or_default();
                if items.is_empty() {
                    return ToolOutput::ok(format!("No {resource}."));
                }
                let head = format!("{resource}: {} of {} (page {} of {}):", p["filtered_count"], p["total_count"], p["page"], p["page_count"]);
                ToolOutput::ok(format!("{head}\n{}", items.join("\n")))
            }
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- rmq_peek

struct RmqPeek(Arc<Scope>);

#[async_trait]
impl Tool for RmqPeek {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_peek".into(),
            description: "Look at the first `count` messages (default 5, at most 50) of a queue on a DevCat RabbitMQ connection: payload, routing key, exchange, headers and properties. The messages go back on the queue (marked redelivered); nothing is consumed.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "RabbitMQ connection name or id" },
                    "queue": { "type": "string" },
                    "vhost": { "type": "string" },
                    "count": { "type": "integer" }
                },
                "required": ["connection", "queue"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Query
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {}", conn_summary(input), arg(input, "queue").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match rmq_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(queue) = arg(&input, "queue") else { return ToolOutput::err("Missing `queue`.") };
        let count = input.get("count").and_then(Value::as_u64).unwrap_or(5).clamp(1, 50) as u32;
        match mq::rmq_get_messages(St::get(), c.id.clone(), vhost(&c, &input), queue.clone(), count, "ack_requeue_true".into(), Some("auto".into()), Some(20_000)).await {
            Ok(Value::Array(msgs)) if msgs.is_empty() => ToolOutput::ok(format!("{queue} has no ready messages.")),
            Ok(v) => ToolOutput::ok(pretty(&v)),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- rmq_publish

struct RmqPublish(Arc<Scope>);

#[async_trait]
impl Tool for RmqPublish {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_publish".into(),
            description: "Publish a message on a DevCat RabbitMQ connection: to `exchange` (empty: the default exchange, where the routing key is the queue name) with `routing_key`. `properties`: AMQP properties like content_type, headers, delivery_mode (2 = persistent). Tells whether a queue got it. Consumers act on it: only when the user asks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "RabbitMQ connection name or id" },
                    "vhost": { "type": "string" },
                    "exchange": { "type": "string" },
                    "routing_key": { "type": "string" },
                    "payload": { "type": "string" },
                    "base64": { "type": "boolean", "description": "The payload is base64" },
                    "properties": { "type": "object" }
                },
                "required": ["connection", "routing_key", "payload"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {} → {}", conn_summary(input), arg(input, "exchange").unwrap_or_else(|| "(default)".into()), arg(input, "routing_key").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match rmq_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let payload = input.get("payload").and_then(Value::as_str).unwrap_or("").to_string();
        let encoding = if arg_bool(&input, "base64") == Some(true) { "base64" } else { "string" };
        let props = input.get("properties").filter(|p| p.is_object()).cloned();
        let exchange = input.get("exchange").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let key = input.get("routing_key").and_then(Value::as_str).unwrap_or("").to_string();
        match mq::rmq_publish(St::get(), c.id.clone(), vhost(&c, &input), exchange, key, payload, Some(encoding.into()), props).await {
            Ok(true) => ToolOutput::ok("Published; a queue got it."),
            Ok(false) => ToolOutput::ok("Published, but no queue is bound for that routing key: the message was dropped."),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- rmq_manage

struct RmqManage(Arc<Scope>);

#[async_trait]
impl Tool for RmqManage {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_manage".into(),
            description: "Change a DevCat RabbitMQ connection's broker: `op` = declare_queue (name, durable, auto_delete, arguments), declare_exchange (name, type direct/fanout/topic/headers, durable, auto_delete, internal, arguments), bind (source exchange → destination queue or exchange, routing_key, arguments), unbind (source, destination, properties_key from `rmq_list` bindings) or close_connection (name, reason; the client may reconnect). Deleting and purging are `rmq_delete`, in the DevCat Agent only.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "RabbitMQ connection name or id" },
                    "op": { "type": "string", "enum": ["declare_queue", "declare_exchange", "bind", "unbind", "close_connection"] },
                    "vhost": { "type": "string" },
                    "name": { "type": "string" },
                    "type": { "type": "string" },
                    "durable": { "type": "boolean" },
                    "auto_delete": { "type": "boolean" },
                    "internal": { "type": "boolean" },
                    "arguments": { "type": "object" },
                    "source": { "type": "string" },
                    "destination": { "type": "string" },
                    "destination_type": { "type": "string", "enum": ["queue", "exchange"] },
                    "routing_key": { "type": "string" },
                    "properties_key": { "type": "string" },
                    "reason": { "type": "string" }
                },
                "required": ["connection", "op"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        let what = arg(input, "name").or_else(|| arg(input, "source").map(|s| format!("{s} → {}", arg(input, "destination").unwrap_or_default()))).unwrap_or_default();
        format!("{}: {} {what}", conn_summary(input), arg(input, "op").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match rmq_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let (st, id, vh) = (St::get(), c.id.clone(), vhost(&c, &input));
        let b = |k: &str| arg_bool(&input, k);
        let need = |k: &str| arg(&input, k).ok_or_else(|| ToolOutput::err(format!("Missing `{k}`.")));
        let args_of = || input.get("arguments").filter(|a| a.is_object()).cloned();
        let op = arg(&input, "op").unwrap_or_default();
        let res = match op.as_str() {
            "declare_queue" => match need("name") {
                Ok(n) => mq::rmq_declare_queue(st, id, vh, n, b("durable").unwrap_or(true), b("auto_delete").unwrap_or(false), args_of()).await,
                Err(e) => return e,
            },
            "declare_exchange" => match need("name") {
                Ok(n) => mq::rmq_declare_exchange(st, id, vh, n, arg(&input, "type").unwrap_or_else(|| "direct".into()), b("durable").unwrap_or(true), b("auto_delete").unwrap_or(false), b("internal").unwrap_or(false), args_of()).await,
                Err(e) => return e,
            },
            "bind" => match (need("source"), need("destination")) {
                (Ok(s), Ok(d)) => {
                    let key = input.get("routing_key").and_then(Value::as_str).unwrap_or("").to_string();
                    mq::rmq_create_binding(st, id, vh, s, dest_type(&input), d, key, args_of()).await
                }
                (Err(e), _) | (_, Err(e)) => return e,
            },
            "unbind" => match (need("source"), need("destination")) {
                (Ok(s), Ok(d)) => {
                    let key = arg(&input, "properties_key").unwrap_or_else(|| "~".into());
                    mq::rmq_delete_binding(st, id, vh, s, dest_type(&input), d, key).await
                }
                (Err(e), _) | (_, Err(e)) => return e,
            },
            "close_connection" => match need("name") {
                Ok(n) => mq::rmq_close_connection(st, id, n, arg(&input, "reason")).await,
                Err(e) => return e,
            },
            "delete_queue" | "purge_queue" | "delete_exchange" => return ToolOutput::err("Deleting and purging are `rmq_delete` (DevCat Agent only: the user confirms it by typing the name)."),
            other => return ToolOutput::err(format!("Unknown `op` \"{other}\".")),
        };
        match res {
            Ok(()) => ToolOutput::ok(format!("Done: {op}.")),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- rmq_delete

struct RmqDelete(Arc<Scope>);

#[async_trait]
impl Tool for RmqDelete {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rmq_delete".into(),
            description: "On a DevCat RabbitMQ connection's broker: `op` = delete_queue (with its messages; `if_empty` / `if_unused` to refuse otherwise), purge_queue (drops every ready message, keeps the queue) or delete_exchange (`if_unused`). The messages are gone for good: only when the user asked, say which first. DevCat asks the user to confirm by typing the queue's or exchange's name.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "RabbitMQ connection name or id" },
                    "op": { "type": "string", "enum": ["delete_queue", "purge_queue", "delete_exchange"] },
                    "vhost": { "type": "string" },
                    "name": { "type": "string" },
                    "if_empty": { "type": "boolean" },
                    "if_unused": { "type": "boolean" }
                },
                "required": ["connection", "op", "name"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {} {}", conn_summary(input), arg(input, "op").unwrap_or_default(), arg(input, "name").unwrap_or_default())
    }
    fn confirm(&self, input: &Value) -> Option<String> {
        Some(arg(input, "name").unwrap_or_else(|| "(no name)".into()))
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        if self.0.mcp {
            return ToolOutput::err("Queues and exchanges are deleted or purged from the DevCat Agent in DevCat only, where the user confirms it.");
        }
        let c = match rmq_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(name) = arg(&input, "name") else { return ToolOutput::err("Missing `name`.") };
        let (st, id, vh, b) = (St::get(), c.id.clone(), vhost(&c, &input), |k: &str| arg_bool(&input, k));
        let op = arg(&input, "op").unwrap_or_default();
        let res = match op.as_str() {
            "delete_queue" => mq::rmq_delete_queue(st, id, vh, name.clone(), b("if_empty"), b("if_unused")).await,
            "purge_queue" => mq::rmq_purge_queue(st, id, vh, name.clone()).await,
            "delete_exchange" => mq::rmq_delete_exchange(st, id, vh, name.clone(), b("if_unused")).await,
            other => return ToolOutput::err(format!("Unknown `op` {other}: delete_queue, purge_queue or delete_exchange.")),
        };
        match res {
            Ok(()) => ToolOutput::ok(format!("Done: {op} {name}.")),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

/// "q" / "e" as the management API's binding paths want them.
fn dest_type(input: &Value) -> String {
    if arg(input, "destination_type").as_deref() == Some("exchange") { "e" } else { "q" }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_queues_keep_what_matters() {
        let q = json!({ "name": "jobs", "vhost": "/", "messages": 12, "consumers": 0, "garbage_collection": { "x": 1 }, "message_stats": { "publish_details": { "rate": 2.5 } } });
        let fields = RESOURCES.iter().find(|(r, _)| *r == "queues").unwrap().1;
        assert_eq!(pick(&q, fields), json!({ "name": "jobs", "vhost": "/", "messages": 12, "consumers": 0, "publish_rate": 2.5 }));
        assert!(!RESOURCES.iter().any(|(r, _)| *r == "users" || *r == "permissions"), "users carry password hashes");
    }
}
