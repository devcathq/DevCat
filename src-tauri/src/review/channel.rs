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

/// Is `url` on the review's site? Its origin, or the same site after the redirects nearly every
/// site makes when it loads: `http` → `https` (typed addresses start as `http://`) and adding or
/// removing `www.` (`google.com` → `www.google.com`). Same port, never a different host. `inspector.js` (`sameSite`) applies the same rule in the page.
pub fn same_site(review_origin: &str, url: &Url) -> bool {
    let Ok(o) = Url::parse(review_origin) else { return false };
    if !o.origin().is_tuple() || !url.origin().is_tuple() {
        return false;
    }
    if o.origin() == url.origin() {
        return true;
    }
    let scheme_ok = o.scheme() == url.scheme() || (o.scheme() == "http" && url.scheme() == "https");
    let (Some(a), Some(b)) = (o.host_str(), url.host_str()) else { return false };
    let www = |x: &str, y: &str| x.strip_prefix("www.").is_some_and(|rest| rest == y && rest.contains('.'));
    scheme_ok && o.port() == url.port() && (a == b || www(a, b) || www(b, a))
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
        Target::Url { origin } => same_site(origin, webview_url),
        Target::File { .. } => {
            file_origin.is_some_and(same_origin) && webview_url.path().starts_with(&format!("/f/{}/", review.id))
        }
    }
}

/// May DevCat call into the review webview (`window.__devcatReview.*`, which gets the notes)?
/// Only while it shows the review's own page: on any other origin the inspector is inert, so that
/// site could define `__devcatReview` itself. `webview_url` is `None` when it can't be read.
pub fn may_tell_page(review: &Review, webview_label: &str, webview_url: Option<&Url>, file_origin: Option<&str>) -> bool {
    webview_url.is_some_and(|url| caller_allowed(review, webview_label, url, file_origin))
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
        // a link or window.open asking for a new tab: the inspector reports it itself, since WebView2
        // doesn't always tell DevCat (see `webview::open_new_tab`); nothing in the review changes
        if msg.get("kind").and_then(|k| k.as_str()) == Some("newWindow") {
            let url = review_store().new_window(&review, &secret, &msg)?;
            if let Ok(url) = reqwest::Url::parse(&url) {
                super::super::webview::open_new_tab(webview.app_handle(), &r.id, &url);
            }
            return Ok(MsgReply { notes: r.doc.notes });
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
        // http → https on the same host and port is the same site (see same_site)
        assert!(caller_allowed(&r, &label, &u("https://localhost:5173/"), None));
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
    fn same_site_follows_https_and_www_redirects_only() {
        // what typing `google.com` does: http://google.com → https://www.google.com/?zx=…
        assert!(same_site("http://google.com", &u("https://www.google.com/?zx=1")));
        assert!(same_site("https://www.example.com", &u("https://example.com/a")));
        assert!(same_site("http://example.com", &u("http://www.example.com/")));
        assert!(same_site("http://localhost:3000", &u("http://localhost:3000/x")));
        // never a downgrade, another port, another host or a look-alike
        assert!(!same_site("https://example.com", &u("http://example.com/")));
        assert!(!same_site("https://example.com", &u("https://example.com:8443/")));
        assert!(!same_site("https://example.com", &u("https://evil.com/")));
        assert!(!same_site("https://example.com", &u("https://www.evil.com/")));
        assert!(!same_site("https://example.com", &u("https://wwwexample.com/")));
        assert!(!same_site("https://example.com", &u("https://www.www.example.com/")));
        assert!(!same_site("https://example.com", &u("https://app.example.com/")));
        assert!(!same_site("https://example.com", &u("https://example.com.evil.test/")));
        assert!(!same_site("http://localhost", &u("http://www.localhost/")), "www. needs a dotted host");
        assert!(!same_site("https://www.com", &u("https://com/")));
        assert!(!same_site("not a url", &u("https://example.com/")));
        assert!(!same_site("https://example.com", &u("data:text/html,x")));
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

    #[test]
    fn devcat_tells_only_the_reviews_own_page() {
        let r = review(Target::Url { origin: "http://localhost:5173".into() });
        let label = format!("review-{}", r.id);
        assert!(may_tell_page(&r, &label, Some(&u("http://localhost:5173/x")), None));
        // navigated away: that site could define `__devcatReview` and read every note
        assert!(!may_tell_page(&r, &label, Some(&u("https://evil.example/")), None));
        assert!(!may_tell_page(&r, &label, None, None), "URL unknown");
        assert!(!may_tell_page(&r, "review-rv-other", Some(&u("http://localhost:5173/x")), None));
        let f = review(Target::File { root: "/tmp/site".into(), rel: "index.html".into() });
        let fl = format!("review-{}", f.id);
        let fo = Some("http://127.0.0.1:4000");
        assert!(may_tell_page(&f, &fl, Some(&u(&format!("http://127.0.0.1:4000/f/{}/index.html", f.id))), fo));
        assert!(!may_tell_page(&f, &fl, Some(&u("http://127.0.0.1:4000/f/rv-other/index.html")), fo));
    }
}
