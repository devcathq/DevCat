//! SSH tunnelling for database connections.
//!
//! Drivers call `open_endpoint(cfg)` before connecting and connect to the returned
//! host/port instead of `cfg.host`/`cfg.port`. They must keep the returned
//! `SshTunnel` alive (store it in the driver struct) for the lifetime of the session.
//!
//! With SSH enabled we authenticate against the bastion (password or private key),
//! bind a listener on `127.0.0.1:0` and open one `direct-tcpip` channel per accepted
//! local connection to `cfg.host:cfg.port`, piping bytes both ways.
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, SshConfig};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Guard that keeps a local port-forward alive; dropping it closes the tunnel.
pub struct SshTunnel {
    local_port: u16,
    accept_task: JoinHandle<()>,
    session: Arc<client::Handle<TunnelHandler>>,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        // Stops accepting new connections; the per-connection tasks hold their own
        // channel and finish when either side closes. The session itself shuts down
        // once the last handle is dropped, but we ask for a clean disconnect too.
        self.accept_task.abort();
        let session = self.session.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
            });
        }
    }
}

pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tunnel: Option<SshTunnel>,
}

/// Resolve the endpoint to connect to. Without SSH this is just `cfg.host:cfg.port`
/// (falling back to `default_port`). With SSH enabled it opens a tunnel and returns
/// `127.0.0.1:<local port>`.
pub async fn open_endpoint(cfg: &ConnectionConfig, default_port: u16) -> AppResult<Endpoint> {
    let host = if cfg.host.is_empty() { "localhost".to_string() } else { cfg.host.clone() };
    let port = cfg.port.unwrap_or(default_port);
    match &cfg.ssh {
        Some(ssh) if ssh.enabled => {
            let tunnel = open_tunnel(ssh, &host, port).await?;
            Ok(Endpoint { host: "127.0.0.1".into(), port: tunnel.local_port, tunnel: Some(tunnel) })
        }
        _ => Ok(Endpoint { host, port, tunnel: None }),
    }
}

/// Checks the server's host key (`check_host_key`); why it refused one, for the error.
pub(crate) struct TunnelHandler {
    host: String,
    port: u16,
    refused: Arc<std::sync::Mutex<Option<String>>>,
}

/// An authenticated SSH client session.
pub(crate) type SshSession = client::Handle<TunnelHandler>;

impl client::Handler for TunnelHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            // a host certificate: its key, as known_hosts records it
            PublicKeyOrCertificate::Certificate(c) => russh::keys::PublicKey::from(c.public_key().clone()),
        };
        match check_host_key(&self.host, self.port, &key, known_hosts_file().as_deref()) {
            Ok(()) => Ok(true),
            Err(why) => {
                *self.refused.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                Ok(false)
            }
        }
    }
}

/// DevCat's own record of SSH host keys (`<data>/ssh_known_hosts`).
static KNOWN_HOSTS: once_cell::sync::OnceCell<PathBuf> = once_cell::sync::OnceCell::new();

/// Where DevCat records the SSH host keys it has seen (the app and `devcat mcp`, from their data
/// folder; else the default one).
pub fn init(data_dir: PathBuf) {
    let _ = KNOWN_HOSTS.set(data_dir.join("ssh_known_hosts"));
}

fn known_hosts_file() -> Option<PathBuf> {
    KNOWN_HOSTS.get().cloned().or_else(|| crate::cli::default_data_dir().map(|d| d.join("ssh_known_hosts")))
}

/// Whether `key` may be `host:port`'s. As the user's `~/.ssh/known_hosts` has it; else as DevCat
/// recorded it the first time it connected (`known`), trust on first use, like OpenSSH's
/// `StrictHostKeyChecking=accept-new`. A key that differs from a recorded one is refused:
/// someone may be between DevCat and the server, waiting for the password and the database
/// traffic. With nowhere to record keys, any is taken (as before).
fn check_host_key(host: &str, port: u16, key: &russh::keys::PublicKey, known: Option<&std::path::Path>) -> Result<(), String> {
    use russh::keys::Error::KeyChanged;
    let changed = |line: usize, file: &str| {
        format!("SSH: {host}'s host key isn't the one recorded for it (line {line} of {file}), so DevCat didn't connect: someone may be between you and the server. If its key really changed, remove that line.")
    };
    match russh::keys::check_known_hosts(host, port, key) {
        Ok(true) => return Ok(()),
        Err(KeyChanged { line }) => return Err(changed(line, "~/.ssh/known_hosts")),
        _ => {}
    }
    let Some(known) = known else { return Ok(()) };
    match russh::keys::check_known_hosts_path(host, port, key, known) {
        Ok(true) => Ok(()),
        Err(KeyChanged { line }) => Err(changed(line, &known.display().to_string())),
        _ => {
            // the name goes into the file as it is: nothing that could start another line
            if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:%[]".contains(c)) {
                return Err(format!("SSH: {host:?} isn't a host name DevCat can record a key for"));
            }
            russh::keys::known_hosts::learn_known_hosts_path(host, port, key, known).map_err(|e| format!("SSH: couldn't record {host}'s host key: {e}"))
        }
    }
}

fn ssh_err(e: impl std::fmt::Display) -> AppError {
    AppError::msg(format!("SSH: {e}"))
}

pub(crate) fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// Key files to try when key auth is selected: the configured one, or the usual defaults.
fn key_candidates(ssh: &SshConfig) -> Vec<PathBuf> {
    match ssh.key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => vec![expand_home(p)],
        None => {
            let Some(home) = dirs::home_dir() else { return vec![] };
            ["id_ed25519", "id_ecdsa", "id_rsa"]
                .iter()
                .map(|n| home.join(".ssh").join(n))
                .filter(|p| p.exists())
                .collect()
        }
    }
}

async fn authenticate(session: &mut client::Handle<TunnelHandler>, ssh: &SshConfig) -> AppResult<()> {
    let user = if ssh.user.is_empty() { "root".to_string() } else { ssh.user.clone() };
    if ssh.auth == "key" {
        let candidates = key_candidates(ssh);
        if candidates.is_empty() {
            return Err(ssh_err("no private key file configured and none found in ~/.ssh"));
        }
        let mut last_err = String::from("public key rejected by server");
        for path in candidates {
            // For key auth the SSH password field doubles as the key passphrase.
            let key = match russh::keys::load_secret_key(&path, ssh.password.as_deref().filter(|p| !p.is_empty())) {
                Ok(k) => k,
                Err(e) => {
                    last_err = format!("cannot load key {}: {e}", path.display());
                    continue;
                }
            };
            let hash = session.best_supported_rsa_hash().await.map_err(ssh_err)?.flatten();
            let res = session
                .authenticate_publickey(user.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(ssh_err)?;
            if res.success() {
                return Ok(());
            }
        }
        Err(ssh_err(format!("authentication failed for {user}: {last_err}")))
    } else {
        let res = session
            .authenticate_password(user.clone(), ssh.password.clone().unwrap_or_default())
            .await
            .map_err(ssh_err)?;
        if res.success() { Ok(()) } else { Err(ssh_err(format!("password authentication failed for {user}"))) }
    }
}

/// Connect and authenticate to `ssh.host` (keepalive every 30s; the session closes after
/// three unanswered keepalives). Also used by standalone tunnels in `crate::network`.
pub(crate) async fn connect_session(ssh: &SshConfig) -> AppResult<SshSession> {
    if ssh.host.is_empty() {
        return Err(ssh_err("SSH host is empty"));
    }
    let config = Arc::new(client::Config {
        nodelay: true,
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let ssh_port = if ssh.port == 0 { 22 } else { ssh.port };
    let refused = Arc::new(std::sync::Mutex::new(None));
    let handler = TunnelHandler { host: ssh.host.trim().to_string(), port: ssh_port, refused: refused.clone() };
    let connect = client::connect(config, (ssh.host.as_str(), ssh_port), handler);
    let mut session = tokio::time::timeout(Duration::from_secs(20), connect)
        .await
        .map_err(|_| ssh_err(format!("timed out connecting to {}:{ssh_port}", ssh.host)))?
        .map_err(|e| match refused.lock().unwrap_or_else(|p| p.into_inner()).take() {
            Some(why) => AppError::msg(why),
            None => ssh_err(e),
        })?;
    authenticate(&mut session, ssh).await?;
    Ok(session)
}

async fn open_tunnel(ssh: &SshConfig, target_host: &str, target_port: u16) -> AppResult<SshTunnel> {
    let session = Arc::new(connect_session(ssh).await?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_port = listener.local_addr()?.port();
    let target_host = target_host.to_string();
    let accept_session = session.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, peer)) = listener.accept().await else { break };
            let session = accept_session.clone();
            let host = target_host.clone();
            tokio::spawn(async move {
                let _ = socket.set_nodelay(true);
                let channel = match session
                    .channel_open_direct_tcpip(host, target_port as u32, peer.ip().to_string(), peer.port() as u32)
                    .await
                {
                    Ok(c) => c,
                    Err(_) => return, // dropping the socket tells the client it failed
                };
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
            });
        }
    });
    Ok(SshTunnel { local_port, accept_task, session })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first key a host shows is recorded; the same one later passes, another is refused.
    #[test]
    fn host_keys_trusted_on_first_use() {
        let k = |s: &str| russh::keys::PublicKey::from_openssh(s).unwrap();
        let one = k("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFpOWZwegZe1YLMdfAJ34ibygmw5yRoGPtvILh5609Bb");
        let two = k("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIE5MZqX2fDi8meur0pdVrMnQjcBksJNQjJWskd59+X/o");
        let file = std::env::temp_dir().join(format!("devcat-known-hosts-{}", uuid::Uuid::new_v4()));
        let host = "bastion.devcat-test.invalid";
        assert_eq!(check_host_key(host, 22, &one, Some(&file)), Ok(()), "first use: recorded");
        assert_eq!(check_host_key(host, 22, &one, Some(&file)), Ok(()), "the same key again");
        assert!(check_host_key(host, 22, &two, Some(&file)).unwrap_err().contains("isn't the one recorded"), "a different key");
        assert_eq!(check_host_key(host, 2222, &two, Some(&file)), Ok(()), "another port is another host");
        assert!(check_host_key("evil\nother.example ssh-ed25519 AAAA", 22, &one, Some(&file)).is_err(), "no new lines through the name");
        let _ = std::fs::remove_file(&file);
    }

    fn base_cfg() -> ConnectionConfig {
        serde_json::from_value(serde_json::json!({
            "id": "t", "name": "t", "kind": "postgres", "host": "", "port": null, "user": "postgres"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn no_ssh_passthrough() {
        let mut cfg = base_cfg();
        let ep = open_endpoint(&cfg, 5432).await.unwrap();
        assert_eq!((ep.host.as_str(), ep.port), ("localhost", 5432));
        assert!(ep.tunnel.is_none());
        cfg.host = "db.example".into();
        cfg.port = Some(6000);
        cfg.ssh = Some(SshConfig { enabled: false, host: "bastion".into(), ..Default::default() });
        let ep = open_endpoint(&cfg, 5432).await.unwrap();
        assert_eq!((ep.host.as_str(), ep.port), ("db.example", 6000));
    }

    #[test]
    fn home_expansion() {
        let p = expand_home("~/.ssh/id_rsa");
        assert!(!p.to_string_lossy().starts_with('~'));
        assert_eq!(expand_home("/tmp/k"), PathBuf::from("/tmp/k"));
    }

    /// Needs a throwaway SSH server on the docker-compose network, e.g.
    /// `docker run -d --name devcat-ssh-test --network devcat-dev_default -p 52222:2222
    ///  -e PASSWORD_ACCESS=true -e USER_NAME=devcat -e USER_PASSWORD=devcat linuxserver/openssh-server`
    /// then set `AllowTcpForwarding yes` in /config/sshd/sshd_config and restart it.
    /// Env: DEVCAT_SSH_PORT (default 52222), DEVCAT_SSH_TARGET (default "postgres:5432").
    fn test_ssh_port() -> u16 {
        std::env::var("DEVCAT_SSH_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(52222)
    }

    /// The SSH server is not part of docker-compose; skip (pass) when it is not running.
    async fn ssh_server_running() -> bool {
        let up = tokio::net::TcpStream::connect(("127.0.0.1", test_ssh_port())).await.is_ok();
        if !up {
            eprintln!("skipping: no SSH test server on 127.0.0.1:{}", test_ssh_port());
        }
        up
    }

    #[tokio::test]
    #[ignore]
    async fn tunnel_echo_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if !ssh_server_running().await {
            return;
        }
        let port = test_ssh_port();
        let target = std::env::var("DEVCAT_SSH_TARGET").unwrap_or_else(|_| "postgres:5432".into());
        let (th, tp) = target.split_once(':').unwrap();
        let mut cfg = base_cfg();
        cfg.host = th.into();
        cfg.port = Some(tp.parse().unwrap());
        cfg.ssh = Some(SshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port,
            user: "devcat".into(),
            auth: "password".into(),
            key_path: None,
            password: Some("devcat".into()),
        });
        let ep = open_endpoint(&cfg, 5432).await.unwrap();
        assert_eq!(ep.host, "127.0.0.1");
        // Postgres answers an SSLRequest with a single 'S' or 'N' byte.
        let mut s = tokio::net::TcpStream::connect((ep.host.as_str(), ep.port)).await.unwrap();
        s.write_all(&[0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f]).await.unwrap();
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.unwrap();
        assert!(b[0] == b'S' || b[0] == b'N');
        drop(s);
        let port = ep.port;
        drop(ep);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    fn ssh_cfg(auth: &str, key_path: Option<&str>, password: &str) -> ConnectionConfig {
        let port = test_ssh_port();
        let mut cfg: ConnectionConfig = serde_json::from_value(serde_json::json!({
            "id": "ssh", "name": "ssh", "kind": "postgres", "host": "postgres", "port": 5432,
            "user": "postgres", "password": "devcat", "database": "devcat", "sslMode": "disable"
        }))
        .unwrap();
        cfg.ssh = Some(SshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port,
            user: "devcat".into(),
            auth: auth.into(),
            key_path: key_path.map(Into::into),
            password: Some(password.into()),
        });
        cfg
    }

    /// Full Postgres driver session through the tunnel; also key auth with a passphrase
    /// (key at /tmp/devcat_ssh_test_key, passphrase "keypass", added to authorized_keys).
    #[tokio::test]
    #[ignore]
    async fn postgres_driver_over_ssh() {
        if !ssh_server_running().await {
            return;
        }
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let d = crate::db::connect(&ssh_cfg("password", None, "devcat")).await.unwrap();
        let v = d.server_version().await.unwrap();
        assert!(v.contains("PostgreSQL"), "{v}");
        let opts = crate::db::ExecOptions { database: Some("devcat".into()), schema: None, max_rows: Some(10) };
        let r = d.execute("SELECT 41 + 1 AS x", &opts).await.unwrap();
        assert_eq!(r[0].rows[0][0], serde_json::json!(42));
        d.close().await;

        let bad = crate::db::connect(&ssh_cfg("password", None, "wrong")).await;
        assert!(bad.is_err_and(|e| e.to_string().contains("authentication failed")));

        let key = "/tmp/devcat_ssh_test_key";
        if std::path::Path::new(key).exists() {
            let d = crate::db::connect(&ssh_cfg("key", Some(key), "keypass")).await.unwrap();
            assert!(d.server_version().await.unwrap().contains("PostgreSQL"));
            d.close().await;
            let bad = crate::db::connect(&ssh_cfg("key", Some(key), "nope")).await;
            assert!(bad.is_err_and(|e| e.to_string().contains("cannot load key")));
        }
    }
}
