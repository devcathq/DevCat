//! The reviewed page, in a native child webview of the DevCat window (`review-<id>`) placed over
//! the review tab's page area. It gets no DevCat capability (only `devcat-review|msg`, see
//! `channel`), its own browser data store, and the inspector as its initialization script.

use super::{channel, review_store, serve, Review, Target};
use crate::error::{AppError, AppResult};
use parking_lot::Mutex;
use reqwest::Url;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Emitter, EventTarget, LogicalPosition, LogicalSize, Manager, Webview, WebviewBuilder, WebviewUrl, Window, Wry};

/// The review browser data store on macOS 14+ (`devcat-review-p1`, a UUID's 16 bytes).
#[cfg(target_os = "macos")]
pub const DATA_STORE_ID: [u8; 16] = *b"devcat-review-p1";

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// DevCat's data folder (Windows / Linux keep the review browser data in `<data>/review-profile`).
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// Where Windows / Linux keep the review browser data.
#[cfg(not(target_os = "macos"))]
pub fn profile_dir() -> Option<PathBuf> {
    DATA_DIR.get().map(|d| d.join("review-profile"))
}

pub fn label(id: &str) -> String {
    format!("review-{id}")
}

/// DevCat's own dev server (debug builds), a local origin to Tauri.
pub fn dev_url(app: &AppHandle) -> Option<Url> {
    if cfg!(debug_assertions) { app.config().build.dev_url.clone() } else { None }
}

/// May the review webview (or a frame in it: macOS asks for every frame) go there? Web pages and
/// the inert `about:` / `data:` / `blob:` documents frames use, never a local origin.
pub fn may_navigate(url: &Url, dev: Option<&Url>) -> bool {
    matches!(url.scheme(), "http" | "https" | "about" | "data" | "blob") && !channel::is_local_target(url, dev)
}

/// May a page open there as the top-level page (a link or `window.open` asking for a new window)?
fn may_open(url: &Url, dev: Option<&Url>) -> bool {
    matches!(url.scheme(), "http" | "https") && !channel::is_local_target(url, dev)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectorConfig<'a> {
    origin: String,
    review: &'a str,
    secret: &'a str,
    notes: &'a [super::ReviewNote],
    page_prefix: String,
}

/// The inspector with this review's settings: `(function (CONFIG) {…})(<json>);`.
pub fn inspector_script(r: &Review, file_origin: Option<&str>) -> String {
    let (origin, page_prefix) = match &r.doc.target {
        Target::Url { origin } => (origin.clone(), String::new()),
        Target::File { .. } => (file_origin.unwrap_or_default().to_string(), format!("/f/{}/", r.id)),
    };
    let config = InspectorConfig { origin, review: &r.id, secret: &r.secret, notes: &r.doc.notes, page_prefix };
    let json = serde_json::to_string(&config).unwrap_or_else(|_| "null".into());
    format!("{}({json});", include_str!("inspector.js").trim_end())
}

/// Calls `window.__devcatReview.<method>(<arg>)` in the page, if the inspector is there. `arg` is
/// JSON, so nothing from a note can break out of it. Nothing is sent while the webview shows any
/// page but the review's own (`channel::may_tell_page`): another site could define the global.
pub fn call(webview: &Webview, r: &Review, method: &str, arg: &serde_json::Value) -> AppResult<()> {
    if !channel::may_tell_page(r, webview.label(), webview.url().ok().as_ref(), serve::running_origin().as_deref()) {
        return Ok(());
    }
    webview
        .eval(format!("window.__devcatReview && window.__devcatReview.{method}({arg})"))
        .map_err(|e| AppError::Msg(format!("The review page didn't answer: {e}")))
}

/// `review://new-window`: the page asked to open `url` in a new window; the tab opens a new review.
#[derive(Serialize, Clone)]
struct NewWindow {
    id: String,
    url: String,
}

/// The shortest time between two new review tabs one page may open.
const NEW_TAB_EVERY: Duration = Duration::from_secs(1);

/// `review://page`, when a page finished loading.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PageLoaded {
    id: String,
    url: String,
    inspectable: bool,
}

#[cfg(target_os = "macos")]
fn data_store(b: WebviewBuilder<Wry>) -> WebviewBuilder<Wry> {
    // below macOS 14 wry would use DevCat's own store instead: keep nothing there
    if macos_major().is_some_and(|m| m >= 14) { b.data_store_identifier(DATA_STORE_ID) } else { b.incognito(true) }
}

#[cfg(not(target_os = "macos"))]
fn data_store(b: WebviewBuilder<Wry>) -> WebviewBuilder<Wry> {
    match profile_dir() {
        Some(dir) => b.data_directory(dir),
        None => b.incognito(true),
    }
}

/// macOS's major version (`kern.osproductversion`).
#[cfg(target_os = "macos")]
pub fn macos_major() -> Option<u32> {
    let name = c"kern.osproductversion";
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: `buf` and `len` describe a writable buffer; `name` is NUL-terminated.
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..len.min(buf.len())]).ok()?;
    major_of(s.trim_end_matches('\0'))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn major_of(version: &str) -> Option<u32> {
    version.split('.').next()?.trim().parse().ok()
}

/// Creates the review's webview in `window`, at the given place (logical px), loading the
/// review's first page.
pub fn create(window: &Window, r: &Review, pos: LogicalPosition<f64>, size: LogicalSize<f64>) -> AppResult<Webview> {
    let app = window.app_handle().clone();
    let url = Url::parse(&r.load_url).map_err(|_| AppError::Msg("The review has no page to open".into()))?;
    let dev = dev_url(&app);
    let dev_new = dev.clone();
    let wv_label = label(&r.id);
    let new_id = r.id.clone();
    let last_new: Mutex<Option<Instant>> = Mutex::new(None);
    let open_app = app.clone();
    let script = inspector_script(r, serve::running_origin().as_deref());
    let id = r.id.clone();
    let builder = WebviewBuilder::new(wv_label, WebviewUrl::External(url))
        .initialization_script(script)
        .on_navigation(move |u| may_navigate(u, dev.as_ref()))
        // a link or `window.open` asking for a new window opens a new review tab, as a browser
        // opens a new tab (same review browser data, so sign-ins carry over); at most one a
        // second, so a page can't flood the window with tabs
        .on_new_window(move |u, _| {
            let mut last = last_new.lock();
            if may_open(&u, dev_new.as_ref()) && last.is_none_or(|t| t.elapsed() >= NEW_TAB_EVERY) {
                *last = Some(Instant::now());
                if let Some(r) = review_store().get(&new_id) {
                    let ev = NewWindow { id: r.id.clone(), url: u.to_string() };
                    let _ = open_app.emit_to(EventTarget::webview_window(r.window.as_str()), "review://new-window", ev);
                }
            }
            NewWindowResponse::Deny
        })
        .on_page_load(move |wv, p| {
            if p.event() != PageLoadEvent::Finished {
                return;
            }
            let Some(r) = review_store().get(&id) else { return };
            let inspectable = channel::caller_allowed(&r, wv.label(), p.url(), serve::running_origin().as_deref());
            let ev = PageLoaded { id: r.id.clone(), url: p.url().to_string(), inspectable };
            let _ = wv.emit_to(EventTarget::webview_window(r.window.as_str()), "review://page", ev);
        });
    window
        .add_child(data_store(builder), pos, size)
        .map_err(|e| AppError::Msg(format!("Couldn't open the review page: {e}")))
}

/// The review's webview, if it has been created.
pub fn find(app: &AppHandle, id: &str) -> Option<Webview> {
    app.get_webview(&label(id))
}

/// A closed window ends its reviews and closes their webviews.
pub fn on_window_destroyed(window: &str) {
    let ids = review_store().close_window(window);
    let Some(app) = crate::events::app() else { return };
    for id in ids {
        if let Some(wv) = find(app, &id) {
            let _ = wv.close();
        }
    }
}

/// Clears the review browser data (cookies, storage, caches): through an open review webview,
/// else the store itself.
pub async fn clear_data(app: &AppHandle) -> AppResult<()> {
    let open: Vec<Webview> = app.webviews().into_values().filter(|w| w.label().starts_with("review-")).collect();
    if !open.is_empty() {
        for wv in open {
            wv.clear_all_browsing_data().map_err(|e| AppError::Msg(format!("Couldn't clear the review browsing data: {e}")))?;
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        // below 14 review webviews keep nothing (incognito)
        if macos_major().is_some_and(|m| m >= 14) {
            let ids = app.fetch_data_store_identifiers().await.unwrap_or_default();
            if ids.contains(&DATA_STORE_ID) {
                app.remove_data_store(DATA_STORE_ID)
                    .await
                    .map_err(|e| AppError::Msg(format!("Couldn't clear the review browsing data: {e}")))?;
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        match profile_dir() {
            Some(dir) if dir.exists() => std::fs::remove_dir_all(&dir)
                .map_err(|e| AppError::Msg(format!("Couldn't clear the review browsing data ({e}); try again in a moment"))),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ReviewStore, Target};
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn navigation_stays_on_web_pages() {
        let dev = u("http://localhost:1420/");
        for ok in ["https://example.com/x", "http://localhost:5173/", "about:blank", "about:srcdoc", "data:text/html,x", "blob:https://a.b/1"] {
            assert!(may_navigate(&u(ok), Some(&dev)), "{ok}");
        }
        for bad in ["http://localhost:1420/", "tauri://localhost", "ipc://localhost/x", "file:///etc/passwd", "javascript:alert(1)", "http://asset.localhost/x"] {
            assert!(!may_navigate(&u(bad), Some(&dev)), "{bad}");
        }
        assert!(may_open(&u("https://a.b/"), None));
        assert!(!may_open(&u("about:blank"), None));
        assert!(!may_open(&u("http://localhost:1420/"), Some(&dev)));
    }

    #[test]
    fn inspector_gets_its_config() {
        let s = ReviewStore::default();
        let r = s.start(Target::File { root: "/tmp/x".into(), rel: "a.html".into() }, "a.html".into(), None, "main".into());
        let js = inspector_script(&r, Some("http://127.0.0.1:4000"));
        assert!(js.starts_with("(function (CONFIG) {"));
        let tail = &js[js.rfind("})(").unwrap() + 3..];
        let cfg: serde_json::Value = serde_json::from_str(tail.trim_end_matches(");")).unwrap();
        assert_eq!(cfg["origin"], "http://127.0.0.1:4000");
        assert_eq!(cfg["pagePrefix"], format!("/f/{}/", r.id));
        assert_eq!(cfg["secret"], r.secret.as_str());
        assert_eq!(cfg["notes"], serde_json::json!([]));
        let r = s.start(Target::Url { origin: "https://a.b".into() }, "/".into(), None, "main".into());
        let js = inspector_script(&r, None);
        assert!(js.contains(r#""origin":"https://a.b""#) && js.contains(r#""pagePrefix":"""#));
    }

    #[test]
    fn macos_versions() {
        assert_eq!(major_of("14.5"), Some(14));
        assert_eq!(major_of("26.0.1"), Some(26));
        assert_eq!(major_of("10.16"), Some(10));
        assert_eq!(major_of(""), None);
        #[cfg(target_os = "macos")]
        assert!(macos_major().is_some_and(|m| m >= 11));
    }
}
