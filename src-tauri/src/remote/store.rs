//! Remote access files in `<app data>/remote/`: settings, the host's secret key, paired phones
//! and the audit log. Everything is local to this computer; nothing is synced anywhere.
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSettings {
    /// Accept connections from paired phones.
    #[serde(default)]
    pub enabled: bool,
    /// Name phones show for this computer (default: the computer's name).
    #[serde(default)]
    pub host_name: Option<String>,
    /// UDP port phones connect to.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Public address (host name or IP, optionally with a port) for phones outside this
    /// network; the router must forward the UDP port here.
    #[serde(default)]
    pub public_addr: Option<String>,
    /// Also serve DevCat Mobile to phone browsers (TCP, same port number).
    #[serde(default = "yes")]
    pub web: bool,
    /// How phones on other networks get here: through iroh's public relays, your own relay
    /// (`relay_url`), or not at all (same network / public address only).
    #[serde(default)]
    pub relay: RelayChoice,
    #[serde(default)]
    pub relay_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayChoice {
    #[default]
    Public,
    Custom,
    Off,
}

fn yes() -> bool {
    true
}

fn default_port() -> u16 {
    devcat_remote_proto::DEFAULT_PORT
}

impl RemoteSettings {
    /// The relay as phones see it ([`devcat_remote_proto::RELAY_PUBLIC`] or a URL); `None` when off.
    pub fn relay_value(&self) -> Option<String> {
        match self.relay {
            RelayChoice::Public => Some(devcat_remote_proto::RELAY_PUBLIC.into()),
            RelayChoice::Custom => self.relay_url.clone().filter(|u| !u.is_empty()),
            RelayChoice::Off => None,
        }
    }
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { enabled: false, host_name: None, port: default_port(), public_addr: None, web: true, relay: RelayChoice::Public, relay_url: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// The phone's iroh endpoint id (public key, hex): the only thing that lets it in.
    pub id: String,
    pub name: String,
    pub platform: String,
    /// "full" | "readonly"
    pub access: String,
    pub paired_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// "app" (DevCat Mobile), "web" (a phone browser on this computer's page) or "browser" (the
    /// hosted page, through the relay; same protocol as the app).
    #[serde(default = "app_kind")]
    pub kind: String,
    /// Browsers: the device secret (base64url) their session keys come from. Never sent to
    /// the desktop UI (see `Device::public`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

fn app_kind() -> String {
    "app".into()
}

impl Device {
    /// Without the secret (for the desktop UI).
    pub fn public(mut self) -> Self {
        self.secret = None;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub at: i64,
    pub device: String,
    pub device_name: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub ok: bool,
}

const AUDIT_KEEP: usize = 2000;

pub struct RemoteStore {
    dir: PathBuf,
}

/// Write `data` atomically, readable by the current user only (unix).
pub(crate) fn write_private(path: &Path, data: &[u8]) -> AppResult<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl RemoteStore {
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("remote");
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Self { dir }
    }

    fn read_json<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        std::fs::read(self.dir.join(name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn settings(&self) -> RemoteSettings {
        self.read_json("settings.json")
    }

    pub fn save_settings(&self, s: &RemoteSettings) -> AppResult<()> {
        write_private(&self.dir.join("settings.json"), &serde_json::to_vec_pretty(s)?)
    }

    /// The host's identity (32 secret bytes), created on first use. A file instead of the
    /// keychain: reading the keychain at every launch shows a password prompt on macOS.
    pub fn secret_key(&self) -> AppResult<[u8; 32]> {
        let path = self.dir.join("host.key");
        if let Ok(hex_text) = std::fs::read_to_string(&path) {
            let bytes = hex::decode(hex_text.trim()).map_err(|e| AppError::msg(format!("remote/host.key: {e}")))?;
            return bytes.try_into().map_err(|_| AppError::msg("remote/host.key has the wrong length"));
        }
        let key: [u8; 32] = random_bytes();
        write_private(&path, hex::encode(key).as_bytes())?;
        Ok(key)
    }

    /// Forget the identity: every paired phone must pair again.
    pub fn reset_identity(&self) -> AppResult<()> {
        let _ = std::fs::remove_file(self.dir.join("host.key"));
        self.save_devices(&[])
    }

    pub fn devices(&self) -> Vec<Device> {
        self.read_json("devices.json")
    }

    pub fn save_devices(&self, d: &[Device]) -> AppResult<()> {
        write_private(&self.dir.join("devices.json"), &serde_json::to_vec_pretty(d)?)
    }

    pub fn update_device(&self, id: &str, f: impl FnOnce(&mut Device)) -> AppResult<Option<Device>> {
        let mut all = self.devices();
        let found = all.iter_mut().find(|d| d.id == id).map(|d| {
            f(d);
            d.clone()
        });
        if found.is_some() {
            self.save_devices(&all)?;
        }
        Ok(found)
    }

    pub fn add_device(&self, d: Device) -> AppResult<()> {
        let mut all = self.devices();
        all.retain(|x| x.id != d.id);
        all.push(d);
        self.save_devices(&all)
    }

    pub fn remove_device(&self, id: &str) -> AppResult<bool> {
        let mut all = self.devices();
        let n = all.len();
        all.retain(|d| d.id != id);
        if all.len() != n {
            self.save_devices(&all)?;
        }
        Ok(all.len() != n)
    }

    pub fn audit(&self, e: &AuditEntry) {
        audit_append(&self.dir.join("audit.jsonl"), e);
    }

    /// Newest first.
    pub fn audit_tail(&self, n: usize) -> Vec<AuditEntry> {
        audit_read(&self.dir.join("audit.jsonl"), n)
    }
}

/// Add an entry to an audit log (JSON lines, readable by the current user only), trimming it
/// now and then (the file is only ever appended to otherwise). Memory sync keeps one too.
pub(crate) fn audit_append(path: &Path, e: &AuditEntry) {
    let line = match serde_json::to_string(e) {
        Ok(l) => l,
        Err(_) => return,
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(path) {
        let _ = writeln!(f, "{line}");
    }
    if std::fs::metadata(path).map(|m| m.len() > 1_500_000).unwrap_or(false) {
        let keep = audit_read(path, AUDIT_KEEP / 2);
        let text: String = keep.iter().rev().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        let _ = write_private(path, text.as_bytes());
    }
}

/// An audit log's entries, newest first.
pub(crate) fn audit_read(path: &Path, n: usize) -> Vec<AuditEntry> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().rev().filter_map(|l| serde_json::from_str(l).ok()).take(n).collect()
}

/// Cryptographically random bytes from the OS.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("OS random number generator unavailable");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn key_is_stable_and_private() {
        let d = tmp();
        let s = RemoteStore::new(&d);
        let a = s.secret_key().unwrap();
        assert_eq!(a, s.secret_key().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("remote/host.key")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.reset_identity().unwrap();
        assert_ne!(a, s.secret_key().unwrap());
    }

    #[test]
    fn devices_and_audit() {
        let s = RemoteStore::new(&tmp());
        assert_eq!(s.settings(), RemoteSettings::default());
        let d = Device { id: "p1".into(), name: "Phone".into(), platform: "ios".into(), access: "full".into(), paired_at: 1, last_seen: None, kind: "app".into(), secret: None };
        s.add_device(d.clone()).unwrap();
        s.add_device(d.clone()).unwrap();
        assert_eq!(s.devices().len(), 1);
        s.update_device("p1", |d| d.access = "readonly".into()).unwrap();
        assert_eq!(s.devices()[0].access, "readonly");
        assert!(s.remove_device("p1").unwrap());
        assert!(!s.remove_device("p1").unwrap());
        for i in 0..3 {
            s.audit(&AuditEntry { at: i, device: "p1".into(), device_name: "Phone".into(), action: format!("a{i}"), detail: None, ok: true });
        }
        let t = s.audit_tail(2);
        assert_eq!(t.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), ["a2", "a1"]);
    }

    #[test]
    fn random_bytes_differ() {
        let a: [u8; 32] = random_bytes();
        let b: [u8; 32] = random_bytes();
        assert_ne!(a, b);
    }
}
