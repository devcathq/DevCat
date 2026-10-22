//! OS keychain access (service "dev.devcat.app") with an in-memory cache for the life of the
//! process. Only found values are cached; a missing entry is looked up again next time.
//!
//! macOS asks the user before an app reads a keychain item whose access list doesn't trust it,
//! and for an app without an Apple team ID that trust is tied to the exact build, so every update
//! asks again, once per item. So on macOS all secrets live in ONE item ("vault", a JSON map):
//! at most one prompt per update ("Always Allow" covers the rest of that version). Secrets saved
//! by older versions as separate items move into the vault the first time they're read.
//! Windows / Linux keep one item per secret (no prompts there; Windows limits an item's size).
use crate::error::AppResult;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

const SERVICE: &str = "dev.devcat.app";
#[cfg(target_os = "macos")]
const VAULT: &str = "vault";

static CACHE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Secrets live in memory only (tests, and the dev-only browser bridge with its throwaway data).
static MEMORY_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(cfg!(test));

/// Never touch the OS keychain in this process (dev bridge).
pub fn memory_only() {
    MEMORY_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn in_memory() -> bool {
    MEMORY_ONLY.load(std::sync::atomic::Ordering::Relaxed)
}

fn item(key: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, key).ok()?.get_password().ok()
}

#[cfg(target_os = "macos")]
mod vault {
    use super::*;

    /// The vault's contents, read once per process (None until then).
    static VAULT_MAP: LazyLock<Mutex<Option<HashMap<String, String>>>> = LazyLock::new(|| Mutex::new(None));

    pub(super) fn with<R>(f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
        let mut g = VAULT_MAP.lock();
        let map = g.get_or_insert_with(|| item(VAULT).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default());
        f(map)
    }

    pub(super) fn save(map: &HashMap<String, String>) -> AppResult<()> {
        keyring::Entry::new(SERVICE, VAULT)?.set_password(&serde_json::to_string(map)?)?;
        Ok(())
    }

    pub(super) fn get(key: &str) -> Option<String> {
        // "" marks a deleted secret (so an older version's item doesn't come back).
        if let Some(v) = with(|m| m.get(key).cloned()) {
            return (!v.is_empty()).then_some(v);
        }
        // Saved by an older version as its own item: copy it into the vault. The old item stays
        // (deleting it could ask the user again); it's never read once the vault has the key.
        let v = item(key)?;
        let snapshot = with(|m| {
            m.insert(key.to_string(), v.clone());
            m.clone()
        });
        let _ = save(&snapshot);
        Some(v)
    }

    /// Set `key`, or (None) mark it deleted.
    pub(super) fn set(key: &str, value: Option<&str>) -> AppResult<()> {
        let snapshot = with(|m| {
            m.insert(key.to_string(), value.unwrap_or_default().to_string());
            m.clone()
        });
        save(&snapshot)
    }
}

pub fn get(key: &str) -> Option<String> {
    if let Some(v) = CACHE.lock().get(key) {
        return Some(v.clone());
    }
    if in_memory() {
        return None; // tests never touch the real keychain
    }
    #[cfg(target_os = "macos")]
    let v = vault::get(key)?;
    #[cfg(not(target_os = "macos"))]
    let v = item(key)?;
    CACHE.lock().insert(key.to_string(), v.clone());
    Some(v)
}

/// Store `value`; an empty value deletes the entry.
pub fn set(key: &str, value: &str) -> AppResult<()> {
    if in_memory() {
        if value.is_empty() { CACHE.lock().remove(key); } else { CACHE.lock().insert(key.into(), value.into()); }
        return Ok(());
    }
    if value.is_empty() {
        delete(key);
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    vault::set(key, Some(value))?;
    #[cfg(not(target_os = "macos"))]
    keyring::Entry::new(SERVICE, key)?.set_password(value)?;
    CACHE.lock().insert(key.to_string(), value.to_string());
    Ok(())
}

pub fn delete(key: &str) {
    CACHE.lock().remove(key);
    if in_memory() {
        return;
    }
    // macOS: marked deleted in the vault (an older version's separate item is left alone:
    // removing it may ask the user, and it's never read again).
    #[cfg(target_os = "macos")]
    {
        let _ = vault::set(key, None);
    }
    #[cfg(not(target_os = "macos"))]
    if let Ok(e) = keyring::Entry::new(SERVICE, key) {
        let _ = e.delete_credential();
    }
}
