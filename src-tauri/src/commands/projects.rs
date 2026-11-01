use crate::error::{AppError, AppResult};
use crate::models::Project;
use crate::state::St;

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn list_projects(state: St) -> AppResult<Vec<Project>> {
    state.projects.list()
}

/// Create (empty `id`) or change a project (name, color, code folders, MCP writes).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn save_project(state: St, project: Project) -> AppResult<Project> {
    let p = state.projects.save(project)?;
    // its code folders may have changed: agents' MCP configs follow
    tokio::task::spawn_blocking(crate::mcp_sync::auto);
    Ok(p)
}

/// Delete a project. Its connections are moved out of the project, or deleted
/// (including their keychain secrets) when `delete_connections` is true.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_project(state: St, id: String, delete_connections: Option<bool>) -> AppResult<()> {
    let members: Vec<String> = state
        .store
        .load_all()?
        .into_iter()
        .filter(|c| c.project_id.as_deref() == Some(id.as_str()))
        .map(|c| c.id)
        .collect();
    if delete_connections.unwrap_or(false) {
        for cid in &members {
            if let Some(d) = state.sessions.write().await.remove(cid) {
                d.close().await;
            }
            state.close_read_only(cid).await;
            super::connections::drop_infra_clients(cid).await;
            state.store.delete(cid)?;
            if let Err(e) = state.bookmarks.remove_for_connection(cid) {
                eprintln!("bookmark cleanup for {cid} failed: {e}");
            }
        }
    } else {
        state.store.update_all(|c| {
            if c.project_id.as_deref() == Some(id.as_str()) {
                c.project_id = None;
            }
        })?;
    }
    state.notes.unassign_project(&id)?;
    state.projects.delete(&id)?;
    tokio::task::spawn_blocking(crate::mcp_sync::auto);
    Ok(())
}

/// Move connections into a project (`project_id = None` removes them from any project).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn move_connections(state: St, ids: Vec<String>, project_id: Option<String>) -> AppResult<()> {
    if let Some(pid) = &project_id {
        if !state.projects.exists(pid)? {
            return Err(AppError::msg("Project not found"));
        }
    }
    state.store.update_all(|c| {
        if ids.contains(&c.id) {
            c.project_id = project_id.clone();
        }
    })
}

/// Persist a new sidebar order; returns the reordered list.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn reorder_projects(state: St, ids: Vec<String>) -> AppResult<Vec<Project>> {
    state.projects.reorder(&ids)
}
