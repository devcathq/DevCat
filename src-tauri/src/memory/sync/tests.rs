//! Two computers in one process, syncing over real iroh endpoints on localhost (relays off).
use super::node::{Config, Host, Node, PairRequest};
use crate::models::Project;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct TestHost {
    name: String,
    allow: Mutex<bool>,
    projects: Mutex<Vec<Project>>,
    requests: Mutex<Vec<PairRequest>>,
}

#[async_trait]
impl Host for TestHost {
    async fn approve(&self, req: PairRequest) -> bool {
        self.requests.lock().push(req);
        *self.allow.lock()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn projects(&self) -> Vec<Project> {
        self.projects.lock().clone()
    }
    fn changed(&self) {}
    fn memories_changed(&self) {}
}

struct Computer {
    node: Node,
    host: Arc<TestHost>,
    memory: PathBuf,
}

impl Computer {
    fn write(&self, rel: &str, text: &str) {
        let p = self.memory.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.memory.join(rel)).ok()
    }
    fn files(&self, dir: &str) -> Vec<String> {
        super::plan::files_in(&self.memory.join(dir))
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("devcat-msync-it-{}-{name}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn computer(name: &str) -> Computer {
    let d = tmp(name);
    let memory = d.join("memory");
    std::fs::create_dir_all(memory.join("global")).unwrap();
    let host = Arc::new(TestHost { name: name.into(), allow: Mutex::new(true), projects: Mutex::new(vec![]), requests: Mutex::new(vec![]) });
    let cfg = Config { dir: d.join("memory-sync"), memory: memory.clone(), relay: None, port: 0, auto: false };
    Computer { node: Node::start(cfg, host.clone()).await.unwrap(), host, memory }
}

fn memory(name: &str, body: &str, updated: &str) -> String {
    format!("---\nname: {name}\ndescription: about {name}\nmetadata:\n  type: project\n  origin: codex\n  updated: {updated}\n---\n\n{body}\n")
}

/// A git checkout with this origin, as a project's code folder.
fn repo(remote: &str) -> String {
    let d = tmp("repo");
    std::fs::create_dir_all(d.join(".git")).unwrap();
    std::fs::write(d.join(".git/config"), format!("[remote \"origin\"]\n\turl = {remote}\n")).unwrap();
    d.to_string_lossy().into()
}

fn project(id: &str, name: &str, folder: &str) -> Project {
    Project { id: id.into(), name: name.into(), color: None, description: None, order: 0, folders: vec![folder.into()], mcp_writes: false }
}

fn place_json(c: &Computer, dir: &str, id: &str) {
    c.write(&format!("{dir}/place.json"), &format!("{{\"kind\":\"project\",\"id\":\"{id}\"}}"));
}

fn mtime(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_sync_conflict_delete() {
    let a = computer("Laptop").await;
    let b = computer("Desk").await;

    // Pairing needs the code and the other side's yes.
    *a.host.allow.lock() = false;
    let (ticket, _, _) = a.node.pair_start().await;
    assert!(b.node.join(&ticket).await.unwrap_err().contains("declined"));
    assert!(b.node.join(&ticket).await.unwrap_err().contains("wrong or expired"), "a code works once");
    assert!(b.node.join("devcat://pair?v=1").await.is_err());
    *a.host.allow.lock() = true;
    let (ticket, _, _) = a.node.pair_start().await;
    assert!(a.node.join(&ticket).await.unwrap_err().contains("own code"));
    let peer = b.node.join(&ticket).await.unwrap();
    assert_eq!((peer.id.as_str(), peer.name.as_str()), (a.node.id().as_str(), "Laptop"));
    assert_eq!(a.host.requests.lock().last().unwrap().name, "Desk");
    assert_eq!(a.node.store().peers()[0].id, b.node.id(), "both sides remember each other");

    // New files travel both ways; secrets are scrubbed on the way in.
    a.write("global/style.md", &memory("style", "Short functions", "2026-01-01T00:00:00Z"));
    a.write("global/handoffs/20260101-codex.md", "---\norigin: codex\n---\n\nhalf done");
    b.write("global/db.md", &memory("db", "password = hunter22 for the dev db", "2026-01-01T00:00:00Z"));
    let r = b.node.sync_peer(&a.node.id()).await.unwrap();
    assert_eq!((r.received, r.sent, r.conflicts), (2, 1, 0), "{r:?}");
    assert_eq!(b.read("global/style.md").unwrap(), a.read("global/style.md").unwrap());
    assert!(b.read("global/handoffs/20260101-codex.md").is_some());
    let db = a.read("global/db.md").unwrap();
    assert!(db.contains("password = ***") && !db.contains("hunter22"), "{db}");

    // Nothing changed: nothing moves (the scrubbed copy comes back to b once, then it's quiet).
    let r = a.node.sync_peer(&b.node.id()).await.unwrap();
    assert_eq!((r.received, r.sent), (0, 1), "a hands back the scrubbed version: {r:?}");
    assert_eq!(b.read("global/db.md"), a.read("global/db.md"));
    let before = mtime(&b.memory.join("global/style.md"));
    let r = b.node.sync_peer(&a.node.id()).await.unwrap();
    assert!(!r.moved(), "{r:?}");
    assert_eq!(mtime(&b.memory.join("global/style.md")), before);

    // An edit on one side wins over the version it replaced.
    b.write("global/style.md", &memory("style", "Short functions, no abbreviations", "2026-02-01T00:00:00Z"));
    let r = a.node.sync_peer(&b.node.id()).await.unwrap();
    assert_eq!(r.received, 1, "{r:?}");
    assert!(a.read("global/style.md").unwrap().contains("no abbreviations"));

    // Both edit the same memory: the newer wins, the other is kept as a conflict copy on both.
    a.write("global/style.md", &memory("style", "Laptop's version", "2026-03-01T00:00:00Z"));
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    b.write("global/style.md", &memory("style", "Desk's version", "2026-03-02T00:00:00Z"));
    let r = a.node.sync_peer(&b.node.id()).await.unwrap();
    assert_eq!(r.conflicts, 1, "{r:?}");
    for c in [&a, &b] {
        assert!(c.read("global/style.md").unwrap().contains("Desk's version"), "the newer edit wins on {}", c.host.name);
        let copy = c.read("global/style-conflict-from-laptop.md").unwrap_or_else(|| panic!("no copy on {}: {:?}", c.host.name, c.files("global")));
        assert!(copy.contains("Laptop's version") && copy.contains("name: \"style (conflict from Laptop)\""), "{copy}");
    }

    // A deletion travels; an edit the deleting side hadn't seen survives it.
    std::fs::remove_file(a.memory.join("global/style-conflict-from-laptop.md")).unwrap();
    b.node.sync_peer(&a.node.id()).await.unwrap();
    assert!(b.read("global/style-conflict-from-laptop.md").is_none(), "deleted on b too");
    std::fs::remove_file(a.memory.join("global/db.md")).unwrap();
    b.write("global/db.md", &memory("db", "use the staging db", "2026-04-01T00:00:00Z"));
    b.node.sync_peer(&a.node.id()).await.unwrap();
    assert!(a.read("global/db.md").unwrap().contains("staging"), "the edit came back");

    // Unpaired: the other computer is refused.
    assert!(a.node.remove(&b.node.id()));
    assert!(b.node.sync_peer(&a.node.id()).await.unwrap_err().contains("doesn't know this computer"));
    assert!(b.node.store().peers()[0].last_error.is_some());
    a.node.shutdown().await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projects_match_by_git_remote() {
    let a = computer("Laptop").await;
    let b = computer("Desk").await;
    // the same repository in different folders, under different project ids
    *a.host.projects.lock() = vec![project("p-laptop", "Shop", &repo("git@github.com:acme/shop.git"))];
    *b.host.projects.lock() = vec![project("p-desk", "Shop (work)", &repo("https://github.com/Acme/shop"))];
    // a repository only the laptop has
    *a.host.projects.lock() = [a.host.projects(), vec![project("p-other", "Other", &repo("git@github.com:acme/other.git"))]].concat();

    place_json(&a, "projects/p-laptop", "p-laptop");
    a.write("projects/p-laptop/deploy.md", &memory("deploy", "make deploy", "2026-01-01T00:00:00Z"));
    place_json(&a, "projects/p-other", "p-other");
    a.write("projects/p-other/x.md", &memory("x", "only here", "2026-01-01T00:00:00Z"));

    let (ticket, _, _) = a.node.pair_start().await;
    b.node.join(&ticket).await.unwrap();
    let r = b.node.sync_peer(&a.node.id()).await.unwrap();
    assert_eq!((r.received, r.elsewhere), (1, 1), "{r:?}");
    assert!(b.read("projects/p-desk/deploy.md").unwrap().contains("make deploy"), "landed in b's own project");
    assert!(b.read("projects/p-desk/place.json").unwrap().contains("p-desk"));
    assert!(!b.memory.join("projects/p-other").exists() && !b.memory.join("projects/p-laptop").exists());
    a.node.shutdown().await;
    b.node.shutdown().await;
}
