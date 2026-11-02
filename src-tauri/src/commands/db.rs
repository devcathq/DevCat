use crate::db::*;
use crate::error::AppResult;
use crate::history::HistoryEntry;
use crate::state::St;
use serde_json::Value;

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_children(state: St, conn_id: String, path: Vec<String>) -> AppResult<Vec<TreeNode>> {
    state.driver(&conn_id).await?.children(&path).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_list_databases(state: St, conn_id: String) -> AppResult<Vec<String>> {
    state.driver(&conn_id).await?.list_databases().await
}

/// Execute a script and record it in history (unless `skip_history`, used for
/// internal queries such as Mongo page loads or Redis key lookups).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_execute(state: St, conn_id: String, script: String, opts: ExecOptions, skip_history: Option<bool>) -> AppResult<Vec<QueryResult>> {
    let d = state.driver(&conn_id).await?;
    execute_on(state, d, conn_id, script, opts, skip_history).await
}

/// `db_execute` on a read-only session (`St::read_only_driver`), for a caller that may only read
/// (a read-only phone): the driver and server refuse writes, not only DevCat's look at the words.
pub async fn db_execute_read_only(state: St, conn_id: String, script: String, opts: ExecOptions) -> AppResult<Vec<QueryResult>> {
    let d = state.read_only_driver(&conn_id).await?;
    execute_on(state, d, conn_id, script, opts, Some(false)).await
}

/// `db_table_data` on a read-only session (its `filter` is the caller's SQL).
pub async fn db_table_data_read_only(state: St, conn_id: String, req: TableDataRequest) -> AppResult<TableDataPage> {
    state.read_only_driver(&conn_id).await?.table_data(&req).await
}

async fn execute_on(state: St, d: crate::db::DynDriver, conn_id: String, script: String, opts: ExecOptions, skip_history: Option<bool>) -> AppResult<Vec<QueryResult>> {
    let started = std::time::Instant::now();
    let res = d.execute(&script, &opts).await;
    if skip_history.unwrap_or(false) {
        return res;
    }
    let _ = state.history.add(HistoryEntry {
        id: String::new(),
        connection_id: conn_id,
        database: opts.database.clone(),
        query: script,
        executed_at: String::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        success: res.is_ok(),
        error: res.as_ref().err().map(|e| e.to_string()),
    });
    res
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_cancel(state: St, conn_id: String) -> AppResult<()> {
    // a read-only phone's query runs on the read-only twin
    if let Some(ro) = state.open_read_only(&conn_id).await {
        let _ = ro.cancel().await;
    }
    state.driver(&conn_id).await?.cancel().await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_table_data(state: St, conn_id: String, req: TableDataRequest) -> AppResult<TableDataPage> {
    state.driver(&conn_id).await?.table_data(&req).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_table_structure(state: St, conn_id: String, table: TableRef) -> AppResult<TableStructure> {
    state.driver(&conn_id).await?.table_structure(&table).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_apply_changes(state: St, conn_id: String, table: TableRef, changes: Vec<RowChange>) -> AppResult<u64> {
    state.driver(&conn_id).await?.apply_changes(&table, &changes).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_object_ddl(state: St, conn_id: String, table: TableRef, kind: NodeKind) -> AppResult<String> {
    state.driver(&conn_id).await?.object_ddl(&table, &kind).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_schema_snapshot(state: St, conn_id: String, database: Option<String>, schema: Option<String>) -> AppResult<SchemaSnapshot> {
    state.driver(&conn_id).await?.schema_snapshot(database.as_deref(), schema.as_deref()).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_get_key(state: St, conn_id: String, database: Option<String>, key: String) -> AppResult<Value> {
    state.driver(&conn_id).await?.get_key(database.as_deref(), &key).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_set_key(state: St, conn_id: String, database: Option<String>, value: Value) -> AppResult<()> {
    state.driver(&conn_id).await?.set_key(database.as_deref(), &value).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn db_delete_key(state: St, conn_id: String, database: Option<String>, key: String) -> AppResult<()> {
    state.driver(&conn_id).await?.delete_key(database.as_deref(), &key).await
}
