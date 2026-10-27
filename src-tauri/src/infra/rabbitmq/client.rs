//! Thin client over the RabbitMQ management HTTP API, plus a per-connection cache.
use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::state::AppState;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub struct Rmq {
    http: reqwest::Client,
    /// Base URL including the trailing `/api` (no trailing slash).
    pub base: String,
    user: String,
    password: Option<String>,
    /// Cache key to evict on authentication failures (so a changed password is re-read).
    cache_key: Option<String>,
}

/// conn_id -> (profile fingerprint, client)
static CLIENTS: LazyLock<Mutex<HashMap<String, (String, Arc<Rmq>)>>> = LazyLock::new(Default::default);

/// Resolve (and cache) the client for a saved connection profile.
pub async fn client(state: &AppState, conn_id: &str) -> AppResult<Arc<Rmq>> {
    // The JSON store has no secrets, so it's cheap to read for a fingerprint; the
    // keychain is only touched when the profile changed or the cache is cold.
    let stored = state
        .store
        .load_all()?
        .into_iter()
        .find(|c| c.id == conn_id)
        .ok_or_else(|| AppError::msg(format!("Connection {conn_id} not found")))?;
    // Reconnects the VPN / tunnel this profile depends on (cheap no-op when connected).
    crate::network::ensure_for(&stored).await?;
    let fp = serde_json::to_string(&stored)?;
    if let Some((f, c)) = CLIENTS.lock().get(conn_id) {
        if *f == fp {
            return Ok(c.clone());
        }
    }
    let cfg = state.store.get(conn_id)?;
    let mut rmq = Rmq::from_config(&cfg)?;
    rmq.cache_key = Some(conn_id.to_string());
    let rmq = Arc::new(rmq);
    CLIENTS.lock().insert(conn_id.to_string(), (fp, rmq.clone()));
    Ok(rmq)
}

pub fn evict(conn_id: &str) {
    CLIENTS.lock().remove(conn_id);
}

/// Percent-encode a single path segment (vhost `/` becomes `%2F`).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn base_url(cfg: &ConnectionConfig) -> String {
    let opt = |k: &str| cfg.options.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());
    let mut base = match cfg.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => {
            let tls = opt("useTls") == Some("true");
            let host = cfg.host.trim();
            let host = if host.is_empty() { "localhost" } else { host };
            // Tolerate a scheme typed into the host field.
            let (scheme, host) = match host.split_once("://") {
                Some((s, h)) => (s.to_string(), h.trim_end_matches('/')),
                None => ((if tls { "https" } else { "http" }).to_string(), host),
            };
            let port = cfg
                .port
                .or_else(|| opt("managementPort").and_then(|p| p.parse().ok()))
                .unwrap_or(if tls { 443 } else { 15672 });
            let mut b = format!("{scheme}://{host}:{port}");
            if let Some(p) = opt("basePath") {
                let p = p.trim_matches('/');
                if !p.is_empty() {
                    b.push('/');
                    b.push_str(p);
                }
            }
            b
        }
    };
    if !base.ends_with("/api") {
        base.push_str("/api");
    }
    base
}

impl Rmq {
    pub fn from_config(cfg: &ConnectionConfig) -> AppResult<Self> {
        // Self-signed management certs are common on dev brokers.
        let insecure = cfg.options.get("insecureTls").is_some_and(|s| s == "true");
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .tls_danger_accept_invalid_certs(insecure)
            .build()?;
        let user = if cfg.user.is_empty() { "guest".to_string() } else { cfg.user.clone() };
        let password = cfg.password.clone().or_else(|| (user == "guest").then(|| "guest".to_string()));
        Ok(Self { http, base: base_url(cfg), user, password, cache_key: None })
    }

    /// Perform a request against `{base}{path}`; `path` starts with `/`.
    /// Returns the parsed JSON body, or `Null` for empty bodies.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        headers: &[(&str, String)],
    ) -> AppResult<Value> {
        let url = format!("{}{}", self.base, path);
        let mut rb = self.http.request(method, &url).basic_auth(&self.user, self.password.as_ref());
        if !query.is_empty() {
            rb = rb.query(query);
        }
        for (k, v) in headers {
            rb = rb.header(*k, v);
        }
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let resp = rb.send().await.map_err(|e| AppError::msg(format!("Request to {url} failed: {}", crate::error::chain(&e))))?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            if status == StatusCode::UNAUTHORIZED {
                if let Some(k) = &self.cache_key {
                    evict(k);
                }
            }
            return Err(api_error(status, &text));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> AppResult<Value> {
        self.send(Method::GET, path, query, None, &[]).await
    }
    pub async fn put(&self, path: &str, body: &Value) -> AppResult<Value> {
        self.send(Method::PUT, path, &[], Some(body), &[]).await
    }
    pub async fn post(&self, path: &str, body: &Value) -> AppResult<Value> {
        self.send(Method::POST, path, &[], Some(body), &[]).await
    }
    pub async fn delete(&self, path: &str, query: &[(&str, String)]) -> AppResult<Value> {
        self.send(Method::DELETE, path, query, None, &[]).await
    }
}

fn api_error(status: StatusCode, body: &str) -> AppError {
    let reason = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let r = v.get("reason").and_then(reason_str);
            let e = v.get("error").and_then(reason_str);
            r.or(e)
        })
        .unwrap_or_else(|| body.trim().chars().take(300).collect());
    let label = match status {
        StatusCode::UNAUTHORIZED => "authentication failed".to_string(),
        StatusCode::FORBIDDEN => "access refused".to_string(),
        StatusCode::NOT_FOUND => "not found".to_string(),
        s => s.canonical_reason().unwrap_or("error").to_lowercase(),
    };
    if reason.is_empty() {
        AppError::Msg(format!("RabbitMQ API {} ({label})", status.as_u16()))
    } else {
        AppError::Msg(format!("RabbitMQ API {} ({label}): {reason}", status.as_u16()))
    }
}

fn reason_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Local (client-side) paging for endpoints the API cannot paginate.

/// Look up a dotted path (`message_stats.publish_details.rate`) in a JSON value.
pub fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, k| cur.get(k))
}

/// Text that a name filter is matched against for a given resource row.
pub fn search_text(resource: &str, v: &Value) -> String {
    let fields: &[&str] = match resource {
        "bindings" => &["source", "destination", "routing_key"],
        "consumers" => &["queue.name", "consumer_tag", "channel_details.connection_name"],
        "permissions" | "topic-permissions" => &["user", "vhost"],
        "connections" => &["name", "user", "client_properties.connection_name"],
        "channels" => &["name", "user", "connection_details.name"],
        _ => &["name"],
    };
    let mut s = String::new();
    for f in fields {
        if let Some(Value::String(x)) = get_path(v, f) {
            s.push_str(x);
            s.push('\u{1}');
        }
    }
    s
}

fn cmp_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => {
            x.as_f64().unwrap_or(0.0).partial_cmp(&y.as_f64().unwrap_or(0.0)).unwrap_or(Ordering::Equal)
        }
        (Some(Value::String(x)), Some(Value::String(y))) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
        (None | Some(Value::Null), None | Some(Value::Null)) => Ordering::Equal,
        (None | Some(Value::Null), _) => Ordering::Less,
        (_, None | Some(Value::Null)) => Ordering::Greater,
        (Some(x), Some(y)) => x.to_string().cmp(&y.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn page_locally(
    resource: &str,
    list: Value,
    page: u32,
    page_size: u32,
    name_filter: Option<&str>,
    use_regex: bool,
    sort: Option<&str>,
    sort_reverse: bool,
) -> AppResult<Value> {
    let mut items = match list {
        Value::Array(a) => a,
        other => return Err(AppError::msg(format!("Unexpected response for {resource}: {other}"))),
    };
    let total = items.len();
    if let Some(f) = name_filter.filter(|f| !f.is_empty()) {
        if use_regex {
            let re = regex::RegexBuilder::new(f)
                .case_insensitive(true)
                .build()
                .map_err(|e| AppError::msg(format!("Invalid regex: {e}")))?;
            items.retain(|v| re.is_match(&search_text(resource, v)));
        } else {
            let f = f.to_lowercase();
            items.retain(|v| search_text(resource, v).to_lowercase().contains(&f));
        }
    }
    if let Some(s) = sort.filter(|s| !s.is_empty()) {
        items.sort_by(|a, b| cmp_values(get_path(a, s), get_path(b, s)));
        if sort_reverse {
            items.reverse();
        }
    }
    let filtered = items.len();
    let page_size = page_size.max(1) as usize;
    let page_count = filtered.div_ceil(page_size).max(1);
    let page = (page.max(1) as usize).min(page_count);
    let start = (page - 1) * page_size;
    let slice: Vec<Value> = items.into_iter().skip(start).take(page_size).collect();
    Ok(serde_json::json!({
        "items": slice,
        "total_count": total,
        "filtered_count": filtered,
        "page_count": page_count,
        "page": page,
        "page_size": page_size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_vhost() {
        assert_eq!(enc("/"), "%2F");
        assert_eq!(enc("my vhost/a"), "my%20vhost%2Fa");
        assert_eq!(enc("q.name-1_x~"), "q.name-1_x~");
    }

    #[test]
    fn local_paging() {
        let list = serde_json::json!([{"name":"b","messages":2},{"name":"a","messages":5},{"name":"c","messages":1}]);
        let p = page_locally("queues", list, 1, 2, None, false, Some("messages"), true).unwrap();
        assert_eq!(p["items"][0]["name"], "a");
        assert_eq!(p["page_count"], 2);
        assert_eq!(p["filtered_count"], 3);
    }
}
