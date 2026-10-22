//! RabbitMQ management & monitoring via the HTTP management API.
//!
//! Profile mapping: `host`/`port` = management API (default 15672), `user`/`password`,
//! `database` = default vhost, `options.useTls` = https, `options.basePath` = path prefix
//! for reverse-proxied UIs, `options.insecureTls` = accept self-signed certs,
//! `url` = full base URL override.
pub(crate) mod client;

use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::state::St;
use client::{client, enc, page_locally, Rmq};
use reqwest::Method;
use serde_json::{json, Value};

/// Sample window for charts: last 10 minutes at 10s resolution.
fn sample_params() -> Vec<(&'static str, String)> {
    ["lengths_age", "msg_rates_age", "data_rates_age"]
        .into_iter()
        .map(|k| (k, "600".to_string()))
        .chain(["lengths_incr", "msg_rates_incr", "data_rates_incr"].into_iter().map(|k| (k, "10".to_string())))
        .collect()
}

fn vh(vhost: &str) -> String {
    enc(if vhost.is_empty() { "/" } else { vhost })
}

fn norm_resource(resource: &str) -> String {
    resource.trim().trim_matches('/').replace('_', "-")
}

/// Collection path for a resource, optionally scoped to a vhost.
fn list_path(resource: &str, vhost: Option<&str>) -> AppResult<String> {
    let r = norm_resource(resource);
    let vhost = vhost.filter(|v| !v.is_empty());
    Ok(match r.as_str() {
        "queues" | "exchanges" | "bindings" | "consumers" | "policies" | "operator-policies" => match vhost {
            Some(v) => format!("/{r}/{}", enc(v)),
            None => format!("/{r}"),
        },
        "connections" | "channels" | "permissions" | "topic-permissions" => match vhost {
            Some(v) => format!("/vhosts/{}/{r}", enc(v)),
            None => format!("/{r}"),
        },
        "vhosts" | "users" | "nodes" | "feature-flags" | "extensions" | "global-parameters" | "parameters" => {
            format!("/{r}")
        }
        _ => return Err(AppError::msg(format!("Unknown RabbitMQ resource: {resource}"))),
    })
}

/// Detail path for a single object.
fn item_path(resource: &str, vhost: Option<&str>, name: &str) -> AppResult<String> {
    let r = norm_resource(resource);
    Ok(match r.as_str() {
        "exchanges" => format!("/exchanges/{}/{}", vh(vhost.unwrap_or("/")), enc(exchange_name(name))),
        "queues" | "policies" | "operator-policies" => {
            format!("/{r}/{}/{}", vh(vhost.unwrap_or("/")), enc(name))
        }
        "connections" | "channels" | "vhosts" | "users" | "nodes" => format!("/{r}/{}", enc(name)),
        _ => return Err(AppError::msg(format!("Unknown RabbitMQ resource: {resource}"))),
    })
}

fn has_samples(resource: &str) -> bool {
    matches!(
        norm_resource(resource).as_str(),
        "queues" | "exchanges" | "vhosts" | "connections" | "channels" | "nodes"
    )
}

impl Rmq {
    pub async fn overview(&self) -> AppResult<Value> {
        self.get("/overview", &sample_params()).await
    }

    pub async fn list(&self, resource: &str, vhost: Option<&str>) -> AppResult<Value> {
        self.get(&list_path(resource, vhost)?, &[]).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_page(
        &self,
        resource: &str,
        vhost: Option<&str>,
        page: u32,
        page_size: u32,
        name_filter: Option<&str>,
        use_regex: bool,
        sort: Option<&str>,
        sort_reverse: bool,
    ) -> AppResult<Value> {
        let r = norm_resource(resource);
        let vhost = vhost.filter(|v| !v.is_empty());
        // The API paginates these natively (connections/channels only cluster-wide).
        let server_side = matches!(r.as_str(), "queues" | "exchanges")
            || (matches!(r.as_str(), "connections" | "channels") && vhost.is_none());
        let path = list_path(&r, vhost)?;
        if !server_side {
            let list = self.get(&path, &[]).await?;
            return page_locally(&r, list, page, page_size, name_filter, use_regex, sort, sort_reverse);
        }
        let mut q: Vec<(&str, String)> = vec![("page_size", page_size.clamp(1, 500).to_string())];
        if let Some(f) = name_filter.filter(|f| !f.is_empty()) {
            q.push(("name", f.to_string()));
            q.push(("use_regex", use_regex.to_string()));
        }
        if let Some(s) = sort.filter(|s| !s.is_empty()) {
            q.push(("sort", s.to_string()));
            q.push(("sort_reverse", sort_reverse.to_string()));
        }
        let mut with_page = q.clone();
        with_page.push(("page", page.max(1).to_string()));
        let res = match self.get(&path, &with_page).await {
            // The list may have shrunk under us (e.g. queues deleted): fall back to page 1.
            Err(AppError::Msg(m)) if page > 1 && m.to_lowercase().contains("range") => {
                q.push(("page", "1".into()));
                self.get(&path, &q).await?
            }
            other => other?,
        };
        Ok(json!({
            "items": res.get("items").cloned().unwrap_or_else(|| json!([])),
            "total_count": res.get("total_count").cloned().unwrap_or(json!(0)),
            "filtered_count": res.get("filtered_count").cloned().unwrap_or(json!(0)),
            "page_count": res.get("page_count").cloned().unwrap_or(json!(1)),
            "page": res.get("page").cloned().unwrap_or(json!(page)),
            "page_size": res.get("page_size").cloned().unwrap_or(json!(page_size)),
        }))
    }

    pub async fn detail(&self, resource: &str, vhost: Option<&str>, name: &str) -> AppResult<Value> {
        let path = item_path(resource, vhost, name)?;
        let q = if has_samples(resource) { sample_params() } else { vec![] };
        self.get(&path, &q).await
    }

    pub async fn bindings_of(&self, vhost: &str, kind: &str, name: &str) -> AppResult<Value> {
        let (v, n, x) = (vh(vhost), enc(name), enc(exchange_name(name)));
        let path = match kind {
            "queue" => format!("/queues/{v}/{n}/bindings"),
            "source" => format!("/exchanges/{v}/{x}/bindings/source"),
            "destination" => format!("/exchanges/{v}/{x}/bindings/destination"),
            _ => return Err(AppError::msg(format!("Unknown binding kind: {kind}"))),
        };
        self.get(&path, &[]).await
    }

    pub async fn declare_queue(
        &self,
        vhost: &str,
        name: &str,
        durable: bool,
        auto_delete: bool,
        arguments: Value,
    ) -> AppResult<()> {
        let body = json!({ "durable": durable, "auto_delete": auto_delete, "arguments": obj_or_empty(arguments) });
        self.put(&format!("/queues/{}/{}", vh(vhost), enc(name)), &body).await?;
        Ok(())
    }

    pub async fn delete_queue(&self, vhost: &str, name: &str, if_empty: bool, if_unused: bool) -> AppResult<()> {
        let mut q = vec![];
        if if_empty {
            q.push(("if-empty", "true".to_string()));
        }
        if if_unused {
            q.push(("if-unused", "true".to_string()));
        }
        self.delete(&format!("/queues/{}/{}", vh(vhost), enc(name)), &q).await?;
        Ok(())
    }

    pub async fn purge_queue(&self, vhost: &str, name: &str) -> AppResult<()> {
        self.delete(&format!("/queues/{}/{}/contents", vh(vhost), enc(name)), &[]).await?;
        Ok(())
    }

    pub async fn get_messages(
        &self,
        vhost: &str,
        queue: &str,
        count: u32,
        ackmode: &str,
        encoding: &str,
        truncate: Option<u64>,
    ) -> AppResult<Value> {
        if !matches!(ackmode, "ack_requeue_true" | "ack_requeue_false" | "reject_requeue_true" | "reject_requeue_false") {
            return Err(AppError::msg(format!("Invalid ackmode: {ackmode}")));
        }
        let mut body = json!({
            "count": count.max(1),
            "ackmode": ackmode,
            "encoding": if encoding == "base64" { "base64" } else { "auto" },
        });
        if let Some(t) = truncate.filter(|t| *t > 0) {
            body["truncate"] = json!(t);
        }
        self.post(&format!("/queues/{}/{}/get", vh(vhost), enc(queue)), &body).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn declare_exchange(
        &self,
        vhost: &str,
        name: &str,
        kind: &str,
        durable: bool,
        auto_delete: bool,
        internal: bool,
        arguments: Value,
    ) -> AppResult<()> {
        let body = json!({
            "type": kind, "durable": durable, "auto_delete": auto_delete,
            "internal": internal, "arguments": obj_or_empty(arguments),
        });
        self.put(&format!("/exchanges/{}/{}", vh(vhost), enc(name)), &body).await?;
        Ok(())
    }

    pub async fn delete_exchange(&self, vhost: &str, name: &str, if_unused: bool) -> AppResult<()> {
        let q = if if_unused { vec![("if-unused", "true".to_string())] } else { vec![] };
        self.delete(&format!("/exchanges/{}/{}", vh(vhost), enc(exchange_name(name))), &q).await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        vhost: &str,
        exchange: &str,
        routing_key: &str,
        payload: &str,
        payload_encoding: &str,
        properties: Value,
    ) -> AppResult<bool> {
        let body = json!({
            "routing_key": routing_key,
            "payload": payload,
            "payload_encoding": if payload_encoding == "base64" { "base64" } else { "string" },
            "properties": clean_properties(properties),
        });
        let res = self
            .post(&format!("/exchanges/{}/{}/publish", vh(vhost), enc(exchange_name(exchange))), &body)
            .await?;
        Ok(res.get("routed").and_then(Value::as_bool).unwrap_or(false))
    }

    pub async fn create_binding(
        &self,
        vhost: &str,
        source: &str,
        destination_type: &str,
        destination: &str,
        routing_key: &str,
        arguments: Value,
    ) -> AppResult<()> {
        let dt = dest_type(destination_type)?;
        let body = json!({ "routing_key": routing_key, "arguments": obj_or_empty(arguments) });
        let path = format!("/bindings/{}/e/{}/{dt}/{}", vh(vhost), enc(source), enc(destination));
        self.post(&path, &body).await?;
        Ok(())
    }

    pub async fn delete_binding(
        &self,
        vhost: &str,
        source: &str,
        destination_type: &str,
        destination: &str,
        props_key: &str,
    ) -> AppResult<()> {
        let dt = dest_type(destination_type)?;
        let path = format!(
            "/bindings/{}/e/{}/{dt}/{}/{}",
            vh(vhost),
            enc(source),
            enc(destination),
            enc(props_key)
        );
        self.delete(&path, &[]).await?;
        Ok(())
    }

    pub async fn close_connection(&self, name: &str, reason: Option<&str>) -> AppResult<()> {
        let reason = reason.filter(|r| !r.is_empty()).unwrap_or("Closed from DevCat");
        self.send(Method::DELETE, &format!("/connections/{}", enc(name)), &[], None, &[("X-Reason", reason.to_string())])
            .await?;
        Ok(())
    }

    pub async fn raw(&self, method: &str, path: &str, body: Option<Value>) -> AppResult<Value> {
        let method = Method::from_bytes(method.trim().to_uppercase().as_bytes())
            .map_err(|_| AppError::msg(format!("Invalid HTTP method: {method}")))?;
        let mut p = path.trim();
        if let Some(rest) = p.strip_prefix("/api") {
            p = rest;
        }
        let p = if p.starts_with('/') { p.to_string() } else { format!("/{p}") };
        self.send(method, &p, &[], body.as_ref(), &[]).await
    }
}

/// The default exchange is addressed as `amq.default` in the HTTP API.
fn exchange_name(name: &str) -> &str {
    if name.is_empty() { "amq.default" } else { name }
}

fn dest_type(t: &str) -> AppResult<&'static str> {
    match t {
        "q" | "queue" => Ok("q"),
        "e" | "exchange" => Ok("e"),
        _ => Err(AppError::msg(format!("destination_type must be \"q\" or \"e\", got {t:?}"))),
    }
}

fn obj_or_empty(v: Value) -> Value {
    if v.is_object() { v } else { json!({}) }
}

/// Drop empty strings / nulls so the broker doesn't reject e.g. `"expiration": ""`.
fn clean_properties(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, v)| match v {
                    Value::Null => false,
                    Value::String(s) => !s.is_empty(),
                    Value::Object(o) => !o.is_empty(),
                    _ => true,
                })
                .collect(),
        ),
        _ => json!({}),
    }
}

/// Test a RabbitMQ connection profile; returns e.g. "RabbitMQ 4.1.0 (Erlang 27.3)".
pub async fn test(cfg: &ConnectionConfig) -> AppResult<String> {
    let c = Rmq::from_config(cfg)?;
    let o = c.get("/overview", &[]).await?;
    Ok(version_string(&o))
}

fn version_string(o: &Value) -> String {
    let v = o.get("rabbitmq_version").and_then(Value::as_str).unwrap_or("?");
    let e = o.get("erlang_version").and_then(Value::as_str).unwrap_or("?");
    format!("RabbitMQ {v} (Erlang {e})")
}

// ---------------------------------------------------------------------------
// Tauri commands


#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_overview(state: St, conn_id: String) -> AppResult<Value> {
    client(&state, &conn_id).await?.overview().await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_nodes(state: St, conn_id: String) -> AppResult<Value> {
    client(&state, &conn_id).await?.get("/nodes", &[]).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_list(state: St, conn_id: String, resource: String, vhost: Option<String>) -> AppResult<Value> {
    client(&state, &conn_id).await?.list(&resource, vhost.as_deref()).await
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_list_page(
    state: St,
    conn_id: String,
    resource: String,
    vhost: Option<String>,
    page: Option<u32>,
    page_size: Option<u32>,
    name_filter: Option<String>,
    use_regex: Option<bool>,
    sort: Option<String>,
    sort_reverse: Option<bool>,
) -> AppResult<Value> {
    client(&state, &conn_id)
        .await?
        .list_page(
            &resource,
            vhost.as_deref(),
            page.unwrap_or(1),
            page_size.unwrap_or(100),
            name_filter.as_deref(),
            use_regex.unwrap_or(false),
            sort.as_deref(),
            sort_reverse.unwrap_or(false),
        )
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_get(
    state: St,
    conn_id: String,
    resource: String,
    vhost: Option<String>,
    name: String,
) -> AppResult<Value> {
    client(&state, &conn_id).await?.detail(&resource, vhost.as_deref(), &name).await
}

/// Bindings of a queue (`kind` = "queue") or of an exchange as `source` / `destination`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_bindings(state: St, conn_id: String, vhost: String, kind: String, name: String) -> AppResult<Value> {
    client(&state, &conn_id).await?.bindings_of(&vhost, &kind, &name).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_declare_queue(
    state: St,
    conn_id: String,
    vhost: String,
    name: String,
    durable: bool,
    auto_delete: bool,
    arguments: Option<Value>,
) -> AppResult<()> {
    client(&state, &conn_id)
        .await?
        .declare_queue(&vhost, &name, durable, auto_delete, arguments.unwrap_or(Value::Null))
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_delete_queue(
    state: St,
    conn_id: String,
    vhost: String,
    name: String,
    if_empty: Option<bool>,
    if_unused: Option<bool>,
) -> AppResult<()> {
    client(&state, &conn_id)
        .await?
        .delete_queue(&vhost, &name, if_empty.unwrap_or(false), if_unused.unwrap_or(false))
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_purge_queue(state: St, conn_id: String, vhost: String, name: String) -> AppResult<()> {
    client(&state, &conn_id).await?.purge_queue(&vhost, &name).await
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_get_messages(
    state: St,
    conn_id: String,
    vhost: String,
    queue: String,
    count: u32,
    ackmode: String,
    encoding: Option<String>,
    truncate: Option<u64>,
) -> AppResult<Value> {
    client(&state, &conn_id)
        .await?
        .get_messages(&vhost, &queue, count, &ackmode, encoding.as_deref().unwrap_or("auto"), truncate)
        .await
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_declare_exchange(
    state: St,
    conn_id: String,
    vhost: String,
    name: String,
    exchange_type: String,
    durable: bool,
    auto_delete: bool,
    internal: bool,
    arguments: Option<Value>,
) -> AppResult<()> {
    client(&state, &conn_id)
        .await?
        .declare_exchange(&vhost, &name, &exchange_type, durable, auto_delete, internal, arguments.unwrap_or(Value::Null))
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_delete_exchange(
    state: St,
    conn_id: String,
    vhost: String,
    name: String,
    if_unused: Option<bool>,
) -> AppResult<()> {
    client(&state, &conn_id).await?.delete_exchange(&vhost, &name, if_unused.unwrap_or(false)).await
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_publish(
    state: St,
    conn_id: String,
    vhost: String,
    exchange: String,
    routing_key: String,
    payload: String,
    payload_encoding: Option<String>,
    properties: Option<Value>,
) -> AppResult<bool> {
    client(&state, &conn_id)
        .await?
        .publish(
            &vhost,
            &exchange,
            &routing_key,
            &payload,
            payload_encoding.as_deref().unwrap_or("string"),
            properties.unwrap_or(Value::Null),
        )
        .await
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_create_binding(
    state: St,
    conn_id: String,
    vhost: String,
    source: String,
    destination_type: String,
    destination: String,
    routing_key: String,
    arguments: Option<Value>,
) -> AppResult<()> {
    client(&state, &conn_id)
        .await?
        .create_binding(&vhost, &source, &destination_type, &destination, &routing_key, arguments.unwrap_or(Value::Null))
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_delete_binding(
    state: St,
    conn_id: String,
    vhost: String,
    source: String,
    destination_type: String,
    destination: String,
    props_key: String,
) -> AppResult<()> {
    client(&state, &conn_id)
        .await?
        .delete_binding(&vhost, &source, &destination_type, &destination, &props_key)
        .await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_close_connection(
    state: St,
    conn_id: String,
    name: String,
    reason: Option<String>,
) -> AppResult<()> {
    client(&state, &conn_id).await?.close_connection(&name, reason.as_deref()).await
}

/// Raw escape hatch: `path` is relative to `/api` (a leading `/api` is accepted too).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn rmq_request(
    state: St,
    conn_id: String,
    method: String,
    path: String,
    body: Option<Value>,
) -> AppResult<Value> {
    client(&state, &conn_id).await?.raw(&method, &path, body).await
}

#[cfg(test)]
mod tests {
    //! Integration tests against the dev broker (docker-compose.dev.yml).
    //! Run with: `cargo test --lib -- --ignored rabbit`
    use super::*;
    use std::collections::HashMap;

    fn cfg() -> ConnectionConfig {
        // ring and aws-lc-rs are both in the tree; the app installs this in run().
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        ConnectionConfig {
            id: "test".into(),
            name: "test".into(),
            kind: crate::models::ConnectionKind::Rabbitmq,
            host: "localhost".into(),
            port: Some(55673),
            user: "devcat".into(),
            password: Some("devcat".into()),
            database: None,
            file_path: None,
            url: None,
            ssl_mode: None,
            ssh: None,
            color: None,
            group: None,
            project_id: None,
            options: HashMap::new(),
            read_only: false,
        }
    }

    #[test]
    fn rabbit_paths() {
        assert_eq!(list_path("queues", Some("/")).unwrap(), "/queues/%2F");
        assert_eq!(list_path("connections", Some("a/b")).unwrap(), "/vhosts/a%2Fb/connections");
        assert_eq!(list_path("users", Some("/")).unwrap(), "/users");
        assert_eq!(item_path("queues", None, "q 1").unwrap(), "/queues/%2F/q%201");
        let mut c = cfg();
        assert_eq!(client::base_url(&c), "http://localhost:55673/api");
        c.options.insert("useTls".into(), "true".into());
        c.options.insert("basePath".into(), "/rabbit/".into());
        assert_eq!(client::base_url(&c), "https://localhost:55673/rabbit/api");
        c.url = Some("https://mq.example.com/mgmt/".into());
        assert_eq!(client::base_url(&c), "https://mq.example.com/mgmt/api");
    }

    #[tokio::test]
    #[ignore]
    async fn rabbit_version() {
        let v = test(&cfg()).await.unwrap();
        println!("{v}");
        assert!(v.starts_with("RabbitMQ "));
    }

    #[tokio::test]
    #[ignore]
    async fn rabbit_bad_auth() {
        let mut c = cfg();
        c.password = Some("nope".into());
        let e = test(&c).await.unwrap_err().to_string();
        assert!(e.contains("401"), "{e}");
    }

    #[tokio::test]
    #[ignore]
    async fn rabbit_queue_exchange_roundtrip() {
        let c = Rmq::from_config(&cfg()).unwrap();
        let vh = "/";
        let (q, x) = ("devcat.test.q", "devcat.test.x");
        let _ = c.delete_queue(vh, q, false, false).await;
        let _ = c.delete_exchange(vh, x, false).await;

        c.declare_queue(vh, q, true, false, json!({"x-queue-type": "classic", "x-max-length": 1000}))
            .await
            .unwrap();
        c.declare_exchange(vh, x, "topic", true, false, false, json!({})).await.unwrap();
        c.create_binding(vh, x, "q", q, "orders.*", json!({})).await.unwrap();

        // routed through the topic exchange and the default exchange
        assert!(c.publish(vh, x, "orders.created", r#"{"id":1}"#, "string", json!({"content_type":"application/json","delivery_mode":2,"headers":{"h":"v"},"expiration":""})).await.unwrap());
        assert!(c.publish(vh, "", q, "aGVsbG8=", "base64", json!({})).await.unwrap());
        assert!(!c.publish(vh, x, "nomatch", "x", "string", json!({})).await.unwrap());

        let msgs = c.get_messages(vh, q, 10, "ack_requeue_true", "auto", None).await.unwrap();
        let arr = msgs.as_array().unwrap();
        assert_eq!(arr.len(), 2, "{msgs}");
        assert_eq!(arr[0]["payload"], r#"{"id":1}"#);
        assert_eq!(arr[0]["properties"]["headers"]["h"], "v");
        assert_eq!(arr[1]["payload"], "hello");

        let d = c.detail("queues", Some(vh), q).await.unwrap();
        assert_eq!(d["name"], q);
        assert_eq!(d["arguments"]["x-max-length"], 1000);

        let b = c.bindings_of(vh, "queue", q).await.unwrap();
        let ours = b.as_array().unwrap().iter().find(|b| b["source"] == x).cloned().unwrap();
        let src = c.bindings_of(vh, "source", x).await.unwrap();
        assert_eq!(src.as_array().unwrap().len(), 1);

        let page = c.list_page("queues", Some(vh), 1, 10, Some("devcat\\.test"), true, Some("name"), false).await.unwrap();
        assert_eq!(page["filtered_count"], 1, "{page}");
        let page = c.list_page("bindings", Some(vh), 1, 10, Some("devcat.test"), false, Some("routing_key"), true).await.unwrap();
        assert!(page["filtered_count"].as_u64().unwrap() >= 1, "{page}");

        c.delete_binding(vh, x, "q", q, ours["properties_key"].as_str().unwrap()).await.unwrap();
        c.purge_queue(vh, q).await.unwrap();
        let err = c.delete_queue(vh, "devcat.does.not.exist", false, false).await.unwrap_err().to_string();
        assert!(err.contains("404"), "{err}");

        c.delete_queue(vh, q, true, false).await.unwrap();
        c.delete_exchange(vh, x, false).await.unwrap();

        // quorum + stream queue types
        for (name, ty) in [("devcat.test.qq", "quorum"), ("devcat.test.st", "stream")] {
            c.declare_queue(vh, name, true, false, json!({"x-queue-type": ty})).await.unwrap();
            let d = c.detail("queues", Some(vh), name).await.unwrap();
            assert_eq!(d["type"], ty);
            c.delete_queue(vh, name, false, false).await.unwrap();
        }
    }

    #[tokio::test]
    #[ignore]
    async fn rabbit_lists_and_overview() {
        let c = Rmq::from_config(&cfg()).unwrap();
        let o = c.overview().await.unwrap();
        assert!(o.get("rabbitmq_version").is_some());
        assert!(o.get("object_totals").is_some());
        for r in ["queues", "exchanges", "bindings", "connections", "channels", "consumers", "vhosts", "users", "policies", "permissions"] {
            let l = c.list(r, None).await.unwrap();
            assert!(l.is_array(), "{r}: {l}");
            let l = c.list(r, Some("/")).await.unwrap();
            assert!(l.is_array(), "{r} (vhost): {l}");
        }
        let nodes = c.get("/nodes", &[]).await.unwrap();
        let n = nodes[0]["name"].as_str().unwrap().to_string();
        let nd = c.detail("nodes", None, &n).await.unwrap();
        assert!(nd.get("mem_used").is_some());
        let raw = c.raw("get", "/api/whoami", None).await.unwrap();
        assert_eq!(raw["name"], "devcat");
        let p = c.list_page("exchanges", None, 99, 5, None, false, None, false).await.unwrap();
        assert_eq!(p["page"], 1, "{p}");
    }
}
