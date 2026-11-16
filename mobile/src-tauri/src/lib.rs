//! DevCat Mobile: a remote control for DevCat on your computer. The phone keeps only its key
//! and the list of paired computers; every request goes to a computer and runs there.
//!
//! Commands (all errors are user-facing strings):
//!   m_info, m_set_name, m_machines, m_pair, m_call, m_connect, m_disconnect, m_forget,
//!   m_set_addrs, m_network_changed, m_resume, m_download, m_visible, m_background, m_background_set,
//!   m_battery_settings
//! Events: `m-conn` ({ id, state: connecting|online|offline, error? }),
//!         `m-event` ({ id, event, payload }), `m-pair` ({ stage, hostName }),
//!         `m-transfer` ({ path, done, total }), `m-path` ({ id, path: { direct, rttMs, relay? } | null }).
pub mod client;
pub mod store;
pub mod watch;

use client::{Client, Hello};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use store::{Machine, Store};
use tauri::{Emitter, Manager, State};
use tauri_plugin_devcat_keepalive::KeepAliveExt;
use watch::Watch;

pub fn platform() -> &'static str {
    if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        std::env::consts::OS
    }
}

fn default_name() -> String {
    match platform() {
        "ios" => "iPhone".into(),
        "android" => "Android phone".into(),
        os => format!("DevCat Mobile ({os})"),
    }
}

pub struct App {
    pub client: Arc<Client>,
    pub store: Arc<Store>,
}

#[tauri::command]
fn m_info(app: State<'_, App>, handle: tauri::AppHandle) -> Value {
    let h = app.client.hello();
    json!({
        "deviceId": app.client.device_id(),
        "deviceName": h.name,
        "platform": platform(),
        "version": handle.package_info().version.to_string(),
        "online": app.client.online(),
    })
}

#[tauri::command]
fn m_set_name(app: State<'_, App>, name: String) -> Result<(), String> {
    let name: String = name.trim().chars().take(60).collect();
    if name.is_empty() {
        return Err("Enter a name".into());
    }
    let mut p = app.store.prefs();
    p.device_name = Some(name.clone());
    app.store.save_prefs(&p)?;
    app.client.set_name(name);
    Ok(())
}

#[tauri::command]
fn m_machines(app: State<'_, App>) -> Vec<Machine> {
    app.store.machines()
}

#[tauri::command]
async fn m_pair(app: State<'_, App>, link: String, addr: Option<String>) -> Result<Machine, String> {
    app.client.pair(&link, addr).await
}

#[tauri::command]
async fn m_connect(app: State<'_, App>, id: String) -> Result<Value, String> {
    let s = app.client.session(&id).await?;
    Ok(json!({ "host": s.host, "device": s.device }))
}

#[tauri::command]
async fn m_call(app: State<'_, App>, id: String, cmd: String, args: Option<Value>) -> Result<Value, Value> {
    app.client.call(&id, &cmd, args.unwrap_or(Value::Null)).await
}

#[tauri::command]
fn m_disconnect(app: State<'_, App>, id: String) {
    app.client.disconnect(&id);
}

#[tauri::command]
fn m_forget(app: State<'_, App>, watch: State<'_, Arc<Watch>>, id: String) -> Result<Vec<Machine>, String> {
    app.client.forget(&id)?;
    watch.forget(&id);
    Ok(app.store.machines())
}

/// Addresses the user typed for a computer (tried before the ones it reports).
#[tauri::command]
fn m_set_addrs(app: State<'_, App>, id: String, addrs: Vec<String>) -> Result<Vec<Machine>, String> {
    let manual: Vec<String> = addrs.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    app.store.update(&id, |m| {
        let reported: Vec<String> = m.addrs.iter().filter(|a| !m.manual_addrs.contains(a)).cloned().collect();
        m.addrs = store::merged_addrs(&manual, &reported);
        m.manual_addrs = manual;
    })?;
    app.client.disconnect(&id);
    Ok(app.store.machines())
}

/// Back on screen: keep the connections that still answer (client.rs `resume`).
#[tauri::command]
async fn m_resume(app: State<'_, App>) -> Result<(), String> {
    app.client.resume().await;
    Ok(())
}

#[tauri::command]
async fn m_network_changed(app: State<'_, App>) -> Result<(), String> {
    app.client.network_changed().await;
    Ok(())
}

/// The web page is on screen or not (notifications only when it isn't: on screen it shows toasts).
#[tauri::command]
fn m_visible(watch: State<'_, Arc<Watch>>, visible: bool) {
    watch.set_visible(visible);
}

/// Background connection (Android): whether it's possible, on, and free of battery
/// optimization. Starts the service when it's on and isn't running yet (the app is on screen:
/// the page asks when it starts). Async: the plugin call needs the main thread free.
#[tauri::command]
async fn m_background(handle: tauri::AppHandle, app: State<'_, App>, watch: State<'_, Arc<Watch>>) -> Result<Value, String> {
    let ka = handle.keep_alive();
    // nothing to stay connected to before a computer is paired
    if ka.supported() && watch.keep() && !app.store.machines().is_empty() {
        ka.start()?;
    }
    Ok(json!({ "supported": ka.supported(), "on": watch.keep(), "batteryUnrestricted": ka.battery_unrestricted() }))
}

#[tauri::command]
async fn m_background_set(handle: tauri::AppHandle, app: State<'_, App>, watch: State<'_, Arc<Watch>>, on: bool) -> Result<Value, String> {
    let ka = handle.keep_alive();
    if !ka.supported() {
        return Err("Staying connected in the background is only for Android".into());
    }
    if on { ka.start()? } else { ka.stop()? }
    let mut p = app.store.prefs();
    p.background = Some(on);
    app.store.save_prefs(&p)?;
    watch.set_keep(on);
    Ok(json!({ "supported": true, "on": on, "batteryUnrestricted": ka.battery_unrestricted() }))
}

#[tauri::command]
async fn m_battery_settings(handle: tauri::AppHandle) -> Result<(), String> {
    handle.keep_alive().open_battery_settings()
}

/// Where a downloaded file goes: iOS keeps it in DevCat's folder of the Files app (Documents,
/// shared through Info.ios.plist); elsewhere the system save dialog asks. `None`: cancelled.
async fn save_target(handle: &tauri::AppHandle, name: &str) -> Result<Option<(std::fs::File, String)>, String> {
    if cfg!(target_os = "ios") {
        let dir = handle.path().document_dir().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (name.to_string(), String::new()),
        };
        let mut path = dir.join(name);
        for n in 2.. {
            if !path.exists() {
                break;
            }
            path = dir.join(format!("{stem} ({n}){ext}"));
        }
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let shown = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Some((file, format!("Files › On My iPhone › DevCat › {shown}"))));
    }
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle.dialog().file().set_file_name(name).save_file(move |p| {
        let _ = tx.send(p);
    });
    let Some(dest) = rx.await.ok().flatten() else { return Ok(None) };
    let shown = match &dest {
        FilePath::Path(p) => p.display().to_string(),
        FilePath::Url(_) => name.to_string(),
    };
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    let file = handle.fs().open(dest, opts).map_err(|e| e.to_string())?;
    Ok(Some((file, shown)))
}

/// Copy a file from a computer to the phone, piece by piece. Returns where it went (`None`:
/// the user cancelled the save dialog).
#[tauri::command]
async fn m_download(app: State<'_, App>, handle: tauri::AppHandle, id: String, path: String, name: String) -> Result<Option<String>, String> {
    use base64::Engine;
    use std::io::Write;
    let Some((mut out, place)) = save_target(&handle, &name).await? else { return Ok(None) };
    let mut offset = 0u64;
    loop {
        let r = app.client.call(&id, "fs_download", json!({ "path": path, "offset": offset, "len": 1024 * 1024 })).await.map_err(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()))?;
        let data = base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or_default()).map_err(|_| "The download got garbled; try again".to_string())?;
        out.write_all(&data).map_err(|e| e.to_string())?;
        offset += data.len() as u64;
        let _ = handle.emit("m-transfer", json!({ "path": path, "done": offset, "total": r["size"].as_u64().unwrap_or(offset) }));
        if data.is_empty() || r["eof"].as_bool().unwrap_or(true) {
            break;
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;
    Ok(Some(place))
}

/// The same commands without Tauri, for the browser bridge (examples/bridge.rs).
pub async fn bridge_call(app: &App, version: &str, cmd: &str, args: Value) -> Result<Value, Value> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let req = |k: &str| s(k).ok_or_else(|| json!(format!("missing {k}")));
    let ok = |v: Result<Value, String>| v.map_err(Value::String);
    match cmd {
        "m_info" => {
            let h = app.client.hello();
            Ok(json!({ "deviceId": app.client.device_id(), "deviceName": h.name, "platform": platform(), "version": version, "online": app.client.online() }))
        }
        "m_set_name" => {
            let name = req("name")?;
            let mut p = app.store.prefs();
            p.device_name = Some(name.clone());
            app.store.save_prefs(&p).map_err(Value::String)?;
            app.client.set_name(name);
            Ok(Value::Null)
        }
        "m_machines" => Ok(json!(app.store.machines())),
        "m_pair" => ok(app.client.pair(&req("link")?, s("addr")).await.map(|m| json!(m))),
        "m_connect" => ok(app.client.session(&req("id")?).await.map(|s| json!({ "host": s.host, "device": s.device }))),
        "m_call" => app.client.call(&req("id")?, &req("cmd")?, args.get("args").cloned().unwrap_or(Value::Null)).await,
        "m_disconnect" => {
            app.client.disconnect(&req("id")?);
            Ok(Value::Null)
        }
        "m_forget" => {
            app.client.forget(&req("id")?).map_err(Value::String)?;
            Ok(json!(app.store.machines()))
        }
        "m_set_addrs" => {
            let id = req("id")?;
            let manual: Vec<String> = serde_json::from_value(args["addrs"].clone()).unwrap_or_default();
            app.store
                .update(&id, |m| {
                    let reported: Vec<String> = m.addrs.iter().filter(|a| !m.manual_addrs.contains(a)).cloned().collect();
                    m.addrs = store::merged_addrs(&manual, &reported);
                    m.manual_addrs = manual.clone();
                })
                .map_err(Value::String)?;
            app.client.disconnect(&id);
            Ok(json!(app.store.machines()))
        }
        "m_network_changed" => {
            app.client.network_changed().await;
            Ok(Value::Null)
        }
        "m_resume" => {
            app.client.resume().await;
            Ok(Value::Null)
        }
        _ => Err(json!(format!("unknown command {cmd}"))),
    }
}

/// A phone-side app outside Tauri (browser bridge, tests).
pub fn headless(dir: std::path::PathBuf, name: &str, sink: client::Sink) -> Result<App, String> {
    let store = Arc::new(Store::new(dir));
    let name = store.prefs().device_name.unwrap_or_else(|| name.to_string());
    let client = Client::new(store.clone(), sink, Hello { name, platform: "web".into(), app_version: "dev".into() })?;
    Ok(App { client, store })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg_attr(not(mobile), allow(unused_mut))]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        // devcat://pair?… links (from the web page's "Open in DevCat app") open the pairing screen.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_devcat_keepalive::init());
    #[cfg(mobile)]
    {
        builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    }
    builder
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let store = Arc::new(Store::new(dir));
            let handle = app.handle().clone();
            let shown = handle.clone();
            let notifier: watch::Notifier = Arc::new(move |title, body| {
                use tauri_plugin_notification::NotificationExt;
                // devcat_notify: the status bar icon from the keepalive plugin (Android).
                let _ = shown.notification().builder().title(title).body(body).icon("devcat_notify").show();
            });
            let watch = Watch::new(store.clone(), notifier);
            // The sink comes before the client; the watcher reaches the client through this.
            let client_cell: Arc<OnceLock<Arc<Client>>> = Arc::default();
            let (w, cell) = (watch.clone(), client_cell.clone());
            let sink: client::Sink = Arc::new(move |event, payload| {
                match event {
                    "m-event" => {
                        if let (Some(id), Some(ev)) = (payload["id"].as_str(), payload["event"].as_str()) {
                            w.on_event(id, ev, &payload["payload"]);
                        }
                    }
                    "m-conn" if payload["state"] == "online" => {
                        if let (Some(id), Some(c)) = (payload["id"].as_str(), cell.get()) {
                            w.connected(c.clone(), id.to_string());
                        }
                    }
                    _ => {}
                }
                let _ = handle.emit(event, payload);
            });
            let name = store.prefs().device_name.unwrap_or_else(default_name);
            let hello = Hello { name, platform: platform().into(), app_version: app.package_info().version.to_string() };
            let client = Client::new(store.clone(), sink, hello)?;
            let _ = client_cell.set(client.clone());
            tauri::async_runtime::spawn(watch.clone().keep_connected(client.clone()));
            app.manage(watch);
            app.manage(App { client, store });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![m_info, m_set_name, m_machines, m_pair, m_connect, m_call, m_disconnect, m_forget, m_set_addrs, m_network_changed, m_resume, m_download, m_visible, m_background, m_background_set, m_battery_settings])
        .run(tauri::generate_context!())
        .expect("error while running DevCat Mobile");
}
