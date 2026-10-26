//! What the phone keeps: its own key, its name, and the computers it paired with (id, name,
//! addresses). No connection profiles, no passwords: those stay on the computers.
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// The computer's key (hex): connections are only made to this exact computer.
    pub id: String,
    pub name: String,
    /// "ip:port" / "host:port" to try, in order.
    pub addrs: Vec<String>,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub app_version: String,
    /// What the computer granted this phone: "full" | "readonly".
    #[serde(default)]
    pub access: String,
    pub added_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// Addresses the user typed (kept first, and not replaced by what the computer reports).
    #[serde(default)]
    pub manual_addrs: Vec<String>,
    /// The computer's relay (`public` or a relay URL), for reaching it from other networks.
    #[serde(default)]
    pub relay: Option<String>,
    /// The relay server the computer was last connected to (URL), tried before iroh's DNS lookup.
    #[serde(default)]
    pub home: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    #[serde(default)]
    pub device_name: Option<String>,
}

pub struct Store {
    dir: PathBuf,
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
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
    std::fs::rename(tmp, path)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    /// This phone's identity (32 secret bytes), created on first launch.
    pub fn secret_key(&self) -> Result<[u8; 32], String> {
        let path = self.dir.join("identity.key");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let bytes = hex::decode(text.trim()).map_err(|e| format!("identity.key: {e}"))?;
            return bytes.try_into().map_err(|_| "identity.key has the wrong length".to_string());
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| e.to_string())?;
        write_private(&path, hex::encode(key).as_bytes()).map_err(|e| e.to_string())?;
        Ok(key)
    }

    pub fn machines(&self) -> Vec<Machine> {
        std::fs::read(self.dir.join("machines.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save_machines(&self, m: &[Machine]) -> Result<(), String> {
        write_private(&self.dir.join("machines.json"), &serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }

    pub fn machine(&self, id: &str) -> Option<Machine> {
        self.machines().into_iter().find(|m| m.id == id)
    }

    pub fn upsert(&self, m: Machine) -> Result<(), String> {
        let mut all = self.machines();
        match all.iter_mut().find(|x| x.id == m.id) {
            Some(x) => *x = m,
            None => all.push(m),
        }
        self.save_machines(&all)
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut Machine)) -> Result<Option<Machine>, String> {
        let mut all = self.machines();
        let found = all.iter_mut().find(|m| m.id == id).map(|m| {
            f(m);
            m.clone()
        });
        if found.is_some() {
            self.save_machines(&all)?;
        }
        Ok(found)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut all = self.machines();
        all.retain(|m| m.id != id);
        self.save_machines(&all)
    }

    pub fn prefs(&self) -> Prefs {
        std::fs::read(self.dir.join("prefs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save_prefs(&self, p: &Prefs) -> Result<(), String> {
        write_private(&self.dir.join("prefs.json"), &serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }
}

/// Addresses to try: the user's own first, then what the computer reports (deduplicated).
pub fn merged_addrs(manual: &[String], reported: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in manual.iter().chain(reported) {
        if !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines_round_trip() {
        let d = std::env::temp_dir().join(format!("devcat-mobile-{}", now_ms()));
        let s = Store::new(d);
        let k = s.secret_key().unwrap();
        assert_eq!(k, s.secret_key().unwrap());
        let m = Machine { id: "a".into(), name: "Mac".into(), addrs: vec!["1.2.3.4:1".into()], os: String::new(), app_version: String::new(), access: "full".into(), added_at: 1, last_seen: None, manual_addrs: vec![], relay: None, home: None };
        s.upsert(m.clone()).unwrap();
        s.update("a", |m| m.name = "Mac 2".into()).unwrap();
        assert_eq!(s.machine("a").unwrap().name, "Mac 2");
        s.remove("a").unwrap();
        assert!(s.machines().is_empty());
        assert_eq!(merged_addrs(&["x:1".into()], &["y:1".into(), "x:1".into()]), vec!["x:1", "y:1"]);
    }
}
