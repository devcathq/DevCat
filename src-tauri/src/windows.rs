//! Multiple app windows. Every window runs its own copy of the frontend (own tabs, own
//! project focus); backend sessions (connections, port-forwards, tunnels) are shared.
//! Terminals, Claude runs and Kubernetes log / watch / exec streams belong to the window
//! that started them and are stopped when it closes.
use crate::error::{AppError, AppResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

/// Offset of a new window from the one that opened it.
const CASCADE: f64 = 28.0;

static NEXT: AtomicUsize = AtomicUsize::new(1);

/// Frontend URL for a new window: `focus` = project to focus, `tab` = tab spec to open.
fn window_url(project_id: Option<&str>, tab: Option<&serde_json::Value>) -> String {
    let mut url = tauri::Url::parse("http://localhost/index.html").expect("static url");
    {
        let mut q = url.query_pairs_mut();
        if let Some(p) = project_id.filter(|p| !p.is_empty()) {
            q.append_pair("focus", p);
        }
        if let Some(t) = tab.filter(|t| !t.is_null()) {
            q.append_pair("tab", &t.to_string());
        }
    }
    match url.query().filter(|q| !q.is_empty()) {
        Some(q) => format!("index.html?{q}"),
        None => "index.html".into(),
    }
}

fn next_label<R: Runtime>(app: &AppHandle<R>) -> String {
    loop {
        let label = format!("win-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        if app.get_webview_window(&label).is_none() {
            return label;
        }
    }
}

/// Open another app window (same size / behavior as the main one), cascaded from the caller.
/// Returns the new window's label. Async so the builder doesn't deadlock on Windows.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn open_window(
    app: AppHandle,
    window: tauri::Window,
    project_id: Option<String>,
    tab: Option<serde_json::Value>,
) -> AppResult<String> {
    create(&app, Some(&window), project_id.as_deref(), tab.as_ref())
}

pub fn create<R: Runtime>(
    app: &AppHandle<R>,
    from: Option<&tauri::Window<R>>,
    project_id: Option<&str>,
    tab: Option<&serde_json::Value>,
) -> AppResult<String> {
    let label = next_label(app);
    let url = WebviewUrl::App(window_url(project_id, tab).into());
    let mut b = WebviewWindowBuilder::new(app, &label, url)
        .title("DevCat")
        .inner_size(1440.0, 900.0)
        .min_inner_size(960.0, 600.0)
        // See-through when Appearance → Window glass is on (the page paints a solid background otherwise).
        .transparent(true)
        .disable_drag_drop_handler();
    // Cascade from the calling window (or the focused one, e.g. for the app menu).
    let origin = from.cloned().or_else(|| {
        let mut wins = app.webview_windows().into_values();
        wins.find(|w| w.is_focused().unwrap_or(false)).map(|w| w.as_ref().window())
    });
    match origin.and_then(|w| Some((w.outer_position().ok()?, w.scale_factor().ok()?))) {
        Some((pos, scale)) => {
            let pos = pos.to_logical::<f64>(scale);
            b = b.position(pos.x + CASCADE, pos.y + CASCADE);
        }
        None => b = b.center(),
    }
    b.build().map_err(|e| AppError::msg(format!("Could not open a window: {e}")))?;
    Ok(label)
}

/// A window was closed: stop what it started.
pub fn on_destroyed(label: &str) {
    crate::agent::close_ghosts_of(label);
    crate::claude::pty::close_owned_by(label);
    crate::infra::k8s::streams::stop_owned_by(label, None);
    crate::infra::k8s::release_window(label);
    let label = label.to_string();
    crate::rt::spawn(async move { crate::claude::runner::stop_owned_by(&label).await });
}

/// macOS: add "New Window ⇧⌘N" to the File menu of the default app menu (Edit stays intact).
#[cfg(target_os = "macos")]
pub fn install_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::default(app)?;
    // The app menu ("DevCat", the first submenu): Check for Updates… right after About.
    if let Some(app_menu) = menu.items()?.into_iter().find_map(|i| i.as_submenu().cloned()) {
        let item = MenuItem::with_id(app, CHECK_UPDATES_MENU_ID, "Check for Updates…", true, None::<&str>)?;
        app_menu.insert(&item, 1)?;
        app_menu.insert(&PredefinedMenuItem::separator(app)?, 2)?;
    }
    let file = menu.items()?.into_iter().filter_map(|i| i.as_submenu().cloned()).find(|s| s.text().is_ok_and(|t| t == "File"));
    if let Some(file) = file {
        let item = MenuItem::with_id(app, NEW_WINDOW_MENU_ID, "New Window", true, Some("CmdOrCtrl+Shift+N"))?;
        file.insert(&item, 0)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, e| {
        if e.id() == NEW_WINDOW_MENU_ID {
            if let Err(err) = create(app, None, None, None) {
                eprintln!("{err}");
            }
        } else if e.id() == CHECK_UPDATES_MENU_ID {
            // Only one window shows the update dialog: the focused one (else the main window).
            use tauri::{Emitter, EventTarget, Manager};
            let windows = app.webview_windows();
            let label = windows
                .iter()
                .find(|(_, w)| w.is_focused().unwrap_or(false))
                .map(|(l, _)| l.clone())
                .or_else(|| windows.contains_key("main").then(|| "main".to_string()))
                .or_else(|| windows.keys().next().cloned());
            if let Some(label) = label {
                let _ = app.emit_to(EventTarget::webview_window(label), "menu-check-updates", ());
            }
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
const NEW_WINDOW_MENU_ID: &str = "devcat-new-window";
#[cfg(target_os = "macos")]
const CHECK_UPDATES_MENU_ID: &str = "devcat-check-updates";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_query() {
        assert_eq!(window_url(None, None), "index.html");
        assert_eq!(window_url(Some(""), Some(&serde_json::Value::Null)), "index.html");
        let u = window_url(Some("p 1"), Some(&serde_json::json!({ "type": "query", "connId": "a&b" })));
        assert!(u.starts_with("index.html?focus=p+1&tab="), "{u}");
        let parsed = tauri::Url::parse(&format!("http://x/{u}")).unwrap();
        let tab = parsed.query_pairs().find(|(k, _)| k == "tab").unwrap().1;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&tab).unwrap()["connId"], "a&b");
    }
}
