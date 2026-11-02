use crate::bookmarks::BookmarkStore;
use crate::db::DynDriver;
use crate::error::{AppError, AppResult};
use crate::history::HistoryStore;
use crate::notes::NoteStore;
use crate::projects::ProjectStore;
use crate::store::ConnectionStore;
use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct AppState {
    pub store: ConnectionStore,
    pub history: HistoryStore,
    pub projects: ProjectStore,
    pub bookmarks: BookmarkStore,
    pub notes: NoteStore,
    /// Open database sessions keyed by connection id.
    pub sessions: RwLock<HashMap<String, DynDriver>>,
    /// Read-only twins of those sessions (`read_only_driver`), keyed by connection id.
    read_only_sessions: RwLock<HashMap<String, DynDriver>>,
}

impl AppState {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        let store = ConnectionStore::new(data_dir.clone());
        let projects = ProjectStore::new(data_dir.clone());
        if let Err(e) = projects.migrate(&store) {
            eprintln!("project migration failed: {e}");
        }
        Self {
            store,
            history: HistoryStore::new(data_dir.clone()),
            bookmarks: BookmarkStore::new(data_dir.clone()),
            notes: NoteStore::new(data_dir),
            projects,
            sessions: RwLock::new(HashMap::new()),
            read_only_sessions: RwLock::new(HashMap::new()),
        }
    }

    /// A session on `conn_id` opened read-only (as if the connection were set read-only), for a
    /// caller that may only read: an agent's query DevCat judged a read by its words (it then runs
    /// without asking, or over MCP without full access), a read-only phone. Words can be made to
    /// look like a read (comment and quoting rules differ per database, functions can write), so
    /// the driver and, where it can, the server refuse writes too (`default_transaction_read_only`,
    /// `SET SESSION TRANSACTION READ ONLY`, SQLite opened read-only). A connection that's read-only
    /// already uses its own session.
    pub async fn read_only_driver(&self, conn_id: &str) -> AppResult<DynDriver> {
        let mut cfg = self.store.get(conn_id).map_err(|_| AppError::NotConnected(conn_id.into()))?;
        if cfg.read_only {
            return self.driver(conn_id).await;
        }
        if let Some(d) = self.read_only_sessions.read().await.get(conn_id) {
            return Ok(d.clone());
        }
        cfg.read_only = true;
        crate::network::ensure_for(&cfg).await?;
        let d = crate::db::connect(&cfg).await?;
        self.read_only_sessions.write().await.insert(conn_id.to_string(), d.clone());
        Ok(d)
    }

    /// The open read-only twin of `conn_id`'s session, if any (to cancel its query).
    pub async fn open_read_only(&self, conn_id: &str) -> Option<DynDriver> {
        self.read_only_sessions.read().await.get(conn_id).cloned()
    }

    /// Close the read-only twin of `conn_id`'s session: wherever the session itself is dropped
    /// (settings changed, connection closed or deleted), so the next read connects anew.
    pub async fn close_read_only(&self, conn_id: &str) {
        let removed = self.read_only_sessions.write().await.remove(conn_id);
        if let Some(d) = removed {
            d.close().await;
        }
    }

    /// Get an open session, connecting lazily if the profile exists but is not open yet.
    pub async fn driver(&self, conn_id: &str) -> AppResult<DynDriver> {
        if let Some(d) = self.sessions.read().await.get(conn_id) {
            return Ok(d.clone());
        }
        let cfg = self.store.get(conn_id).map_err(|_| AppError::NotConnected(conn_id.into()))?;
        crate::network::ensure_for(&cfg).await?;
        let d = crate::db::connect(&cfg).await?;
        self.sessions.write().await.insert(conn_id.to_string(), d.clone());
        Ok(d)
    }
}

static STATE: once_cell::sync::OnceCell<AppState> = once_cell::sync::OnceCell::new();

/// Create the app state (once, at startup).
pub fn init(data_dir: std::path::PathBuf) {
    let _ = STATE.set(AppState::new(data_dir));
}

/// The app state, as a command argument (`state: St`): commands then work the same from a
/// window, a phone, or `devcat serve` (no Tauri app at all).
#[derive(Clone, Copy)]
pub struct St(&'static AppState);

impl St {
    pub fn get() -> Self {
        St(STATE.get().expect("app state not initialized"))
    }

    /// The app state, or None before `init` (unit tests, early startup).
    pub fn try_get() -> Option<Self> {
        STATE.get().map(St)
    }
}

impl std::ops::Deref for St {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        self.0
    }
}

#[cfg(feature = "gui")]
impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for St {
    fn from_command(_: tauri::ipc::CommandItem<'de, R>) -> Result<Self, tauri::ipc::InvokeError> {
        STATE.get().map(St).ok_or_else(|| tauri::ipc::InvokeError::from("DevCat isn't ready yet"))
    }
}
