//! Persistence: profiles in `<data>/network.json`, VPN configs in `<data>/network/<id>.ovpn|.conf`
//! (0600), secrets in the OS keychain (`net:<id>:password`, `net:<id>:ssh`).
use super::{NetKind, NetProfile, NetProfileFull};
use crate::error::{AppError, AppResult};
use std::io::Write;
use std::path::{Path, PathBuf};

pub trait Secrets: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> AppResult<()>;
    fn delete(&self, key: &str);
}

pub struct Keyring;

impl Secrets for Keyring {
    fn get(&self, key: &str) -> Option<String> {
        crate::secrets::get(key)
    }
    fn set(&self, key: &str, value: &str) -> AppResult<()> {
        crate::secrets::set(key, value)
    }
    fn delete(&self, key: &str) {
        crate::secrets::delete(key)
    }
}

/// Ids end up in file names: allow only uuid-like ids.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn check_id(id: &str) -> AppResult<()> {
    if valid_id(id) { Ok(()) } else { Err(AppError::msg(format!("Invalid network profile id \"{id}\""))) }
}

fn pw_key(id: &str) -> String {
    format!("net:{id}:password")
}
fn ssh_key(id: &str) -> String {
    format!("net:{id}:ssh")
}

/// Write `data` to `path` atomically (temp file + rename), mode 0600 on unix.
pub fn write_private(path: &Path, data: &[u8]) -> AppResult<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().unwrap_or_default().to_string_lossy()));
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
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub struct NetStore {
    data_dir: PathBuf,
    secrets: Box<dyn Secrets>,
}

impl NetStore {
    pub fn new(data_dir: PathBuf, secrets: Box<dyn Secrets>) -> Self {
        Self { data_dir, secrets }
    }

    fn json_path(&self) -> PathBuf {
        self.data_dir.join("network.json")
    }

    /// `<data>/network`, created with 0700 on demand.
    pub fn dir(&self) -> AppResult<PathBuf> {
        let d = self.data_dir.join("network");
        if !d.exists() {
            std::fs::create_dir_all(&d)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
            }
        }
        Ok(d)
    }

    pub fn config_path(&self, id: &str, kind: NetKind) -> AppResult<PathBuf> {
        check_id(id)?;
        let ext = match kind {
            NetKind::Openvpn => "ovpn",
            _ => "conf",
        };
        Ok(self.dir()?.join(format!("{id}.{ext}")))
    }

    pub fn load_all(&self) -> AppResult<Vec<NetProfile>> {
        let p = self.json_path();
        if !p.exists() {
            return Ok(vec![]);
        }
        let mut list: Vec<NetProfile> = serde_json::from_str(&std::fs::read_to_string(p)?)?;
        for n in &mut list {
            n.vpn_password = None;
            if let Some(s) = n.ssh.as_mut() {
                s.password = None;
            }
        }
        Ok(list)
    }

    fn write_all(&self, list: &[NetProfile]) -> AppResult<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let p = self.json_path();
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(list)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    /// Profile without secrets.
    pub fn find(&self, id: &str) -> AppResult<NetProfile> {
        self.load_all()?
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(|| AppError::msg(format!("Network profile {id} not found")))
    }

    /// Profile with secrets and config text.
    pub fn get(&self, id: &str) -> AppResult<NetProfileFull> {
        check_id(id)?;
        let mut p = self.find(id)?;
        p.vpn_password = self.secrets.get(&pw_key(id));
        if let Some(s) = p.ssh.as_mut() {
            s.password = self.secrets.get(&ssh_key(id));
        }
        let config_text = match p.kind {
            NetKind::Ssh | NetKind::K8s => None,
            k => std::fs::read_to_string(self.config_path(id, k)?).ok(),
        };
        Ok(NetProfileFull { profile: p, config_text })
    }

    /// Create (empty id) or update. Secrets: `Some(..)` replaces (empty clears), `None` keeps.
    pub fn save(&self, mut p: NetProfile, config_text: Option<String>) -> AppResult<NetProfile> {
        let mut all = self.load_all()?;
        if p.id.is_empty() {
            p.id = uuid::Uuid::new_v4().to_string();
        }
        check_id(&p.id)?;
        if p.name.trim().is_empty() {
            return Err(AppError::msg("Name is required"));
        }
        let existing = all.iter().position(|n| n.id == p.id);
        match p.kind {
            NetKind::Ssh => {
                let ssh = p.ssh.as_ref().ok_or_else(|| AppError::msg("SSH server settings are required"))?;
                if ssh.host.trim().is_empty() {
                    return Err(AppError::msg("SSH host is required"));
                }
                for f in &p.forwards {
                    match f.kind.as_str() {
                        "local" if f.remote_host.trim().is_empty() || f.remote_port == 0 => {
                            return Err(AppError::msg("Local forwards need a remote host and port"));
                        }
                        "local" | "dynamic" => {}
                        k => return Err(AppError::msg(format!("Unknown forward kind \"{k}\""))),
                    }
                }
            }
            NetKind::K8s => {
                if p.options.get(super::kube_forward::CONN_OPTION).is_none_or(|s| s.trim().is_empty()) {
                    return Err(AppError::msg("Pick the Kubernetes connection this tunnel uses"));
                }
                if p.forwards.is_empty() {
                    return Err(AppError::msg("Add at least one port forward"));
                }
                if let Some(e) = p.forwards.iter().find_map(super::kube_forward::validate) {
                    return Err(AppError::msg(e));
                }
                p.summary = Some(super::kube_forward::summary(&p.forwards));
            }
            kind => {
                let path = self.config_path(&p.id, kind)?;
                match config_text.as_deref() {
                    Some(t) if !t.trim().is_empty() => {
                        p.summary = Some(match kind {
                            NetKind::Openvpn => super::parse::parse_ovpn(t).summary,
                            _ => super::parse::parse_wireguard(t).summary,
                        });
                        write_private(&path, t.as_bytes())?;
                    }
                    _ if !path.exists() => return Err(AppError::msg("The VPN configuration is empty")),
                    _ => {}
                }
            }
        }
        // Remove a config file left over from a kind change.
        for k in [NetKind::Openvpn, NetKind::Wireguard] {
            if k != p.kind {
                let _ = std::fs::remove_file(self.config_path(&p.id, k)?);
            }
        }
        if let Some(pw) = p.vpn_password.take() {
            self.secrets.set(&pw_key(&p.id), &pw)?;
        }
        if let Some(pw) = p.ssh.as_mut().and_then(|s| s.password.take()) {
            self.secrets.set(&ssh_key(&p.id), &pw)?;
        }
        match existing {
            Some(i) => all[i] = p.clone(),
            None => all.push(p.clone()),
        }
        self.write_all(&all)?;
        Ok(p)
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        check_id(id)?;
        let mut all = self.load_all()?;
        all.retain(|n| n.id != id);
        self.write_all(&all)?;
        for ext in ["ovpn", "run.ovpn", "conf", "log", "pid", "mgmt"] {
            let _ = std::fs::remove_file(self.dir()?.join(format!("{id}.{ext}")));
        }
        self.secrets.delete(&pw_key(id));
        self.secrets.delete(&ssh_key(id));
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::models::SshConfig;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default, Clone)]
    pub struct MemSecrets(pub Arc<Mutex<HashMap<String, String>>>);

    impl Secrets for MemSecrets {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) -> AppResult<()> {
            if value.is_empty() {
                self.0.lock().remove(key);
            } else {
                self.0.lock().insert(key.into(), value.into());
            }
            Ok(())
        }
        fn delete(&self, key: &str) {
            self.0.lock().remove(key);
        }
    }

    pub fn profile(kind: NetKind) -> NetProfile {
        NetProfile {
            id: String::new(),
            name: "Office".into(),
            kind,
            project_id: None,
            color: None,
            auto_connect: false,
            ssh: None,
            forwards: vec![],
            vpn_user: None,
            vpn_password: None,
            summary: None,
            options: Default::default(),
            mcp_access: None,
        }
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("devcat-netstore-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn ids() {
        assert!(valid_id("3f2b1c9e-0000-4a4a-8b8b-123456789abc"));
        assert!(!valid_id("../etc/passwd"));
        assert!(!valid_id("a b"));
        assert!(!valid_id(""));
    }

    #[test]
    fn round_trip() {
        let dir = tmp();
        let secrets = MemSecrets::default();
        let store = NetStore::new(dir.clone(), Box::new(secrets.clone()));

        let mut p = profile(NetKind::Openvpn);
        p.vpn_user = Some("alice".into());
        p.vpn_password = Some("pw1".into());
        assert!(store.save(p.clone(), None).is_err(), "VPN without config");
        let saved = store.save(p, Some("remote vpn.example.com 443\nproto tcp\n".into())).unwrap();
        assert!(valid_id(&saved.id));
        assert!(saved.vpn_password.is_none());
        assert_eq!(saved.summary.as_deref(), Some("tcp vpn.example.com:443"));
        let id = saved.id.clone();

        let raw = std::fs::read_to_string(dir.join("network.json")).unwrap();
        assert!(!raw.contains("pw1"), "secret on disk: {raw}");
        let cfg = dir.join("network").join(format!("{id}.ovpn"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777, 0o600);
        }

        assert!(store.load_all().unwrap()[0].vpn_password.is_none());
        let full = store.get(&id).unwrap();
        assert_eq!(full.profile.vpn_password.as_deref(), Some("pw1"));
        assert!(full.config_text.unwrap().contains("vpn.example.com"));

        // Update without config text / password keeps both.
        let mut upd = full.profile.clone();
        upd.vpn_password = None;
        upd.name = "Renamed".into();
        store.save(upd, None).unwrap();
        let full = store.get(&id).unwrap();
        assert_eq!(full.profile.name, "Renamed");
        assert_eq!(full.profile.vpn_password.as_deref(), Some("pw1"));
        assert!(full.config_text.is_some());
        assert_eq!(store.load_all().unwrap().len(), 1);

        // Kind change removes the old file.
        let mut wg = full.profile.clone();
        wg.kind = NetKind::Wireguard;
        store.save(wg, Some("[Interface]\nPrivateKey = x\n[Peer]\nEndpoint = h:1\n".into())).unwrap();
        assert!(!cfg.exists());
        assert!(dir.join("network").join(format!("{id}.conf")).exists());

        store.delete(&id).unwrap();
        assert!(store.load_all().unwrap().is_empty());
        assert!(secrets.0.lock().is_empty());
        assert!(!dir.join("network").join(format!("{id}.conf")).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ssh_profile() {
        let dir = tmp();
        let secrets = MemSecrets::default();
        let store = NetStore::new(dir.clone(), Box::new(secrets.clone()));
        let mut p = profile(NetKind::Ssh);
        assert!(store.save(p.clone(), None).is_err());
        p.ssh = Some(SshConfig { enabled: true, host: "bastion".into(), port: 22, user: "u".into(), auth: "password".into(), key_path: None, password: Some("sshpw".into()) });
        p.forwards = vec![super::super::PortForward { kind: "local".into(), bind_port: 0, remote_host: "".into(), remote_port: 0, namespace: String::new() }];
        assert!(store.save(p.clone(), None).is_err(), "local forward without target");
        p.forwards[0].remote_host = "db".into();
        p.forwards[0].remote_port = 5432;
        let saved = store.save(p, None).unwrap();
        assert!(saved.ssh.as_ref().unwrap().password.is_none());
        assert_eq!(secrets.get(&format!("net:{}:ssh", saved.id)).as_deref(), Some("sshpw"));
        let full = store.get(&saved.id).unwrap();
        assert_eq!(full.profile.ssh.unwrap().password.as_deref(), Some("sshpw"));
        assert!(full.config_text.is_none());
        assert!(store.get("../x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn k8s_profiles_need_a_connection_and_valid_forwards() {
        let dir = tmp();
        let st = NetStore::new(dir.clone(), Box::new(MemSecrets::default()));
        let mut p = profile(NetKind::K8s);
        p.forwards = vec![super::super::PortForward { kind: "service".into(), bind_port: 6543, remote_host: "postgres-prod-rw".into(), remote_port: 5432, namespace: "database".into() }];
        assert!(st.save(p.clone(), None).unwrap_err().to_string().contains("Kubernetes connection"));
        p.options.insert("k8sConnId".into(), "conn-1".into());
        let saved = st.save(p.clone(), None).unwrap();
        let back = st.get(&saved.id).unwrap();
        assert!(back.config_text.is_none());
        assert_eq!(back.profile.forwards[0].namespace, "database");
        p.forwards[0].remote_port = 0;
        assert!(st.save(p.clone(), None).is_err());
        p.forwards.clear();
        assert!(st.save(p, None).unwrap_err().to_string().contains("port forward"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
