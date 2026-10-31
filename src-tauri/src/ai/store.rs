//! Persistence: AI connection profiles in `<data>/ai_providers.json`, API keys in the OS
//! keychain (`ai:<id>:apiKey`). Keys are write-only from the frontend: profiles only carry
//! a `hasApiKey` flag back.
use super::{AiKind, AiProvider, BUILTIN_ID};
use crate::error::{AppError, AppResult};
use crate::network::store::Secrets;
use std::path::PathBuf;

pub struct AiStore {
    path: PathBuf,
    secrets: Box<dyn Secrets>,
}

fn key_name(id: &str) -> String {
    format!("ai:{id}:apiKey")
}

/// Ids end up in keychain entry names: allow only uuid-like ids.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// POSIX-style environment variable name.
pub fn valid_env_name(k: &str) -> bool {
    let mut c = k.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The implicit default profile: Claude Code with its own login.
pub fn builtin_claude() -> AiProvider {
    AiProvider {
        id: BUILTIN_ID.into(),
        name: AiKind::Claude.label().into(),
        kind: AiKind::Claude,
        command: None,
        args: vec![],
        env: vec![],
        api_key_env: None,
        api_key: None,
        has_api_key: false,
        builtin: true,
        agent: None,
    }
}

impl AiStore {
    pub fn new(data_dir: PathBuf, secrets: Box<dyn Secrets>) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        Self { path: data_dir.join("ai_providers.json"), secrets }
    }

    fn load_saved(&self) -> AppResult<Vec<AiProvider>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(&self.path)?)?)
    }

    fn write_all(&self, list: &[AiProvider]) -> AppResult<()> {
        let clean: Vec<AiProvider> =
            list.iter().cloned().map(|p| AiProvider { api_key: None, has_api_key: false, builtin: false, ..p }).collect();
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&clean)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    /// Saved profiles, with the built-in Claude Code profile first unless it was customised.
    pub fn list(&self) -> AppResult<Vec<AiProvider>> {
        let mut all = self.load_saved()?;
        match all.iter().position(|p| p.id == BUILTIN_ID) {
            Some(i) => {
                let mut p = all.remove(i);
                p.builtin = true;
                all.insert(0, p);
            }
            None => all.insert(0, builtin_claude()),
        }
        for p in &mut all {
            p.has_api_key = self.secrets.get(&key_name(&p.id)).is_some();
        }
        Ok(all)
    }

    pub fn get(&self, id: &str) -> AppResult<AiProvider> {
        self.list()?.into_iter().find(|p| p.id == id).ok_or_else(|| AppError::msg(format!("AI connection {id} not found")))
    }

    pub fn api_key(&self, id: &str) -> Option<String> {
        self.secrets.get(&key_name(id)).filter(|k| !k.is_empty())
    }

    pub fn save(&self, mut p: AiProvider) -> AppResult<AiProvider> {
        if p.id.is_empty() {
            p.id = uuid::Uuid::new_v4().to_string();
        }
        if !valid_id(&p.id) {
            return Err(AppError::msg(format!("Invalid AI connection id \"{}\"", p.id)));
        }
        p.name = p.name.trim().to_string();
        if p.name.is_empty() {
            p.name = p.kind.label().into();
        }
        p.command = p.command.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        if p.kind == AiKind::Custom && p.command.is_none() {
            return Err(AppError::msg("A custom AI connection needs a command"));
        }
        if p.kind == AiKind::Agent {
            let a = p.agent.as_mut().ok_or_else(|| AppError::msg("Choose the model API for DevCat Agent"))?;
            a.base_url = a.base_url.trim().trim_end_matches('/').to_string();
            a.model = a.model.trim().to_string();
            if !(a.base_url.starts_with("https://") || a.base_url.starts_with("http://")) {
                return Err(AppError::msg("The API address must start with https:// or http://"));
            }
            if a.model.is_empty() {
                return Err(AppError::msg("Enter the model name, e.g. glm-5.3-flash"));
            }
            // It runs no program and sends the key itself: a variable name or variables left over
            // from another type (hidden in the form) mean nothing here, and mustn't block saving.
            p.api_key_env = None;
            p.env.clear();
        } else {
            p.agent = None;
        }
        p.args.retain(|a| !a.is_empty());
        p.env.retain(|e| !e.key.trim().is_empty());
        for e in &mut p.env {
            e.key = e.key.trim().to_string();
            if !valid_env_name(&e.key) {
                return Err(AppError::msg(format!("Invalid environment variable name \"{}\"", e.key)));
            }
        }
        p.api_key_env = p.api_key_env.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        if let Some(k) = &p.api_key_env {
            if !valid_env_name(k) {
                return Err(AppError::msg(format!("Invalid API key variable name \"{k}\"")));
            }
        }
        if let Some(key) = p.api_key.take() {
            if !key.trim().is_empty() && p.key_env().is_none() {
                return Err(AppError::msg("Choose the environment variable the API key is passed in"));
            }
            self.secrets.set(&key_name(&p.id), key.trim())?;
        }
        let mut all = self.load_saved()?;
        match all.iter_mut().find(|x| x.id == p.id) {
            Some(existing) => *existing = p.clone(),
            None => all.push(p.clone()),
        }
        self.write_all(&all)?;
        p.has_api_key = self.api_key(&p.id).is_some();
        p.builtin = p.id == BUILTIN_ID;
        Ok(p)
    }

    /// Delete a profile and its key. The built-in Claude Code profile is reset instead.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let mut all = self.load_saved()?;
        all.retain(|p| p.id != id);
        self.write_all(&all)?;
        self.secrets.delete(&key_name(id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiEnvVar;
    use crate::network::store::tests::MemSecrets;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("devcat-aistore-{}", uuid::Uuid::new_v4()))
    }

    pub fn provider(kind: AiKind) -> AiProvider {
        AiProvider { id: String::new(), name: String::new(), kind, ..builtin_claude() }
    }

    #[test]
    fn builtin_is_listed_and_resettable() {
        let dir = tmp();
        let secrets = MemSecrets::default();
        let store = AiStore::new(dir.clone(), Box::new(secrets.clone()));
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].builtin && list[0].id == BUILTIN_ID && list[0].kind == AiKind::Claude);

        // Customising the built-in keeps it first; deleting resets it.
        let mut c = list[0].clone();
        c.args = vec!["--verbose".into()];
        c.api_key = Some("sk-ant".into());
        let codex = store.save(provider(AiKind::Codex)).unwrap();
        let saved = store.save(c).unwrap();
        assert!(saved.has_api_key && saved.builtin);
        let list = store.list().unwrap();
        assert_eq!(list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![BUILTIN_ID, codex.id.as_str()]);
        assert_eq!(list[0].args, vec!["--verbose"]);
        store.delete(BUILTIN_ID).unwrap();
        let list = store.list().unwrap();
        assert!(list[0].args.is_empty() && !list[0].has_api_key);
        assert!(secrets.0.lock().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keys_stay_out_of_json() {
        let dir = tmp();
        let secrets = MemSecrets::default();
        let store = AiStore::new(dir.clone(), Box::new(secrets.clone()));
        let mut p = provider(AiKind::Codex);
        p.api_key = Some("  sk-secret ".into());
        p.env = vec![AiEnvVar { key: " OPENAI_BASE_URL ".into(), value: "http://x".into() }, AiEnvVar { key: "".into(), value: "y".into() }];
        let saved = store.save(p).unwrap();
        assert_eq!(saved.name, "OpenAI Codex");
        assert!(saved.has_api_key && saved.api_key.is_none());
        assert_eq!(saved.env.len(), 1);
        assert_eq!(saved.env[0].key, "OPENAI_BASE_URL");
        assert_eq!(store.api_key(&saved.id).as_deref(), Some("sk-secret"));
        let raw = std::fs::read_to_string(dir.join("ai_providers.json")).unwrap();
        assert!(!raw.contains("sk-secret") && !raw.contains("hasApiKey\": true"), "{raw}");
        // Wire format never carries the key back.
        assert!(!serde_json::to_string(&store.get(&saved.id).unwrap()).unwrap().contains("sk-secret"));

        // None keeps the key, "" clears it.
        let mut edit = store.get(&saved.id).unwrap();
        edit.name = "Work".into();
        assert!(store.save(edit.clone()).unwrap().has_api_key);
        edit.api_key = Some(String::new());
        assert!(!store.save(edit).unwrap().has_api_key);

        store.delete(&saved.id).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validation() {
        let dir = tmp();
        let store = AiStore::new(dir.clone(), Box::new(MemSecrets::default()));
        assert!(store.save(provider(AiKind::Custom)).is_err(), "custom needs a command");
        let mut p = provider(AiKind::Custom);
        p.command = Some("llm".into());
        p.api_key = Some("k".into());
        assert!(store.save(p.clone()).is_err(), "custom key needs a var name");
        p.api_key_env = Some("LLM_KEY".into());
        assert!(store.save(p.clone()).is_ok());
        p.id = "../x".into();
        assert!(store.save(p.clone()).is_err());
        let mut p = provider(AiKind::Aider);
        p.env = vec![AiEnvVar { key: "BAD-NAME".into(), value: "1".into() }];
        assert!(store.save(p).is_err());
        assert!(valid_env_name("_A1") && !valid_env_name("1A") && !valid_env_name(""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn devcat_agent_saves_whatever_another_type_left_in_the_form() {
        // #91: "Passed as" typed under Custom, then the type switched to DevCat Agent
        let dir = tmp();
        let store = AiStore::new(dir.clone(), Box::new(MemSecrets::default()));
        let mut p = provider(AiKind::Agent);
        p.agent = Some(crate::ai::AgentSettings { protocol: devcat_harness::Protocol::Anthropic, base_url: "https://api.anthropic.com/".into(), model: "claude-opus-5-5".into(), max_steps: None });
        p.api_key_env = Some("API Key".into());
        p.env = vec![AiEnvVar { key: "BAD-NAME".into(), value: "1".into() }];
        p.api_key = Some("sk-ant".into());
        let saved = store.save(p).unwrap();
        assert!(saved.api_key_env.is_none() && saved.env.is_empty());
        assert_eq!(store.api_key(&saved.id).as_deref(), Some("sk-ant"));
        assert_eq!(saved.agent.unwrap().base_url, "https://api.anthropic.com");
        let _ = std::fs::remove_dir_all(dir);
    }
}
