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
    state.close_read_only(&config.id).await;
    drop_infra_clients(&config.id).await;
    let mut config = config;
    // An SSH session that can't be used shouldn't save (its folders would just fail later).
    if config.kind == crate::models::ConnectionKind::Ssh {
        shape_variant(&state, &mut config)?;
        let full = crate::claude::remote::effective_config(&config, |id| state.store.get(id))?;
        crate::claude::remote::Session::from_config(&full)?;
    }
    crate::claude::remote::forget_session(&config.id).await;
    config.group = None;
    if let Some(pid) = &config.project_id {
        if !state.projects.exists(pid)? {
            config.project_id = None;
        }
    }
    let saved = state.store.save(config)?;
    // A session's variants log in as it does: let go of their connections too, and keep them in
    // its project.
    let variants = variants_of(&state, &saved.id)?;
    if !variants.is_empty() {
        for v in &variants {
            crate::claude::remote::forget_session(v).await;
        }
        let project = saved.project_id.clone();
        state.store.update_all(|c| {
            if variants.contains(&c.id) {
                c.project_id = project.clone();
            }
        })?;
    }
    Ok(saved)
}

/// Ids of the SSH session variants of `id`.
fn variants_of(state: &St, id: &str) -> AppResult<Vec<String>> {
    if id.is_empty() {
        return Ok(vec![]);
    }
    Ok(state.store.load_all()?.into_iter().filter(|c| c.options.get(crate::claude::remote::PARENT).map(String::as_str) == Some(id)).map(|c| c.id).collect())
}

/// A variant of an SSH session sits in its parent's project; whatever it leaves empty is read from
/// its parent each time (`remote::effective_config`), so editing the parent changes it too.
fn shape_variant(state: &St, c: &mut ConnectionConfig) -> AppResult<()> {
    use crate::claude::remote::PARENT;
    let Some(pid) = c.options.get(PARENT).map(|p| p.trim().to_string()).filter(|p| !p.is_empty()) else {
        c.options.remove(PARENT);
        return Ok(());
    };
    if !variants_of(state, &c.id)?.is_empty() {
        return Err(crate::error::AppError::msg("This session has variants of its own, so it can't become a variant"));
    }
    let parent = state.store.get(&pid).map_err(|_| crate::error::AppError::msg("The session this is a variant of was deleted"))?;
    c.project_id = parent.project_id;
    c.ssh = None;
    c.mcp_access = None;
    c.read_only = false;
    // Empty = the parent's: keep no empty options around.
    c.options.retain(|_, v| !v.trim().is_empty());
    c.options.insert(PARENT.into(), pid);
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn delete_connection(state: St, id: String) -> AppResult<()> {
    // An SSH session's variants can't work without it: they go with it (the window says so).
    for v in variants_of(&state, &id)? {
        Box::pin(delete_connection(state, v)).await?;
    }
    let removed = state.sessions.write().await.remove(&id);
    if let Some(d) = removed {
        d.close().await;
    }
    state.close_read_only(&id).await;
    drop_infra_clients(&id).await;
    crate::claude::remote::forget_session(&id).await;
    state.store.delete(&id)?;
    if let Err(e) = state.bookmarks.remove_for_connection(&id) {
        eprintln!("bookmark cleanup for {id} failed: {e}");
    }
    Ok(())
}

/// Test an unsaved profile. Database kinds return the server version.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn test_connection(config: ConnectionConfig) -> AppResult<String> {
    if config.kind == crate::models::ConnectionKind::Ssh {
        // The session connects its VPN itself (a variant's is its parent's).
        return crate::claude::remote::test_session(&config).await;
    }
    crate::network::ensure_for(&config).await?;
    match config.kind {
        crate::models::ConnectionKind::Kubernetes => crate::infra::k8s::test(&config).await,
        crate::models::ConnectionKind::Rabbitmq => crate::infra::rabbitmq::test(&config).await,
        crate::models::ConnectionKind::Docker => crate::infra::docker::test(&config).await,
        crate::models::ConnectionKind::Ssh => crate::claude::remote::test_session(&config).await,
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
    state.close_read_only(&id).await;
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
