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

    /// The app state, or None before startup created it.
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
