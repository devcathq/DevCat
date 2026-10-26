//! Projects: user-created folders that group connections in the sidebar.
//! Stored as `projects.json` next to `connections.json`.
use crate::error::{AppError, AppResult};
use crate::models::Project;
use crate::store::ConnectionStore;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};

pub struct ProjectStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ProjectStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { path: data_dir.join("projects.json"), lock: Mutex::new(()) }
    }

    fn read(&self) -> AppResult<Vec<Project>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let mut list: Vec<Project> = serde_json::from_str(&std::fs::read_to_string(&self.path)?)?;
        list.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(list)
    }

    fn write(&self, list: &[Project]) -> AppResult<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(list)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> AppResult<Vec<Project>> {
        let _g = self.lock.lock();
        self.read()
    }

    /// Create (empty id) or update a project. Names must be unique (case-insensitive).
    pub fn save(&self, mut p: Project) -> AppResult<Project> {
        let _g = self.lock.lock();
        p.name = p.name.trim().to_string();
        if p.name.is_empty() {
            return Err(AppError::msg("Project name is required"));
        }
        let mut all = self.read()?;
        if all.iter().any(|x| x.id != p.id && x.name.eq_ignore_ascii_case(&p.name)) {
            return Err(AppError::msg(format!("A project named \"{}\" already exists", p.name)));
        }
        match all.iter_mut().find(|x| !p.id.is_empty() && x.id == p.id) {
            Some(existing) => {
                existing.name = p.name.clone();
                existing.color = p.color.clone();
                existing.description = p.description.clone();
                existing.folders = clean_folders(&p.folders);
                existing.mcp_writes = p.mcp_writes;
                p.folders = existing.folders.clone();
                p.order = existing.order;
            }
            None => {
                p.id = uuid::Uuid::new_v4().to_string();
                p.folders = clean_folders(&p.folders);
                p.order = all.iter().map(|x| x.order).max().map_or(0, |m| m + 1);
                all.push(p.clone());
            }
        }
        self.write(&all)?;
        Ok(p)
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        let _g = self.lock.lock();
        let mut all = self.read()?;
        all.retain(|p| p.id != id);
        self.write(&all)
    }

    /// Reorder projects to match `ids`; projects not listed keep their relative order after them.
    pub fn reorder(&self, ids: &[String]) -> AppResult<Vec<Project>> {
        let _g = self.lock.lock();
        let mut all = self.read()?;
        let pos = |id: &str| ids.iter().position(|x| x == id).unwrap_or(usize::MAX);
        all.sort_by_key(|p| pos(&p.id));
        for (i, p) in all.iter_mut().enumerate() {
            p.order = i as i64;
        }
        self.write(&all)?;
        Ok(all)
    }

    pub fn exists(&self, id: &str) -> AppResult<bool> {
        Ok(self.list()?.iter().any(|p| p.id == id))
    }

    /// One-time migration: turn legacy free-text `group` names into projects and
    /// point connections at them. Also clears `project_id`s that no longer exist.
    pub fn migrate(&self, conns: &ConnectionStore) -> AppResult<()> {
        let all = conns.load_all()?;
        let needs = all.iter().any(|c| c.group.as_deref().is_some_and(|g| !g.trim().is_empty()));
        let mut projects = self.list()?;
        let dangling = all.iter().any(|c| c.project_id.as_ref().is_some_and(|id| !projects.iter().any(|p| &p.id == id)));
        if !needs && !dangling {
            return Ok(());
        }
        for name in all.iter().filter_map(|c| c.group.as_deref().map(str::trim)).filter(|g| !g.is_empty()) {
            if !projects.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
                let p = self.save(Project { name: name.into(), ..Default::default() })?;
                projects.push(p);
            }
        }
        conns.update_all(|c| {
            if let Some(name) = c.group.take().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()) {
                if c.project_id.is_none() {
                    c.project_id = projects.iter().find(|p| p.name.eq_ignore_ascii_case(&name)).map(|p| p.id.clone());
                }
            }
            if c.project_id.as_ref().is_some_and(|id| !projects.iter().any(|p| &p.id == id)) {
                c.project_id = None;
            }
        })
    }
}

/// Folders as saved: trimmed, without trailing separators, each once.
fn clean_folders(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in list {
        let f = f.trim();
        let f = if f.len() > 1 { f.trim_end_matches(['/', '\\']) } else { f };
        if !f.is_empty() && !out.iter().any(|x| same_path(x, f)) {
            out.push(f.to_string());
        }
    }
    out
}

fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.replace('\\', "/").eq_ignore_ascii_case(&b.replace('\\', "/")) } else { a == b }
}

/// The project whose folder holds `dir` (the deepest folder wins when they nest).
pub fn for_folder<'a>(projects: &'a [Project], dir: &Path) -> Option<&'a Project> {
    let dir = comparable(dir);
    projects
        .iter()
        .flat_map(|p| p.folders.iter().map(move |f| (p, comparable(Path::new(f)))))
        .filter(|(_, f)| dir.starts_with(f))
        .max_by_key(|(_, f)| f.components().count())
        .map(|(p, _)| p)
}

/// A path for comparing: symlinks resolved when it exists (macOS /tmp = /private/tmp), and on
/// Windows in one case with `/` separators.
fn comparable(p: &Path) -> PathBuf {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if cfg!(windows) {
        let s = p.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/").to_lowercase();
        PathBuf::from(s)
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ConnectionConfig, ConnectionKind};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-projects-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn conn(id: &str, group: Option<&str>, project: Option<&str>) -> ConnectionConfig {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "kind": "sqlite", "group": group, "projectId": project,
        }))
        .unwrap()
    }

    fn p(name: &str) -> Project {
        Project { name: name.into(), ..Default::default() }
    }

    #[test]
    fn create_rename_unique_reorder_delete() {
        let s = ProjectStore::new(tmp());
        let a = s.save(p("Alpha")).unwrap();
        let b = s.save(p(" Beta ")).unwrap();
        assert_eq!(b.name, "Beta");
        assert_eq!((a.order, b.order), (0, 1));
        assert!(s.save(p("alpha")).is_err(), "names are unique case-insensitively");
        assert!(s.save(p("   ")).is_err());
        let renamed = s.save(Project { name: "Alpha 2".into(), color: Some("#22c55e".into()), ..a.clone() }).unwrap();
        assert_eq!(renamed.id, a.id);
        assert_eq!(renamed.order, 0, "rename keeps position");
        let order: Vec<_> = s.reorder(&[b.id.clone(), a.id.clone()]).unwrap().into_iter().map(|x| x.name).collect();
        assert_eq!(order, ["Beta", "Alpha 2"]);
        s.delete(&b.id).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn folders_are_saved_clean_and_found_deepest_first() {
        let s = ProjectStore::new(tmp());
        let a = s.save(Project { name: "Mono".into(), folders: vec![" /src/mono/ ".into(), "/src/mono".into(), "".into()], ..Default::default() }).unwrap();
        assert_eq!(a.folders, ["/src/mono"]);
        let b = s.save(Project { name: "Api".into(), folders: vec!["/src/mono/api".into()], mcp_writes: true, ..Default::default() }).unwrap();
        let edited = s.save(Project { folders: vec!["/src/mono/api".into(), "/src/api2/".into()], ..b.clone() }).unwrap();
        assert_eq!(edited.folders, ["/src/mono/api", "/src/api2"]);
        let all = s.list().unwrap();
        assert!(all.iter().find(|p| p.id == b.id).unwrap().mcp_writes);
        let at = |d: &str| for_folder(&all, Path::new(d)).map(|p| p.name.as_str());
        assert_eq!(at("/src/mono/api/src"), Some("Api"));
        assert_eq!(at("/src/mono/web"), Some("Mono"));
        assert_eq!(at("/src/mono"), Some("Mono"));
        assert_eq!(at("/src/monolith"), None);
        assert_eq!(at("/elsewhere"), None);
    }

    #[test]
    fn migrates_legacy_groups_and_dangling_ids() {
        let dir = tmp();
        let conns = ConnectionStore::new(dir.clone());
        let list = vec![
            conn("a", Some("Shop"), None),
            conn("b", Some("shop "), None),
            conn("c", Some("Infra"), None),
            conn("d", None, Some("gone")),
            conn("e", None, None),
        ];
        std::fs::write(dir.join("connections.json"), serde_json::to_string(&list).unwrap()).unwrap();
        let s = ProjectStore::new(dir.clone());
        s.migrate(&conns).unwrap();

        let projects = s.list().unwrap();
        let names: Vec<_> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Shop", "Infra"]);
        let by = |id: &str| conns.load_all().unwrap().into_iter().find(|c| c.id == id).unwrap();
        let shop = &projects[0].id;
        assert_eq!(by("a").project_id.as_ref(), Some(shop));
        assert_eq!(by("b").project_id.as_ref(), Some(shop), "group names merge case-insensitively");
        assert_eq!(by("c").project_id.as_ref(), Some(&projects[1].id));
        assert_eq!(by("d").project_id, None, "dangling project ids are cleared");
        assert_eq!(by("e").project_id, None);
        assert!(conns.load_all().unwrap().iter().all(|c| c.group.is_none()), "legacy groups cleared");
        let raw = std::fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(!raw.contains("\"group\""), "group is no longer written");

        // Idempotent
        s.migrate(&conns).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
        assert_eq!(by("a").kind, ConnectionKind::Sqlite);
    }
}
