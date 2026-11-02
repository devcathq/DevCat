//! What integrations decide without the network: commands, long text cut into messages, which
//! files may be sent to a chat app.
use std::path::{Path, PathBuf};

/// A `/command args` message (`/files@MyBot` in a Telegram group counts as `/files`); None for
/// plain text.
pub fn command(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (head, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let name = head.split('@').next().unwrap_or("").to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name, args.trim().to_string()))
}

/// `text` cut into messages of at most `max` characters, at line ends where it can.
pub fn split(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if cur.chars().count() + line.chars().count() > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur).trim_end().to_string());
        }
        if line.chars().count() > max {
            // one long line: hard cuts
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(max) {
                let p: String = piece.iter().collect();
                if cur.chars().count() + p.chars().count() > max && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur).trim_end().to_string());
                }
                cur.push_str(&p);
            }
        } else {
            cur.push_str(line);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim_end().to_string());
    }
    out.retain(|s| !s.trim().is_empty());
    out
}

/// At most `max` characters, with "…" when cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Files that look like keys or passwords: never sent to a chat app, whatever is asked.
pub fn secret_like(rel: &Path) -> bool {
    let dirs = [".ssh", ".git", ".aws", ".gnupg", ".kube", ".docker"];
    if rel.components().any(|c| dirs.contains(&c.as_os_str().to_string_lossy().as_ref())) {
        return true;
    }
    let name = rel.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let exact = [
        ".env", ".npmrc", ".netrc", ".pypirc", ".pgpass", "credentials", "credentials.json", "secrets.json", "secrets.yaml", "secrets.yml", ".htpasswd",
        // git's plaintext credential store, cloud / agent / tool logins
        ".credentials.json", "auth.json", "oauth_creds.json", "credentials.toml", "credentials.tfrc.json", ".terraformrc", ".dockercfg", ".boto",
        ".s3cfg", ".vault-token", "kubeconfig", ".yarnrc.yml",
    ];
    let ext = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx", ".ovpn", ".gpg", ".asc", ".ppk"];
    exact.contains(&name.as_str())
        || name.starts_with(".git-credentials")
        || name.starts_with(".env.")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || ext.iter().any(|e| name.ends_with(e))
}

/// The file a chat asked for (`asked`: relative to `folder`, or absolute), if it may be sent: an
/// existing file inside `folder` (after symlinks), at most `max` bytes, not a secret.
pub fn sendable(folder: &Path, asked: &str, max: u64) -> Result<PathBuf, String> {
    let asked = asked.trim().trim_matches(['"', '\'', '`']);
    if asked.is_empty() {
        return Err("Which file? `/file src/app.ts` (a path in the project folder).".into());
    }
    let root = folder.canonicalize().map_err(|e| format!("The project folder isn't there ({e})."))?;
    let p = Path::new(asked);
    let full = if p.is_absolute() { p.to_path_buf() } else { folder.join(p) };
    let real = full.canonicalize().map_err(|_| format!("There's no file {asked} in the project folder."))?;
    let Ok(rel) = real.strip_prefix(&root) else { return Err("Only files inside the project folder can be sent.".into()) };
    if secret_like(rel) || secret_like(Path::new(asked)) {
        return Err(format!("{asked} looks like it holds keys or passwords, so it isn't sent to the chat. Open it on the computer."));
    }
    let meta = real.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("{asked} is a folder: ask for a file in it."));
    }
    if meta.len() > max {
        return Err(format!("{asked} is {} MB, more than this chat app takes ({} MB).", meta.len() / 1_000_000, max / 1_000_000));
    }
    Ok(real)
}

/// A random id for a button (letters and digits).
pub fn token(len: usize) -> String {
    const ALPHA: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let bytes: [u8; 32] = crate::remote::store::random_bytes();
    bytes.iter().take(len).map(|b| ALPHA[(*b as usize) % ALPHA.len()] as char).collect()
}

/// A one-time link code: 8 characters people can type (no 0/O, 1/l).
pub fn link_code() -> String {
    let t = token(8).to_ascii_uppercase();
    format!("{}-{}", &t[..4], &t[4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_plain_text() {
        assert_eq!(command("/files"), Some(("files".into(), String::new())));
        assert_eq!(command("  /File@DevCatBot  src/a.ts "), Some(("file".into(), "src/a.ts".into())));
        assert_eq!(command("/link ab12-cd34"), Some(("link".into(), "ab12-cd34".into())));
        assert_eq!(command("fix the login bug"), None);
        assert_eq!(command("/"), None);
        assert_eq!(command("/usr/bin is a path"), None, "not a command name");
    }

    #[test]
    fn long_text_is_cut_at_lines() {
        assert_eq!(split("a\nb\nc", 100), ["a\nb\nc"]);
        assert_eq!(split("aaaa\nbbbb\ncccc", 10), ["aaaa\nbbbb", "cccc"]);
        let long = "x".repeat(25);
        assert_eq!(split(&long, 10), ["x".repeat(10), "x".repeat(10), "x".repeat(5)]);
        assert!(split("", 10).is_empty());
        assert_eq!(clip("hello world", 6), "hello…");
        assert_eq!(clip("hi", 6), "hi");
    }

    #[test]
    fn secrets_stay_on_the_computer() {
        for s in [".env", ".env.production", "config/id_rsa", "certs/server.pem", ".ssh/config", ".git/config", "deploy/secrets.yaml", "a/.aws/credentials"] {
            assert!(secret_like(Path::new(s)), "{s}");
        }
        for s in ["src/app.ts", "README.md", "env.ts", "docs/keys.md", "environment.yml"] {
            assert!(!secret_like(Path::new(s)), "{s}");
        }
    }

    #[test]
    fn only_files_in_the_folder_are_sendable() {
        let dir = std::env::temp_dir().join(format!("devcat-int-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.ts"), "x").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1").unwrap();
        std::fs::write(dir.join("big.bin"), vec![0u8; 2000]).unwrap();
        assert!(sendable(&dir, "src/a.ts", 1000).is_ok());
        assert!(sendable(&dir, &dir.join("src/a.ts").to_string_lossy(), 1000).is_ok());
        assert!(sendable(&dir, "../../etc/hosts", 1000).is_err(), "outside");
        assert!(sendable(&dir, "/etc/hosts", 1000).is_err(), "outside, absolute");
        assert!(sendable(&dir, ".env", 1000).unwrap_err().contains("keys or passwords"));
        assert!(sendable(&dir, "src", 1000).unwrap_err().contains("folder"));
        assert!(sendable(&dir, "big.bin", 1000).unwrap_err().contains("MB"));
        assert!(sendable(&dir, "nope.ts", 1000).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn codes_are_typeable() {
        let c = link_code();
        assert_eq!(c.len(), 9);
        assert!(c.chars().all(|ch| ch == '-' || ch.is_ascii_uppercase() || ch.is_ascii_digit()));
        assert!(!c.contains('0') && !c.contains('O') && !c.contains('1'));
        assert_ne!(c, link_code());
    }
}
