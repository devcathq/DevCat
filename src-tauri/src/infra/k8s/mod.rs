//! Kubernetes monitoring (Lens-like).
//!
//! Every resource is handled through `Api<DynamicObject>` driven by a small table of
//! built-in kinds, plus discovery for arbitrary `apiVersion/Kind` (CRDs). Objects are
//! returned raw (managedFields stripped) and the frontend derives its columns.
//! Streaming features (logs, watch, exec, port-forward) live in `streams`.
pub mod helm;
pub mod streams;

use crate::error::{AppError, AppResult};
use crate::models::ConnectionConfig;
use crate::state::{AppState, St};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Event as CoreEvent, Namespace, Node, Pod};
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, GroupVersionKind};
use kube::discovery::Scope;
use kube::{Client, Config};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Kubeconfig loading & client cache
// ---------------------------------------------------------------------------

/// A live client for one saved connection.
#[derive(Clone)]
pub(crate) struct Conn {
    pub client: Client,
    pub context: String,
    pub server: String,
    pub namespace: String,
    /// Discovery results for generic `apiVersion/Kind` lookups.
    kinds: Arc<RwLock<HashMap<String, Res>>>,
}

static CONNS: Lazy<RwLock<HashMap<String, Conn>>> = Lazy::new(|| RwLock::new(HashMap::new()));
/// Windows using each connection: one window's disconnect must not break another's.
static HOLDERS: Lazy<parking_lot::Mutex<HashMap<String, HashSet<String>>>> = Lazy::new(Default::default);

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

fn kubeconfig_err(e: impl std::fmt::Display) -> AppError {
    AppError::msg(format!("Kubeconfig error: {e}"))
}

/// Loads the kubeconfig from an explicit path (may be a PATH-style list) or the default
/// `KUBECONFIG` / `~/.kube/config` location.
fn load_kubeconfig(path: Option<&str>) -> AppResult<Kubeconfig> {
    let path = path.map(str::trim).filter(|p| !p.is_empty());
    let mut kc = match path {
        Some(p) => {
            let paths: Vec<PathBuf> = std::env::split_paths(p)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| expand_tilde(&p.to_string_lossy()))
                .collect();
            let mut merged = Kubeconfig::default();
            for p in paths {
                merged = merged.merge(Kubeconfig::read_from(&p).map_err(kubeconfig_err)?).map_err(kubeconfig_err)?;
            }
            merged
        }
        None => Kubeconfig::read().map_err(kubeconfig_err)?,
    };
    patch_exec_auth(&mut kc);
    Ok(kc)
}

/// Why DevCat won't connect an agent to the kubeconfig(s) at `path` (a connection's file, several
/// joined like `KUBECONFIG`), if it won't. A kubeconfig can log in by running a program (an `exec`
/// credential plugin, or an `auth-provider`'s `cmd-path`), so one an agent names could run any
/// command without the user's approval: refused unless it's the user's own kubeconfig. And one in
/// a folder the agent writes to (`writable`) is refused too: it's read again on every use, so the
/// agent could add such a program afterwards.
pub(crate) fn agent_kubeconfig_problem(path: &str, writable: &[&Path]) -> Option<String> {
    let defaults = default_kubeconfigs();
    let writable: Vec<PathBuf> = writable.iter().filter_map(|w| std::fs::canonicalize(w).ok()).collect();
    for p in std::env::split_paths(path.trim()).filter(|p| !p.as_os_str().is_empty()) {
        let p = expand_tilde(&p.to_string_lossy());
        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if defaults.contains(&real) {
            continue;
        }
        if writable.iter().any(|w| real.starts_with(w)) {
            return Some(format!(
                "{} is in a folder the agent writes to, where it could later be changed to run a program to log in: keep the kubeconfig elsewhere (e.g. in ~/.kube), or add the connection yourself in DevCat.",
                p.display()
            ));
        }
        let runs = Kubeconfig::read_from(&real)
            .map(|kc| kc.auth_infos.iter().any(|n| n.auth_info.as_ref().is_some_and(|a| a.exec.is_some() || a.auth_provider.is_some())))
            .unwrap_or(false);
        if runs {
            return Some(format!(
                "{} logs in by running a program (an exec plugin or auth-provider), which DevCat doesn't run for an agent: add the connection yourself in DevCat.",
                p.display()
            ));
        }
    }
    None
}

/// The user's own kubeconfigs: `KUBECONFIG`, else `~/.kube/config`.
fn default_kubeconfigs() -> Vec<PathBuf> {
    let list: Vec<PathBuf> = match std::env::var_os("KUBECONFIG") {
        Some(v) if !v.is_empty() => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
        _ => dirs::home_dir().map(|h| vec![h.join(".kube").join("config")]).unwrap_or_default(),
    };
    list.into_iter().map(|p| std::fs::canonicalize(&p).unwrap_or(p)).collect()
}

/// GUI apps on macOS don't inherit the login shell's PATH, so exec credential plugins
/// (gke-gcloud-auth-plugin, aws, kubelogin…) are not found. Build a PATH that includes
/// the usual install locations.
fn augmented_path() -> OsString {
    let mut dirs_list: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    let mut extra: Vec<PathBuf> = vec![
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/usr/local/bin".into(),
        "/usr/local/sbin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
        home.join("google-cloud-sdk/bin"),
        "/opt/homebrew/share/google-cloud-sdk/bin".into(),
        "/usr/local/share/google-cloud-sdk/bin".into(),
        "/opt/homebrew/Caskroom/google-cloud-sdk/latest/google-cloud-sdk/bin".into(),
        "/usr/local/Caskroom/google-cloud-sdk/latest/google-cloud-sdk/bin".into(),
        "/usr/lib/google-cloud-sdk/bin".into(),
        "/snap/bin".into(),
        home.join(".local/bin"),
        home.join("bin"),
        home.join(".krew/bin"),
        home.join("go/bin"),
        home.join(".nix-profile/bin"),
        "/run/current-system/sw/bin".into(),
    ];
    if cfg!(windows) {
        if let Some(local) = dirs::data_local_dir() {
            extra.push(local.join("Google/Cloud SDK/google-cloud-sdk/bin"));
        }
        extra.push("C:/Program Files (x86)/Google/Cloud SDK/google-cloud-sdk/bin".into());
        extra.push("C:/Program Files/Google/Cloud SDK/google-cloud-sdk/bin".into());
    }
    for d in extra {
        if !dirs_list.contains(&d) && d.is_dir() {
            dirs_list.push(d);
        }
    }
    std::env::join_paths(dirs_list).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

fn which_in(cmd: &str, path: &OsString) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    for dir in std::env::split_paths(path) {
        for ext in exts {
            let cand = dir.join(format!("{cmd}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Resolves exec plugin commands to absolute paths and hands them the augmented PATH.
fn patch_exec_auth(kc: &mut Kubeconfig) {
    let path = augmented_path();
    let path_str = path.to_string_lossy().to_string();
    for named in kc.auth_infos.iter_mut() {
        let Some(exec) = named.auth_info.as_mut().and_then(|a| a.exec.as_mut()) else { continue };
        if let Some(cmd) = exec.command.clone() {
            let p = Path::new(&cmd);
            if !p.is_absolute() && p.components().count() == 1 {
                if let Some(found) = which_in(&cmd, &path) {
                    exec.command = Some(found.to_string_lossy().to_string());
                }
            }
        }
        // A GUI app cannot answer prompts; this also makes kube capture the plugin's stderr
        // so failures (e.g. expired gcloud login) can be shown to the user.
        exec.interactive_mode = Some(kube::config::ExecInteractiveMode::Never);
        let env = exec.env.get_or_insert_with(Vec::new);
        if !env.iter().any(|e| e.get("name").is_some_and(|n| n == "PATH")) {
            env.push(HashMap::from([("name".to_string(), "PATH".to_string()), ("value".to_string(), path_str.clone())]));
        }
    }
}

fn opt(cfg: &ConnectionConfig, key: &str) -> Option<String> {
    cfg.options.get(key).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Several dependencies enable both rustls backends (ring and aws-lc-rs), so rustls cannot
/// pick one on its own and panics on first TLS use. Install one process-wide (idempotent).
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub(crate) async fn build_conn(cfg: &ConnectionConfig) -> AppResult<Conn> {
    install_crypto_provider();
    crate::network::ensure_for(cfg).await?;
    let kc = load_kubeconfig(cfg.file_path.as_deref())?;
    let context = opt(cfg, "context")
        .or_else(|| kc.current_context.clone())
        .ok_or_else(|| AppError::msg("Kubeconfig has no current-context; pick a context in the connection settings"))?;
    if !kc.contexts.iter().any(|c| c.name == context) {
        return Err(AppError::msg(format!("Context \"{context}\" not found in kubeconfig")));
    }
    let opts = KubeConfigOptions { context: Some(context.clone()), ..Default::default() };
    let mut config = Config::from_custom_kubeconfig(kc, &opts).await.map_err(kubeconfig_err)?;
    if let Some(ns) = opt(cfg, "namespace") {
        config.default_namespace = ns;
    }
    config.connect_timeout = Some(Duration::from_secs(15));
    let server = config.cluster_url.to_string();
    let namespace = config.default_namespace.clone();
    let client = Client::try_from(config)?;
    Ok(Conn { client, context, server, namespace, kinds: Default::default() })
}

/// Returns the cached client for a connection, building it on first use.
pub(crate) async fn conn(state: &AppState, conn_id: &str) -> AppResult<Conn> {
    if let Some(c) = CONNS.read().await.get(conn_id) {
        return Ok(c.clone());
    }
    let cfg = state.store.get(conn_id).map_err(|_| AppError::NotConnected(conn_id.into()))?;
    let c = build_conn(&cfg).await?;
    CONNS.write().await.insert(conn_id.to_string(), c.clone());
    Ok(c)
}

pub(crate) fn friendly(e: kube::Error) -> AppError {
    const HINT: &str = "If this context uses an exec plugin (e.g. gke-gcloud-auth-plugin), make sure it is installed and you are logged in (e.g. `gcloud auth login`).";
    match e {
        kube::Error::Auth(kube::client::AuthError::AuthExecRun { cmd, status, out }) => {
            // `cmd` embeds KUBERNETES_EXEC_INFO (with CA data); show only the program name.
            let program = cmd.rsplit(' ').next().unwrap_or_default().trim_matches('"').to_string();
            let program = Path::new(&program).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(program);
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let detail = if stderr.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { stderr };
            AppError::msg(format!("Authentication failed: {program} exited with {status}.\n{detail}\n\n{HINT}"))
        }
        kube::Error::Auth(inner) => AppError::msg(format!("Authentication failed: {inner}\n\n{HINT}")),
        other => AppError::Kube(other),
    }
}

/// Test a Kubernetes connection profile; returns the server version.
pub async fn test(cfg: &ConnectionConfig) -> AppResult<String> {
    let c = build_conn(cfg).await?;
    let v = c.client.apiserver_version().await.map_err(friendly)?;
    Ok(format!("Kubernetes {} ({})", v.git_version, c.context))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_contexts(kubeconfig_path: Option<String>) -> AppResult<Vec<String>> {
    let kc = load_kubeconfig(kubeconfig_path.as_deref())?;
    Ok(kc.contexts.into_iter().map(|c| c.name).collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    version: String,
    platform: String,
    context: String,
    server: String,
    namespace: String,
}

/// (Re)connects: always builds a fresh client so this doubles as "retry".
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_connect(window: crate::events::Owner, state: St, conn_id: String) -> AppResult<ConnectInfo> {
    HOLDERS.lock().entry(conn_id.clone()).or_default().insert(window.label().to_string());
    let cfg = state.store.get(&conn_id)?;
    let c = build_conn(&cfg).await?;
    let v = c.client.apiserver_version().await.map_err(friendly)?;
    CONNS.write().await.insert(conn_id, c.clone());
    Ok(ConnectInfo { version: v.git_version, platform: v.platform, context: c.context, server: c.server, namespace: c.namespace })
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_disconnect(window: crate::events::Owner, conn_id: String) -> AppResult<()> {
    let still_used = HOLDERS.lock().get_mut(&conn_id).is_some_and(|h| {
        h.remove(window.label());
        !h.is_empty()
    });
    if still_used {
        // Another window still uses the connection: only stop this window's streams.
        streams::stop_owned_by(window.label(), Some(&conn_id));
    } else {
        drop_conn(&conn_id).await;
    }
    Ok(())
}

/// Forget the client and stop every stream / port-forward of a connection (all windows).
pub(crate) async fn drop_conn(conn_id: &str) {
    HOLDERS.lock().remove(conn_id);
    CONNS.write().await.remove(conn_id);
    streams::stop_all_for(conn_id);
}

/// A window was closed: it no longer holds any connection (port-forwards keep running).
pub fn release_window(label: &str) {
    HOLDERS.lock().retain(|_, h| {
        h.remove(label);
        !h.is_empty()
    });
}

// ---------------------------------------------------------------------------
// Resource kinds
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Res {
    pub ar: ApiResource,
    pub namespaced: bool,
}

/// (plural, group, version, Kind, namespaced)
const BUILTIN: &[(&str, &str, &str, &str, bool)] = &[
    ("pods", "", "v1", "Pod", true),
    ("deployments", "apps", "v1", "Deployment", true),
    ("statefulsets", "apps", "v1", "StatefulSet", true),
    ("daemonsets", "apps", "v1", "DaemonSet", true),
    ("replicasets", "apps", "v1", "ReplicaSet", true),
    ("replicationcontrollers", "", "v1", "ReplicationController", true),
    ("jobs", "batch", "v1", "Job", true),
    ("cronjobs", "batch", "v1", "CronJob", true),
    ("services", "", "v1", "Service", true),
    ("endpoints", "", "v1", "Endpoints", true),
    ("ingresses", "networking.k8s.io", "v1", "Ingress", true),
    ("ingressclasses", "networking.k8s.io", "v1", "IngressClass", false),
    ("networkpolicies", "networking.k8s.io", "v1", "NetworkPolicy", true),
    ("configmaps", "", "v1", "ConfigMap", true),
    ("secrets", "", "v1", "Secret", true),
    ("resourcequotas", "", "v1", "ResourceQuota", true),
    ("limitranges", "", "v1", "LimitRange", true),
    ("persistentvolumeclaims", "", "v1", "PersistentVolumeClaim", true),
    ("persistentvolumes", "", "v1", "PersistentVolume", false),
    ("storageclasses", "storage.k8s.io", "v1", "StorageClass", false),
    ("nodes", "", "v1", "Node", false),
    ("namespaces", "", "v1", "Namespace", false),
    ("events", "", "v1", "Event", true),
    ("horizontalpodautoscalers", "autoscaling", "v2", "HorizontalPodAutoscaler", true),
    ("poddisruptionbudgets", "policy", "v1", "PodDisruptionBudget", true),
    ("priorityclasses", "scheduling.k8s.io", "v1", "PriorityClass", false),
    ("serviceaccounts", "", "v1", "ServiceAccount", true),
    ("roles", "rbac.authorization.k8s.io", "v1", "Role", true),
    ("rolebindings", "rbac.authorization.k8s.io", "v1", "RoleBinding", true),
    ("clusterroles", "rbac.authorization.k8s.io", "v1", "ClusterRole", false),
    ("clusterrolebindings", "rbac.authorization.k8s.io", "v1", "ClusterRoleBinding", false),
    ("customresourcedefinitions", "apiextensions.k8s.io", "v1", "CustomResourceDefinition", false),
];

fn builtin_res(plural: &str, group: &str, version: &str, kind: &str, namespaced: bool) -> Res {
    let api_version = if group.is_empty() { version.to_string() } else { format!("{group}/{version}") };
    Res {
        ar: ApiResource {
            group: group.into(),
            version: version.into(),
            api_version,
            kind: kind.into(),
            plural: plural.into(),
        },
        namespaced,
    }
}

/// Finds a built-in kind by plural ("pods"), Kind ("Pod", case-insensitive) or
/// "apiVersion/Kind" ("apps/v1/Deployment").
fn builtin(kind: &str) -> Option<Res> {
    let lower = kind.to_ascii_lowercase();
    let (av, k) = match lower.rsplit_once('/') {
        Some((av, k)) => (Some(av), k),
        None => (None, lower.as_str()),
    };
    BUILTIN
        .iter()
        .find(|(plural, g, v, kd, _)| {
            let kind_match = *plural == k || kd.to_ascii_lowercase() == k;
            let av_match = match av {
                None => true,
                Some(av) => {
                    let full = if g.is_empty() { v.to_string() } else { format!("{g}/{v}") };
                    full == av
                }
            };
            kind_match && av_match
        })
        .map(|(p, g, v, k, n)| builtin_res(p, g, v, k, *n))
}

/// Resolves a kind string: a built-in name, or `group/version/Kind` / `v1/Kind` via discovery.
pub(crate) async fn resolve(c: &Conn, kind: &str) -> AppResult<Res> {
    if let Some(r) = builtin(kind) {
        return Ok(r);
    }
    if let Some(r) = c.kinds.read().await.get(kind) {
        return Ok(r.clone());
    }
    let (api_version, k) = kind
        .rsplit_once('/')
        .ok_or_else(|| AppError::msg(format!("Unknown resource kind \"{kind}\" (use apiVersion/Kind for custom resources)")))?;
    let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
    let gvk = GroupVersionKind::gvk(group, version, k);
    let (ar, caps) = kube::discovery::pinned_kind(&c.client, &gvk).await?;
    let r = Res { ar, namespaced: caps.scope == Scope::Namespaced };
    c.kinds.write().await.insert(kind.to_string(), r.clone());
    Ok(r)
}

pub(crate) fn dyn_api(c: &Conn, r: &Res, namespace: Option<&str>) -> Api<DynamicObject> {
    match (r.namespaced, namespace.filter(|n| !n.is_empty())) {
        (true, Some(ns)) => Api::namespaced_with(c.client.clone(), ns, &r.ar),
        _ => Api::all_with(c.client.clone(), &r.ar),
    }
}

/// Like `dyn_api` but for single-object operations where a namespaced kind needs a namespace.
fn object_api(c: &Conn, r: &Res, namespace: Option<&str>) -> Api<DynamicObject> {
    if r.namespaced {
        let ns = namespace.filter(|n| !n.is_empty()).unwrap_or(&c.namespace);
        Api::namespaced_with(c.client.clone(), ns, &r.ar)
    } else {
        Api::all_with(c.client.clone(), &r.ar)
    }
}

/// Strips managedFields, fills apiVersion/kind (absent on list items) and optionally
/// redacts secret values.
pub(crate) fn clean(obj: DynamicObject, r: &Res, redact: bool) -> Value {
    let mut v = serde_json::to_value(obj).unwrap_or(Value::Null);
    if let Some(m) = v.as_object_mut() {
        m.insert("apiVersion".into(), Value::String(r.ar.api_version.clone()));
        m.insert("kind".into(), Value::String(r.ar.kind.clone()));
        if let Some(meta) = m.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.remove("managedFields");
            if redact && r.ar.kind == "Secret" && r.ar.group.is_empty() {
                if let Some(ann) = meta.get_mut("annotations").and_then(Value::as_object_mut) {
                    ann.remove("kubectl.kubernetes.io/last-applied-configuration");
                }
            }
        }
        if redact && r.ar.kind == "Secret" && r.ar.group.is_empty() {
            for field in ["data", "stringData"] {
                if let Some(data) = m.get_mut(field).and_then(Value::as_object_mut) {
                    for val in data.values_mut() {
                        *val = Value::Null;
                    }
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------------------
// Basic CRUD commands
// ---------------------------------------------------------------------------

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_namespaces(state: St, conn_id: String) -> AppResult<Vec<String>> {
    let c = conn(&state, &conn_id).await?;
    let api: Api<Namespace> = Api::all(c.client.clone());
    match api.list(&ListParams::default()).await {
        Ok(list) => {
            let mut names: Vec<String> = list.items.into_iter().filter_map(|n| n.metadata.name).collect();
            names.sort();
            Ok(names)
        }
        // Users without cluster-wide namespace list permission still get their default namespace.
        Err(kube::Error::Api(s)) if s.code == 403 => Ok(vec![c.namespace.clone()]),
        Err(e) => Err(friendly(e)),
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_list(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    label_selector: Option<String>,
) -> AppResult<Vec<Value>> {
    let c = conn(&state, &conn_id).await?;
    let r = resolve(&c, &kind).await?;
    let api = dyn_api(&c, &r, namespace.as_deref());
    let mut lp = ListParams::default();
    if let Some(sel) = label_selector.filter(|s| !s.is_empty()) {
        lp = lp.labels(&sel);
    }
    let list = api.list(&lp).await.map_err(friendly)?;
    Ok(list.items.into_iter().map(|o| clean(o, &r, true)).collect())
}

/// Full object as JSON (secret data included — only on explicit request).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_get(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> AppResult<Value> {
    let c = conn(&state, &conn_id).await?;
    let r = resolve(&c, &kind).await?;
    let obj = object_api(&c, &r, namespace.as_deref()).get(&name).await.map_err(friendly)?;
    Ok(clean(obj, &r, false))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_get_yaml(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> AppResult<String> {
    let c = conn(&state, &conn_id).await?;
    let r = resolve(&c, &kind).await?;
    let obj = object_api(&c, &r, namespace.as_deref()).get(&name).await.map_err(friendly)?;
    Ok(serde_yaml::to_string(&clean(obj, &r, false))?)
}

/// Server-side apply of one or more YAML documents (field manager "devcat").
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_apply_yaml(state: St, conn_id: String, yaml: String) -> AppResult<String> {
    apply_yaml(&conn(&state, &conn_id).await?, &yaml).await
}

pub(crate) async fn apply_yaml(c: &Conn, yaml: &str) -> AppResult<String> {
    let mut out = Vec::new();
    // Parse every document up front: serde_yaml's Deserializer is not Send, so it
    // must not be held across the awaits below (Tauri commands need Send futures).
    let docs = serde_yaml::Deserializer::from_str(yaml)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    for mut v in docs {
        if v.is_null() {
            continue;
        }
        let api_version = v["apiVersion"].as_str().unwrap_or_default().to_string();
        let kind = v["kind"].as_str().unwrap_or_default().to_string();
        let name = v["metadata"]["name"].as_str().unwrap_or_default().to_string();
        if api_version.is_empty() || kind.is_empty() || name.is_empty() {
            return Err(AppError::msg("Each document needs apiVersion, kind and metadata.name"));
        }
        let r = resolve(c, &format!("{api_version}/{kind}")).await?;
        let ns = v["metadata"]["namespace"].as_str().map(String::from);
        if let Some(m) = v.as_object_mut() {
            m.remove("status");
            if let Some(meta) = m.get_mut("metadata").and_then(Value::as_object_mut) {
                for k in ["managedFields", "resourceVersion", "creationTimestamp", "generation", "uid"] {
                    meta.remove(k);
                }
            }
        }
        let api = object_api(c, &r, ns.as_deref());
        let pp = PatchParams::apply("devcat").force();
        api.patch(&name, &pp, &Patch::Apply(&v)).await.map_err(friendly)?;
        out.push(format!("{}/{} applied", r.ar.kind.to_lowercase(), name));
    }
    if out.is_empty() {
        return Err(AppError::msg("No documents to apply"));
    }
    Ok(out.join("\n"))
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_delete(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
    force: Option<bool>,
) -> AppResult<()> {
    let c = conn(&state, &conn_id).await?;
    delete(&c, &kind, namespace.as_deref(), &name, force.unwrap_or(false)).await
}

pub(crate) async fn delete(c: &Conn, kind: &str, namespace: Option<&str>, name: &str, force: bool) -> AppResult<()> {
    let r = resolve(c, kind).await?;
    let mut dp = DeleteParams::background();
    if force {
        dp = dp.grace_period(0);
    }
    object_api(c, &r, namespace).delete(name, &dp).await.map_err(friendly)?;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_scale(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
    replicas: i32,
) -> AppResult<()> {
    let c = conn(&state, &conn_id).await?;
    scale(&c, &kind, namespace.as_deref(), &name, replicas).await
}

pub(crate) async fn scale(c: &Conn, kind: &str, namespace: Option<&str>, name: &str, replicas: i32) -> AppResult<()> {
    let r = resolve(c, kind).await?;
    let patch = json!({ "spec": { "replicas": replicas } });
    object_api(c, &r, namespace)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(friendly)?;
    Ok(())
}

/// Rollout restart (same annotation `kubectl rollout restart` uses).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_restart(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> AppResult<()> {
    let c = conn(&state, &conn_id).await?;
    restart(&c, &kind, namespace.as_deref(), &name).await
}

pub(crate) async fn restart(c: &Conn, kind: &str, namespace: Option<&str>, name: &str) -> AppResult<()> {
    let r = resolve(c, kind).await?;
    if !matches!(r.ar.kind.as_str(), "Deployment" | "StatefulSet" | "DaemonSet") {
        return Err(AppError::Unsupported(format!("rollout restart of {}", r.ar.kind)));
    }
    let patch = json!({ "spec": { "template": { "metadata": { "annotations": {
        "kubectl.kubernetes.io/restartedAt": chrono::Utc::now().to_rfc3339()
    } } } } });
    object_api(c, &r, namespace)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(friendly)?;
    Ok(())
}

/// Suspend / resume a CronJob.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_cronjob_suspend(
    state: St,
    conn_id: String,
    namespace: String,
    name: String,
    suspend: bool,
) -> AppResult<()> {
    let c = conn(&state, &conn_id).await?;
    let r = builtin("cronjobs").expect("builtin");
    let patch = json!({ "spec": { "suspend": suspend } });
    object_api(&c, &r, Some(&namespace))
        .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(friendly)?;
    Ok(())
}

/// Events whose involvedObject matches the given object.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_events_for(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> AppResult<Vec<Value>> {
    let c = conn(&state, &conn_id).await?;
    let r = resolve(&c, &kind).await?;
    let ev = builtin("events").expect("builtin");
    let ns = if r.namespaced { namespace } else { None };
    let api = dyn_api(&c, &ev, ns.as_deref());
    let lp = ListParams::default().fields(&format!("involvedObject.name={name},involvedObject.kind={}", r.ar.kind));
    let list = api.list(&lp).await.map_err(friendly)?;
    Ok(list.items.into_iter().map(|o| clean(o, &ev, true)).collect())
}

// ---------------------------------------------------------------------------
// Overview & metrics
// ---------------------------------------------------------------------------

/// Parses a Kubernetes quantity into base units (cores for CPU, bytes for memory).
pub(crate) fn parse_qty(s: &str) -> f64 {
    let s = s.trim();
    if let Ok(v) = s.parse::<f64>() {
        return v;
    }
    let idx = s.find(|ch: char| ch.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, suf) = s.split_at(idx);
    let n: f64 = num.parse().unwrap_or(0.0);
    let mult = match suf {
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        _ => 1.0,
    };
    n * mult
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStat {
    capacity: f64,
    allocatable: f64,
    requests: f64,
    limits: f64,
    /// From metrics-server, if available.
    usage: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    version: String,
    platform: String,
    context: String,
    server: String,
    nodes_total: usize,
    nodes_ready: usize,
    pods_total: usize,
    pods_by_phase: BTreeMap<String, usize>,
    pods_capacity: f64,
    container_restarts: i64,
    deployments_total: usize,
    deployments_ready: usize,
    namespaces: usize,
    cpu: ResourceStat,
    memory: ResourceStat,
    metrics_available: bool,
    warnings: Vec<Value>,
    /// Partial failures (e.g. RBAC forbids listing a kind).
    errors: Vec<String>,
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_cluster_overview(state: St, conn_id: String) -> AppResult<Overview> {
    overview(&conn(&state, &conn_id).await?).await
}

pub(crate) async fn overview(c: &Conn) -> AppResult<Overview> {
    let client = c.client.clone();
    let lp = ListParams::default();
    let nodes_api: Api<Node> = Api::all(client.clone());
    let pods_api: Api<Pod> = Api::all(client.clone());
    let deps_api: Api<Deployment> = Api::all(client.clone());
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let ev_api: Api<CoreEvent> = Api::all(client.clone());
    let warn_lp = ListParams::default().fields("type=Warning");
    let (version, nodes, pods, deps, nss, events, metrics) = tokio::join!(
        client.apiserver_version(),
        nodes_api.list(&lp),
        pods_api.list(&lp),
        deps_api.list(&lp),
        ns_api.list(&lp),
        ev_api.list(&warn_lp),
        node_metrics(c),
    );
    let version = version.map_err(friendly)?;
    let mut errors = Vec::new();
    let mut ok = |name: &str, e: kube::Error| errors.push(format!("{name}: {e}"));

    let mut cpu = ResourceStat::default();
    let mut memory = ResourceStat::default();
    let (mut nodes_total, mut nodes_ready, mut pods_capacity) = (0, 0, 0.0);
    match nodes {
        Ok(list) => {
            nodes_total = list.items.len();
            for n in &list.items {
                let Some(st) = &n.status else { continue };
                if st.conditions.iter().flatten().any(|c| c.type_ == "Ready" && c.status == "True") {
                    nodes_ready += 1;
                }
                if let Some(cap) = &st.capacity {
                    cpu.capacity += cap.get("cpu").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                    memory.capacity += cap.get("memory").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                }
                if let Some(al) = &st.allocatable {
                    cpu.allocatable += al.get("cpu").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                    memory.allocatable += al.get("memory").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                    pods_capacity += al.get("pods").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                }
            }
        }
        Err(e) => ok("nodes", e),
    }

    let mut pods_by_phase = BTreeMap::new();
    let (mut pods_total, mut container_restarts) = (0, 0i64);
    match pods {
        Ok(list) => {
            pods_total = list.items.len();
            for p in &list.items {
                let phase = p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_else(|| "Unknown".into());
                let active = phase != "Succeeded" && phase != "Failed";
                *pods_by_phase.entry(phase).or_insert(0) += 1;
                if let Some(st) = &p.status {
                    container_restarts += st.container_statuses.iter().flatten().map(|c| c.restart_count as i64).sum::<i64>();
                }
                if !active {
                    continue;
                }
                for ctr in p.spec.iter().flat_map(|s| s.containers.iter()) {
                    let Some(res) = &ctr.resources else { continue };
                    if let Some(req) = &res.requests {
                        cpu.requests += req.get("cpu").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                        memory.requests += req.get("memory").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                    }
                    if let Some(lim) = &res.limits {
                        cpu.limits += lim.get("cpu").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                        memory.limits += lim.get("memory").map(|q| parse_qty(&q.0)).unwrap_or(0.0);
                    }
                }
            }
        }
        Err(e) => ok("pods", e),
    }

    let (mut deployments_total, mut deployments_ready) = (0, 0);
    match deps {
        Ok(list) => {
            deployments_total = list.items.len();
            deployments_ready = list
                .items
                .iter()
                .filter(|d| {
                    let want = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
                    let ready = d.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
                    ready >= want
                })
                .count();
        }
        Err(e) => ok("deployments", e),
    }

    let namespaces = match nss {
        Ok(l) => l.items.len(),
        Err(e) => {
            ok("namespaces", e);
            0
        }
    };

    let mut warnings: Vec<Value> = match events {
        Ok(list) => list
            .items
            .into_iter()
            .map(|mut e| {
                e.metadata.managed_fields = None;
                serde_json::to_value(e).unwrap_or(Value::Null)
            })
            .collect(),
        Err(e) => {
            ok("events", e);
            vec![]
        }
    };
    let ts = |v: &Value| {
        v["lastTimestamp"]
            .as_str()
            .or_else(|| v["eventTime"].as_str())
            .or_else(|| v["metadata"]["creationTimestamp"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    warnings.sort_by_key(|v| std::cmp::Reverse(ts(v)));
    warnings.truncate(100);

    let metrics_available = metrics.is_ok();
    if let Ok(items) = metrics {
        cpu.usage = Some(items.iter().map(|m| m.cpu).sum());
        memory.usage = Some(items.iter().map(|m| m.memory).sum());
    }

    Ok(Overview {
        version: version.git_version,
        platform: version.platform,
        context: c.context.clone(),
        server: c.server.clone(),
        nodes_total,
        nodes_ready,
        pods_total,
        pods_by_phase,
        pods_capacity,
        container_restarts,
        deployments_total,
        deployments_ready,
        namespaces,
        cpu,
        memory,
        metrics_available,
        warnings,
        errors,
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ContainerUsage {
    name: String,
    cpu: f64,
    memory: f64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageItem {
    name: String,
    namespace: Option<String>,
    /// cores
    cpu: f64,
    /// bytes
    memory: f64,
    containers: Vec<ContainerUsage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsResult {
    available: bool,
    error: Option<String>,
    items: Vec<UsageItem>,
}

fn metrics_res(kind: &str) -> Res {
    let (k, plural) = if kind == "nodes" { ("NodeMetrics", "nodes") } else { ("PodMetrics", "pods") };
    Res {
        ar: ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: k.into(),
            plural: plural.into(),
        },
        namespaced: kind != "nodes",
    }
}

fn usage_of(v: &Value) -> (f64, f64) {
    (
        v["cpu"].as_str().map(parse_qty).unwrap_or(0.0),
        v["memory"].as_str().map(parse_qty).unwrap_or(0.0),
    )
}

async fn fetch_metrics(c: &Conn, kind: &str, namespace: Option<&str>) -> Result<Vec<UsageItem>, kube::Error> {
    let r = metrics_res(kind);
    let list = dyn_api(c, &r, namespace).list(&ListParams::default()).await?;
    Ok(list
        .items
        .into_iter()
        .map(|o| {
            let name = o.metadata.name.clone().unwrap_or_default();
            let namespace = o.metadata.namespace.clone();
            if kind == "nodes" {
                let (cpu, memory) = usage_of(&o.data["usage"]);
                UsageItem { name, namespace, cpu, memory, containers: vec![] }
            } else {
                let containers: Vec<ContainerUsage> = o.data["containers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|ct| {
                        let (cpu, memory) = usage_of(&ct["usage"]);
                        ContainerUsage { name: ct["name"].as_str().unwrap_or_default().to_string(), cpu, memory }
                    })
                    .collect();
                UsageItem {
                    name,
                    namespace,
                    cpu: containers.iter().map(|x| x.cpu).sum(),
                    memory: containers.iter().map(|x| x.memory).sum(),
                    containers,
                }
            }
        })
        .collect())
}

async fn node_metrics(c: &Conn) -> Result<Vec<UsageItem>, kube::Error> {
    fetch_metrics(c, "nodes", None).await
}

/// metrics.k8s.io usage. Returns `available: false` when metrics-server is missing.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_metrics(
    state: St,
    conn_id: String,
    kind: String,
    namespace: Option<String>,
) -> AppResult<MetricsResult> {
    let c = conn(&state, &conn_id).await?;
    if kind != "nodes" && kind != "pods" {
        return Err(AppError::msg("metrics kind must be \"nodes\" or \"pods\""));
    }
    Ok(match fetch_metrics(&c, &kind, namespace.as_deref()).await {
        Ok(items) => MetricsResult { available: true, error: None, items },
        Err(e) => MetricsResult { available: false, error: Some(e.to_string()), items: vec![] },
    })
}

/// Writes text to a user-chosen path (log download). The fs plugin's default capability
/// does not allow arbitrary writes, so the save dialog result is written here.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn k8s_save_text(path: String, contents: String) -> AppResult<()> {
    tokio::fs::write(path, contents).await?;
    Ok(())
}
