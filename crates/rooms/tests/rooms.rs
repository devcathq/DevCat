//! Several DevCats in one process, talking over real iroh endpoints on localhost (relays off).
use async_trait::async_trait;
use devcat_rooms::doc::Message;
use devcat_rooms::{Change, Config, Host, JoinRequest, Node, RelayConfig, Role};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct TestHost {
    /// How the owner answers join requests.
    answer: Mutex<Option<Role>>,
    requests: Mutex<Vec<JoinRequest>>,
    messages: Mutex<Vec<(String, Message)>>,
    removed: Mutex<bool>,
}

#[async_trait]
impl Host for TestHost {
    async fn approve_join(&self, req: JoinRequest) -> Option<Role> {
        self.requests.lock().push(req);
        *self.answer.lock()
    }
    fn changed(&self, _room: &str, what: Change) {
        if what == Change::Removed {
            *self.removed.lock() = true;
        }
    }
    fn message(&self, room: &str, msg: &Message) {
        self.messages.lock().push((room.to_string(), msg.clone()));
    }
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("devcat-rooms-it-{}-{}-{name}", std::process::id(), devcat_rooms::now_ms()))
}

async fn start(dir: &std::path::Path, keeper: bool) -> (Node, Arc<TestHost>) {
    let host = Arc::new(TestHost::default());
    *host.answer.lock() = Some(Role::Editor);
    let cfg = Config { dir: dir.to_path_buf(), relay: RelayConfig::Off, port: 0, keeper, keeper_owners: vec![] };
    (Node::start(cfg, host.clone()).await.unwrap(), host)
}

async fn until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        if t.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invite_join_sync_remove() {
    let (da, db, dc) = (tmp("a"), tmp("b"), tmp("c"));
    let (a, ha) = start(&da, false).await;
    let (b, hb) = start(&db, false).await;
    let (c, hc) = start(&dc, false).await;

    let room = a.create("Q4 spec", "Arif").unwrap();
    let ra = a.room(&room).unwrap();
    ra.doc().create_file("plan.md", "The deadline is Friday.", &ra.me(false), 1).unwrap();

    // Joining needs the owner's approval; a refused request leaves no trace.
    *ha.answer.lock() = None;
    let link = a.invite(&room, Role::Editor).unwrap();
    assert!(b.join(&link, "Akbar").await.unwrap_err().contains("didn't let you in"));
    assert!(b.join(&link, "Akbar").await.unwrap_err().contains("used already"), "single use");
    *ha.answer.lock() = Some(Role::Editor);
    let link = a.invite(&room, Role::Editor).unwrap();
    assert_eq!(b.join(&link, "Akbar").await.unwrap(), room);
    assert_eq!(ha.requests.lock().last().unwrap().name, "Akbar");
    let rb = b.room(&room).unwrap();
    assert_eq!(rb.doc().read("plan.md").unwrap(), "The deadline is Friday.", "the document came with the welcome");
    assert_eq!(rb.role(), Role::Editor);

    // Live: an edit on one side shows up on the other.
    ra.doc().post(&ra.me(false), "@Akbar-agent can you check the dates?", 0, None, 2).unwrap();
    until("message reaches B", 20, || rb.doc().messages().len() == 1).await;
    until("B's host hears of it", 5, || hb.messages.lock().len() == 1).await;
    rb.doc().suggest("plan.md", "Friday", "Friday at 5pm", &rb.me(true), None, 3).unwrap();
    until("suggestion reaches A", 20, || ra.doc().suggestions().len() == 1).await;
    let sid = ra.doc().suggestions()[0].record.id.clone();
    ra.doc().decide(&sid, true, &ra.me(false), None, 4).unwrap();
    until("accepted change reaches B", 20, || rb.doc().read("plan.md").unwrap() == "The deadline is Friday at 5pm.").await;

    // C joins as a viewer, then is removed: new epoch, B gets the new key, C is cut off.
    let link = a.invite(&room, Role::Viewer).unwrap();
    *ha.answer.lock() = Some(Role::Viewer);
    c.join(&link, "Budi").await.unwrap();
    let rc = c.room(&room).unwrap();
    assert_eq!(rc.role(), Role::Viewer);
    until("B sees three members", 20, || rb.membership().members.len() == 3).await;
    ra.remove_member(&c.id()).unwrap();
    until("C learns it was removed", 30, || *hc.removed.lock()).await;
    until("B moves to the new epoch", 30, || rb.membership().epoch == 1).await;
    ra.doc().post(&ra.me(false), "secret plan after Budi left", 0, None, 5).unwrap();
    until("B gets messages in the new epoch", 30, || rb.doc().messages().iter().any(|m| m.text.contains("secret plan"))).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!rc.doc().messages().iter().any(|m| m.text.contains("secret plan")), "a removed member gets nothing new");
    assert!(rc.removed());

    // Blobs: shared sealed, fetched from whoever has it.
    ra.add_blob("logo.png", b"\x89PNG fake image bytes", "image/png", &ra.me(false)).unwrap();
    until("blob entry reaches B", 20, || rb.doc().files().iter().any(|f| f.path == "logo.png")).await;
    let hash = rb.doc().files().into_iter().find(|f| f.path == "logo.png").unwrap().meta.blob.unwrap().hash;
    assert_eq!(rb.blob(&hash).await.unwrap(), b"\x89PNG fake image bytes");
    assert!(rc.blob(&hash).await.is_err(), "not for a removed member");

    for n in [a, b, c] {
        n.shutdown().await;
    }
    for d in [da, db, dc] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_offline_edits_merge() {
    let (da, db) = (tmp("a2"), tmp("b2"));
    let (a, _ha) = start(&da, false).await;
    let room = a.create("Notes", "Arif").unwrap();
    a.room(&room).unwrap().doc().create_file("n.md", "The deadline is Friday.", &a.room(&room).unwrap().me(false), 1).unwrap();
    let (b, _hb) = start(&db, false).await;
    let link = a.invite(&room, Role::Editor).unwrap();
    b.join(&link, "Akbar").await.unwrap();
    b.shutdown().await;

    // B is offline; both edit.
    let ra = a.room(&room).unwrap();
    let t = ra.doc().read("n.md").unwrap();
    assert_eq!(t, "The deadline is Friday.");
    ra.doc().suggest("n.md", "Friday.", "Friday at 5pm.", &ra.me(false), None, 2).unwrap();
    let sid = ra.doc().suggestions()[0].record.id.clone();
    ra.doc().decide(&sid, true, &ra.me(false), None, 3).unwrap();

    // B comes back (same folder), edits offline-made, then syncs.
    let (b, _hb) = start(&db, false).await;
    let rb = b.room(&room).unwrap();
    rb.doc().suggest("n.md", "The deadline", "The final deadline", &rb.me(false), None, 4).unwrap();
    let sid = rb.doc().suggestions().into_iter().find(|s| s.record.base == "The deadline").unwrap().record.id;
    rb.doc().decide(&sid, true, &rb.me(false), None, 5).unwrap();
    until("both edits on A", 30, || ra.doc().read("n.md").unwrap() == "The final deadline is Friday at 5pm.").await;
    until("both edits on B", 30, || rb.doc().read("n.md").unwrap() == "The final deadline is Friday at 5pm.").await;

    a.shutdown().await;
    b.shutdown().await;
    let _ = std::fs::remove_dir_all(da);
    let _ = std::fs::remove_dir_all(db);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keeper_bridges_members_who_are_never_online_together() {
    let (da, db, dk) = (tmp("a3"), tmp("b3"), tmp("k3"));
    let (k, _hk) = start(&dk, true).await;
    let (a, _ha) = start(&da, false).await;
    let room = a.create("Async", "Arif").unwrap();
    let ra = a.room(&room).unwrap();
    // With relays off, A is told how to reach the keeper; B learns it from A.
    ra.add_keeper(&k.id(), "Keeper", Some(k.hints())).unwrap();
    let (b, _hb) = start(&db, false).await;
    b.join(&a.invite(&room, Role::Editor).unwrap(), "Akbar").await.unwrap();
    b.shutdown().await;

    // A writes while B is away, hands it to the keeper, and goes away too.
    ra.doc().post(&ra.me(false), "left for you", 0, None, 1).unwrap();
    ra.sync_keepers().await;
    a.shutdown().await;

    // B comes back with nobody but the keeper around.
    let (b, _hb) = start(&db, false).await;
    let rb = b.room(&room).unwrap();
    rb.sync_keepers().await;
    until("B gets A's message from the keeper", 30, || rb.doc().messages().iter().any(|m| m.text == "left for you")).await;
    // The keeper can't read it: what it stored is sealed.
    let log = std::fs::read_to_string(dk.join("keep").join(&room).join("log.jsonl")).unwrap();
    assert!(!log.contains("left for you"));

    b.shutdown().await;
    k.shutdown().await;
    for d in [da, db, dk] {
        let _ = std::fs::remove_dir_all(d);
    }
}
