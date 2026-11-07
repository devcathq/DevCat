//! The one path from the inspector in a reviewed page to Rust: the inline plugin `devcat-review`
//! with its single command `msg`, made reachable from remote pages by a runtime capability for the
//! `review-*` webviews only. See the spec's "Channel (decided)".

use super::{Review, Target};
use reqwest::Url;

/// A URL Tauri would treat as a local origin, which gets every DevCat command: never review one
/// (checked when a review starts and on every navigation). Tauri's own `tauri:` / `ipc:`, `asset:`
/// and any scheme the app registers are local, as is `http(s)://<scheme>.localhost` (how Windows
/// serves those schemes); we can't know every registered scheme here, so any scheme but http(s)
/// and the inert `about:` / `data:` / `blob:` counts, and so does every `*.localhost` host (but not
/// plain `localhost`, which dev servers use). `dev_url` is DevCat's own `devUrl` (debug builds).
pub fn is_local_target(url: &Url, dev_url: Option<&Url>) -> bool {
    if dev_url.is_some_and(|dev| dev.origin().is_tuple() && dev.origin() == url.origin()) {
        return true;
    }
    match url.scheme() {
        "http" | "https" => url.host_str().is_some_and(|h| h.trim_end_matches('.').ends_with(".localhost")),
        "about" | "data" | "blob" => false,
        _ => true,
    }
}

/// Is the calling webview this review's webview, on the review's origin? Checked from what Rust
/// knows, never from what the page says: the webview's label and its top-level URL (the native
/// webview's `url()`, not the URL of the frame that called). Frames can't call at all: Tauri's
/// IPC and invoke-key scripts run in the main frame only. For a file review, the file server's
/// origin and this review's `/f/<id>/` folder; `file_origin` is the file server's origin when
/// it's running.
pub fn caller_allowed(review: &Review, webview_label: &str, webview_url: &Url, file_origin: Option<&str>) -> bool {
    if webview_label != format!("review-{}", review.id) {
        return false;
    }
    let same_origin = |origin: &str| {
        Url::parse(origin).is_ok_and(|o| o.origin().is_tuple() && o.origin() == webview_url.origin())
    };
    match &review.doc.target {
        Target::Url { origin } => same_origin(origin),
        Target::File { .. } => {
            file_origin.is_some_and(same_origin) && webview_url.path().starts_with(&format!("/f/{}/", review.id))
        }
    }
}

#[cfg(feature = "gui")]
pub use gui::{add_capability, emit_changed, plugin};

#[cfg(feature = "gui")]
mod gui {
    use super::super::{no_review, review_store, serve, Review, ReviewDoc, ReviewNote};
    use super::caller_allowed;
    use crate::error::AppResult;
    use serde::Serialize;
    use tauri::ipc::CapabilityBuilder;
    use tauri::plugin::TauriPlugin;
    use tauri::{AppHandle, Emitter, EventTarget, Manager, Webview, Wry};

    /// The reply to an inspector message: the review's notes after it was applied.
    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct MsgReply {
        pub notes: Vec<ReviewNote>,
    }

    /// `review://changed`, to the review's window only.
    #[derive(Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct ReviewChanged<'a> {
        id: &'a str,
        doc: &'a ReviewDoc,
        missing: &'a [u32],
        saved: bool,
    }

    /// Tells the review's window (its tab) that the review changed.
    pub fn emit_changed(app: &AppHandle, r: &Review) {
        let changed = ReviewChanged { id: &r.id, doc: &r.doc, missing: &r.missing, saved: r.saved };
        let _ = app.emit_to(EventTarget::webview_window(r.window.as_str()), "review://changed", changed);
    }

    /// `invoke("plugin:devcat-review|msg", { review, secret, msg })` from the inspector. The caller
    /// is checked before the secret is; every refusal is the same error as an unknown review.
    #[tauri::command]
    async fn msg(webview: Webview, review: String, secret: String, msg: serde_json::Value) -> AppResult<MsgReply> {
        let r = review_store().get(&review).ok_or_else(no_review)?;
        let url = webview.url().map_err(|_| no_review())?;
        if !caller_allowed(&r, webview.label(), &url, serve::running_origin().as_deref()) {
            return Err(no_review());
        }
        review_store().apply(&review, &secret, &msg)?;
        let now = review_store().get(&review).ok_or_else(no_review)?;
        emit_changed(webview.app_handle(), &now);
        Ok(MsgReply { notes: now.doc.notes })
    }

    /// The inline plugin `devcat-review` (its permission `devcat-review:allow-msg` comes from build.rs).
    pub fn plugin() -> TauriPlugin<Wry> {
        tauri::plugin::Builder::new("devcat-review").invoke_handler(tauri::generate_handler![msg]).build()
    }

    /// The only capability review webviews get: `msg`, from any http(s) page (`:*` so ports match),
    /// never from a local origin.
    pub fn add_capability(app: &AppHandle) -> tauri::Result<()> {
        app.add_capability(
            CapabilityBuilder::new("review")
                .local(false)
                .webview("review-*")
                .remote("http://*:*".into())
                .remote("https://*:*".into())
                .permission("devcat-review:allow-msg"),
        )
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
    fn local_targets() {
        let dev = u("http://localhost:1420/");
        assert!(is_local_target(&u("tauri://localhost"), None));
        assert!(is_local_target(&u("ipc://localhost"), None));
        assert!(is_local_target(&u("asset://localhost/x"), None));
        assert!(is_local_target(&u("devcat-review://localhost/f/x"), None));
        assert!(is_local_target(&u("http://devcat-review.localhost/"), None));
        assert!(is_local_target(&u("https://tauri.localhost/index.html"), None));
        assert!(is_local_target(&u("http://ASSET.LOCALHOST./x"), None));
        assert!(is_local_target(&u("http://localhost:1420/"), Some(&dev)));
        assert!(is_local_target(&u("http://localhost:1420/deep/page?q"), Some(&dev)));
        assert!(!is_local_target(&u("http://localhost:1420/"), None));
        assert!(!is_local_target(&u("http://localhost:5173/"), Some(&dev)));
        assert!(!is_local_target(&u("http://localhost/"), None));
        assert!(!is_local_target(&u("http://127.0.0.1:1420/"), Some(&dev)));
        assert!(!is_local_target(&u("https://example.com"), Some(&dev)));
        assert!(!is_local_target(&u("https://localhost.example.com"), None));
    }

    fn review(target: Target) -> Review {
        ReviewStore::default().start(target, "/".into(), None, "main".into())
    }

    #[test]
    fn caller_must_be_the_reviews_webview_on_its_origin() {
        let r = review(Target::Url { origin: "http://localhost:5173".into() });
        let label = format!("review-{}", r.id);
        assert!(caller_allowed(&r, &label, &u("http://localhost:5173/a/b?c"), None));
        assert!(!caller_allowed(&r, "review-rv-other", &u("http://localhost:5173/"), None));
        assert!(!caller_allowed(&r, "main", &u("http://localhost:5173/"), None));
        assert!(!caller_allowed(&r, &format!("{label}x"), &u("http://localhost:5173/"), None));
        assert!(!caller_allowed(&r, &label, &u("https://example.com/"), None));
        assert!(!caller_allowed(&r, &label, &u("http://localhost:5174/"), None));
        assert!(!caller_allowed(&r, &label, &u("https://localhost:5173/"), None));
        // `http://localhost:5173.evil.test` isn't a URL (bad port); these are its parsable look-alikes
        assert!(Url::parse("http://localhost:5173.evil.test/").is_err());
        assert!(!caller_allowed(&r, &label, &u("http://localhost.evil.test:5173/"), None));
        assert!(!caller_allowed(&r, &label, &u("http://localhost:5173@evil.test/"), None));
        assert!(!caller_allowed(&r, &label, &u("http://evil.test/?http://localhost:5173"), None));
        assert!(!caller_allowed(&r, &label, &u("http://127.0.0.1:5173/"), None));

        let r = review(Target::Url { origin: "https://example.com".into() });
        let label = format!("review-{}", r.id);
        assert!(caller_allowed(&r, &label, &u("https://example.com:443/x"), None));
        assert!(!caller_allowed(&r, &label, &u("https://example.com:8443/x"), None));
        assert!(!caller_allowed(&r, &label, &u("http://example.com/x"), None));

        let bad = review(Target::Url { origin: "http://localhost:5173.evil.test".into() });
        let label = format!("review-{}", bad.id);
        assert!(!caller_allowed(&bad, &label, &u("http://localhost:5173/"), None));
    }

    #[test]
    fn file_caller_must_be_under_its_own_prefix() {
        let r = review(Target::File { root: "/tmp/site".into(), rel: "index.html".into() });
        let label = format!("review-{}", r.id);
        let fo = Some("http://127.0.0.1:4000");
        let at = |p: &str| u(&format!("http://127.0.0.1:4000{p}"));
        assert!(caller_allowed(&r, &label, &at(&format!("/f/{}/index.html", r.id)), fo));
        assert!(caller_allowed(&r, &label, &at(&format!("/f/{}/a/b.html?x#y", r.id)), fo));
        assert!(!caller_allowed(&r, &label, &at(&format!("/f/{}/index.html", r.id)), None), "server not running");
        assert!(!caller_allowed(&r, &label, &at("/f/rv-other/index.html"), fo));
        assert!(!caller_allowed(&r, &label, &at(&format!("/f/{}x/index.html", r.id)), fo));
        assert!(!caller_allowed(&r, &label, &at(&format!("/f/{}", r.id)), fo));
        assert!(!caller_allowed(&r, &label, &at(&format!("/f/{}/../rv-other/index.html", r.id)), fo));
        assert!(!caller_allowed(&r, &label, &u(&format!("http://127.0.0.1:4001/f/{}/index.html", r.id)), fo));
        assert!(!caller_allowed(&r, &label, &u(&format!("http://localhost:4000/f/{}/index.html", r.id)), fo));
        assert!(!caller_allowed(&r, "review-rv-other", &at(&format!("/f/{}/index.html", r.id)), fo));
    }
}
