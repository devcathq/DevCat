//! The review tab's commands (app windows only: a review webview gets none of them).

use super::webview::{self, call, find};
use super::{channel, file_start, no_review, review_date, review_store, save_review, serve, url_start, Rendered, Review, ReviewDoc, Target};
use crate::error::{AppError, AppResult};
use crate::notes::Note;
use crate::state::St;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Window};

/// What to review: `{ url }` or `{ path }` (a local `.html` file).
#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum StartTarget {
    Url { url: String },
    Path { path: String },
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInfo {
    pub id: String,
    pub title: String,
    pub doc: ReviewDoc,
}

/// `"back"`, `"reload"` or `{ url }`.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum NavAction {
    Go { url: String },
    Named(String),
}

#[tauri::command]
pub async fn review_start(
    app: AppHandle,
    window: Window,
    state: St,
    target: StartTarget,
    project_id: Option<String>,
) -> AppResult<ReviewInfo> {
    let start = match target {
        StartTarget::Url { url } => url_start(&url, webview::dev_url(&app).as_ref())?,
        StartTarget::Path { path } => file_start(&path, &state.projects.list()?)?,
    };
    // the file server runs before the page loads (and before the inspector is told its origin)
    if matches!(start.target, Target::File { .. }) && !serve::file_server().listening() {
        return Err(AppError::Msg("DevCat couldn't start serving local files".into()));
    }
    let title = start.title.clone();
    let r = review_store().start(start.target, start.first_page, project_id, window.label().to_string());
    let load = match &r.doc.target {
        Target::File { rel, .. } => serve::file_server().url(&r.id, rel),
        Target::Url { .. } => start.load_url,
    };
    // a file on an SSH host: connect now, so a login or a missing file is an error here, not a blank page
    if let Target::File { root, rel } = &r.doc.target
        && let Some(remote) = serve::remote_root(root)
    {
        let failed = match super::remote::fetch(&r.id, remote, rel).await {
            Ok(super::remote::Answer::File(_)) => None,
            Ok(super::remote::Answer::TooBig) => Some(AppError::Msg("That file is too big to review".into())),
            Ok(super::remote::Answer::Missing) => Some(AppError::Msg("That file isn't there, or can't be shown".into())),
            Err(e) => Some(e),
        };
        if let Some(e) = failed {
            review_store().close(&r.id);
            return Err(e);
        }
    }
    review_store().set_load_url(&r.id, load);
    Ok(ReviewInfo { id: r.id, title, doc: r.doc })
}

/// The review, if it belongs to the calling window: a window only drives its own reviews.
fn owned(id: &str, window: &Window) -> AppResult<Review> {
    review_store().get(id).filter(|r| r.window == window.label()).ok_or_else(no_review)
}

/// The review and its webview, for the calling window's review.
fn page(app: &AppHandle, window: &Window, id: &str) -> AppResult<(Review, tauri::Webview)> {
    let r = owned(id, window)?;
    Ok((r, find(app, id).ok_or_else(no_review)?))
}

/// Creating and closing are serialized, so two quick calls don't both add the webview and a
/// close can't slip between finding the review and adding its webview.
static CREATING: Mutex<()> = Mutex::new(());

/// Places the review's page over the tab's page area (logical px of the window); the first call
/// creates it. Hidden pages stay loaded.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn review_place(
    app: AppHandle,
    window: Window,
    id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    visible: bool,
) -> AppResult<()> {
    let _g = CREATING.lock();
    let r = owned(&id, &window)?;
    let (pos, size) = (LogicalPosition::new(x.max(0.0), y.max(0.0)), LogicalSize::new(w.max(1.0), h.max(1.0)));
    let err = |e: tauri::Error| AppError::Msg(format!("Couldn't place the review page: {e}"));
    let wv = match find(&app, &id) {
        Some(wv) => {
            wv.set_position(pos).map_err(err)?;
            wv.set_size(size).map_err(err)?;
            wv
        }
        None => {
            let wv = webview::create(&window, &r, pos, size)?;
            // its window closed meanwhile (that doesn't wait for this lock): don't leave it behind
            if review_store().get(&id).is_none() {
                let _ = wv.close();
                return Err(no_review());
            }
            wv
        }
    };
    if visible { wv.show() } else { wv.hide() }.map_err(err)
}

#[tauri::command]
/// Back, reload, or go to an address typed in the tab. A typed address on another site isn't
/// opened here (the review stays on its own site, `channel::same_site`): `false` tells the tab to
/// start a new review for it.
pub async fn review_nav(app: AppHandle, window: Window, id: String, action: NavAction) -> AppResult<bool> {
    let (r, wv) = page(&app, &window, &id)?;
    let err = |e: tauri::Error| AppError::Msg(format!("Couldn't go there: {e}"));
    match action {
        NavAction::Named(a) if a == "back" => wv.eval("history.back()").map(|_| true).map_err(err),
        NavAction::Named(a) if a == "reload" => wv.reload().map(|_| true).map_err(err),
        NavAction::Named(a) => Err(AppError::Msg(format!("Unknown action {a}"))),
        NavAction::Go { url } => {
            let to = url_start(&url, webview::dev_url(&app).as_ref())?.load_url;
            let to = reqwest::Url::parse(&to).map_err(|_| AppError::Msg("Enter a web address (http or https)".into()))?;
            match &r.doc.target {
                Target::Url { origin } if channel::same_site(origin, &to) => wv.navigate(to).map(|_| true).map_err(err),
                _ => Ok(false),
            }
        }
    }
}

#[tauri::command]
pub async fn review_inspect(app: AppHandle, window: Window, id: String, on: bool) -> AppResult<()> {
    let (r, wv) = page(&app, &window, &id)?;
    call(&wv, &r, "setInspect", &json!(on))
}

#[tauri::command]
pub async fn review_highlight(app: AppHandle, window: Window, id: String, n: Option<u32>, scroll: bool) -> AppResult<()> {
    let (r, wv) = page(&app, &window, &id)?;
    match n {
        Some(n) if scroll => call(&wv, &r, "scrollTo", &json!(n)),
        _ => call(&wv, &r, "highlight", &json!(n)),
    }
}

/// After a change from the tab: the page's pins and the tab follow.
fn changed(app: &AppHandle, id: &str) {
    let Some(r) = review_store().get(id) else { return };
    if let Some(wv) = find(app, id) {
        let _ = call(&wv, &r, "setNotes", &json!(r.doc.notes));
    }
    channel::emit_changed(app, &r);
}

#[tauri::command]
pub async fn review_edit(app: AppHandle, window: Window, id: String, n: u32, comment: String) -> AppResult<ReviewDoc> {
    owned(&id, &window)?;
    let doc = review_store().edit(&id, n, &comment)?;
    changed(&app, &id);
    Ok(doc)
}

#[tauri::command]
pub async fn review_remove(app: AppHandle, window: Window, id: String, n: u32) -> AppResult<ReviewDoc> {
    owned(&id, &window)?;
    let doc = review_store().remove(&id, n)?;
    changed(&app, &id);
    Ok(doc)
}

/// The review as text for the user to check, with the revision `review_finish` must be given.
#[tauri::command]
pub async fn review_render(window: Window, id: String, styles: bool) -> AppResult<Rendered> {
    owned(&id, &window)?;
    review_store().render(&id, styles, &review_date(chrono::Local::now().date_naive()))
}

/// Saves exactly the text `review_render` showed for `rev`; refused when the review changed since.
#[tauri::command]
pub async fn review_finish(app: AppHandle, window: Window, state: St, id: String, styles: bool, rev: u64) -> AppResult<Note> {
    owned(&id, &window)?;
    let saved = save_review(review_store(), &state.notes, &state.projects, &id, styles, rev)?;
    // every window reloads its notes (the same message a window sends after a save)
    crate::events::emit("devcat://window-sync", json!({ "kind": "notes", "from": "review" }));
    if let Some(r) = review_store().get(&id) {
        channel::emit_changed(&app, &r);
    }
    Ok(saved)
}

#[tauri::command]
pub async fn review_close(app: AppHandle, window: Window, id: String) -> AppResult<()> {
    let _g = CREATING.lock();
    owned(&id, &window)?;
    review_store().close(&id);
    if let Some(wv) = find(&app, &id) {
        wv.close().map_err(|e| AppError::Msg(format!("Couldn't close the review page: {e}")))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn review_clear_data(app: AppHandle) -> AppResult<()> {
    webview::clear_data(&app).await
}
