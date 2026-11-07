//! The review tab's commands (app windows only: a review webview gets none of them).

use super::webview::{self, call, find};
use super::{channel, file_start, format, no_review, review_date, review_store, serve, url_start, ReviewDoc, Target};
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
    review_store().set_load_url(&r.id, load);
    Ok(ReviewInfo { id: r.id, title, doc: r.doc })
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
    let r = review_store().get(&id).ok_or_else(no_review)?;
    if r.window != window.label() {
        return Err(no_review());
    }
    let (pos, size) = (LogicalPosition::new(x.max(0.0), y.max(0.0)), LogicalSize::new(w.max(1.0), h.max(1.0)));
    let err = |e: tauri::Error| AppError::Msg(format!("Couldn't place the review page: {e}"));
    let wv = match find(&app, &id) {
        Some(wv) => {
            wv.set_position(pos).map_err(err)?;
            wv.set_size(size).map_err(err)?;
            wv
        }
        None => webview::create(&window, &r, pos, size)?,
    };
    if visible { wv.show() } else { wv.hide() }.map_err(err)
}

#[tauri::command]
pub async fn review_nav(app: AppHandle, id: String, action: NavAction) -> AppResult<()> {
    let wv = find(&app, &id).ok_or_else(no_review)?;
    let err = |e: tauri::Error| AppError::Msg(format!("Couldn't go there: {e}"));
    match action {
        NavAction::Named(a) if a == "back" => wv.eval("history.back()").map_err(err),
        NavAction::Named(a) if a == "reload" => wv.reload().map_err(err),
        NavAction::Named(a) => Err(AppError::Msg(format!("Unknown action {a}"))),
        NavAction::Go { url } => {
            let to = url_start(&url, webview::dev_url(&app).as_ref())?.load_url;
            let to = reqwest::Url::parse(&to).map_err(|_| AppError::Msg("Enter a web address (http or https)".into()))?;
            wv.navigate(to).map_err(err)
        }
    }
}

#[tauri::command]
pub async fn review_inspect(app: AppHandle, id: String, on: bool) -> AppResult<()> {
    let wv = find(&app, &id).ok_or_else(no_review)?;
    call(&wv, "setInspect", &json!(on))
}

#[tauri::command]
pub async fn review_highlight(app: AppHandle, id: String, n: Option<u32>, scroll: bool) -> AppResult<()> {
    let wv = find(&app, &id).ok_or_else(no_review)?;
    match n {
        Some(n) if scroll => call(&wv, "scrollTo", &json!(n)),
        _ => call(&wv, "highlight", &json!(n)),
    }
}

/// After a change from the tab: the page's pins and the tab follow.
fn changed(app: &AppHandle, id: &str) {
    let Some(r) = review_store().get(id) else { return };
    if let Some(wv) = find(app, id) {
        let _ = call(&wv, "setNotes", &json!(r.doc.notes));
    }
    channel::emit_changed(app, &r);
}

#[tauri::command]
pub async fn review_edit(app: AppHandle, id: String, n: u32, comment: String) -> AppResult<ReviewDoc> {
    let doc = review_store().edit(&id, n, &comment)?;
    changed(&app, &id);
    Ok(doc)
}

#[tauri::command]
pub async fn review_remove(app: AppHandle, id: String, n: u32) -> AppResult<ReviewDoc> {
    let doc = review_store().remove(&id, n)?;
    changed(&app, &id);
    Ok(doc)
}

fn render(id: &str, styles: bool) -> AppResult<String> {
    let r = review_store().get(id).ok_or_else(no_review)?;
    Ok(format::render(&r.doc, styles, &review_date(chrono::Local::now().date_naive())))
}

#[tauri::command]
pub async fn review_render(id: String, styles: bool) -> AppResult<String> {
    render(&id, styles)
}

/// Saves the review as a note: the one it was saved to before while that note is there, else a
/// new one in the review's project (General when none, or when the project is gone).
#[tauri::command]
pub async fn review_finish(app: AppHandle, state: St, id: String, styles: bool) -> AppResult<Note> {
    let r = review_store().get(&id).ok_or_else(no_review)?;
    let body = render(&id, styles)?;
    let earlier = r.note_id.as_ref().and_then(|nid| state.notes.list().ok()?.into_iter().find(|n| &n.id == nid));
    let note = match earlier {
        Some(old) => Note { body, ..old },
        None => {
            let project_id = r.project_id.filter(|p| state.projects.exists(p).unwrap_or(false));
            Note { id: String::new(), project_id, body, pinned: false, created_at: 0, updated_at: 0 }
        }
    };
    let saved = crate::notes::save_checked(&state.notes, &state.projects, note)?;
    review_store().set_saved(&id, saved.id.clone());
    // every window reloads its notes (the same message a window sends after a save)
    crate::events::emit("devcat://window-sync", json!({ "kind": "notes", "from": "review" }));
    if let Some(r) = review_store().get(&id) {
        channel::emit_changed(&app, &r);
    }
    Ok(saved)
}

#[tauri::command]
pub async fn review_close(app: AppHandle, id: String) -> AppResult<()> {
    let _g = CREATING.lock();
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
