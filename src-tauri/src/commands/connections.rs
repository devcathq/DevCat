use crate::error::AppResult;
use crate::models::ConnectionConfig;
use crate::state::St;

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn list_connections(state: St) -> AppResult<Vec<ConnectionConfig>> {
    state.store.load_all()
}

/// Returns the profile including its password (for the edit dialog).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn get_connection(state: St, id: String) -> AppResult<ConnectionConfig> {
    state.store.get(&id)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn save_connection(state: St, config: ConnectionConfig) -> AppResult<ConnectionConfig> {
    // Drop any open session so the next use picks up the new settings.
    let removed = state.sessions.write().await.remove(&config.id);
    if let Some(d) = removed {
        d.close().await;
    }
    drop_infra_clients(&config.id).await;
    let mut config = config;
    config.group = None;
    if let Some(pid) = &config.project_id {
        if !state.projects.exists(pid)? {
            config.project_id = None;
        }
    }
    state.store.save(config)
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_connection(state: St, id: String) -> AppResult<()> {
    let removed = state.sessions.write().await.remove(&id);
    if let Some(d) = removed {
        d.close().await;
    }
    drop_infra_clients(&id).await;
    state.store.delete(&id)?;
    if let Err(e) = state.bookmarks.remove_for_connection(&id) {
        eprintln!("bookmark cleanup for {id} failed: {e}");
    }
    Ok(())
}

/// Test an unsaved profile. Database kinds return the server version.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn test_connection(config: ConnectionConfig) -> AppResult<String> {
    crate::network::ensure_for(&config).await?;
    match config.kind {
        crate::models::ConnectionKind::Kubernetes => crate::infra::k8s::test(&config).await,
        crate::models::ConnectionKind::Rabbitmq => crate::infra::rabbitmq::test(&config).await,
        _ => {
            let d = crate::db::connect(&config).await?;
            let v = d.server_version().await;
            d.close().await;
            v
        }
    }
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn open_connection(state: St, id: String) -> AppResult<String> {
    let d = state.driver(&id).await?;
    d.server_version().await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn close_connection(state: St, id: String) -> AppResult<()> {
    let removed = state.sessions.write().await.remove(&id);
    if let Some(d) = removed {
        d.close().await;
    }
    drop_infra_clients(&id).await;
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn open_connection_ids(state: St) -> AppResult<Vec<String>> {
    Ok(state.sessions.read().await.keys().cloned().collect())
}

/// Forget cached Kubernetes/RabbitMQ clients (and stop k8s streams) for a profile
/// so edited settings take effect without a manual reconnect.
pub(crate) async fn drop_infra_clients(id: &str) {
    crate::infra::k8s::drop_conn(id).await;
    crate::infra::rabbitmq::client::evict(id);
}
