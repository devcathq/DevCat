//! Notifications on this phone: an agent on a paired computer finished or needs the user.
//! The computer decides what to notify (its "Notify me when" rules, `notify::to_phones` in
//! src-tauri) and sends it as the event `agent-notify`; this shows it while the app isn't on
//! screen (on screen the web page shows toasts), from Rust, so it works while Android has
//! paused the web page. With "Stay connected" on (Android, the keepalive plugin) the app keeps
//! running and every paired computer stays connected (`keep_connected`).
//!
//! Notifications sent while a computer was out of reach are fetched after reconnecting
//! (`notify_recent`): each one is numbered (`seq`, per run of DevCat: `boot`).
use crate::client::Client;
use crate::store::Store;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Shows a notification (title, body).
pub type Notifier = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// How often to reconnect a computer that dropped, while "Stay connected" is on.
const RECONNECT_EVERY: Duration = Duration::from_secs(30);
/// At most this many missed notifications after reconnecting (the newest).
const MAX_CATCH_UP: usize = 5;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    boot: String,
    seq: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    test: bool,
}

pub struct Watch {
    store: Arc<Store>,
    notifier: Notifier,
    /// Per computer: the newest notification seen (boot, seq).
    seen: Mutex<HashMap<String, (String, u64)>>,
    visible: AtomicBool,
    keep: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Watch {
    pub fn new(store: Arc<Store>, notifier: Notifier) -> Arc<Self> {
        let keep = store.prefs().background;
        Arc::new(Self { store, notifier, seen: Mutex::default(), visible: AtomicBool::new(true), keep: AtomicBool::new(keep), wake: tokio::sync::Notify::new() })
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
    }

    pub fn keep(&self) -> bool {
        self.keep.load(Ordering::Relaxed)
    }

    pub fn set_keep(&self, on: bool) {
        self.keep.store(on, Ordering::Relaxed);
        if on {
            self.wake.notify_one();
        }
    }

    fn show(&self, machine_id: &str, n: &Note) {
        // a test shows even on screen: that's where it's sent from
        if self.visible.load(Ordering::Relaxed) && !n.test {
            return;
        }
        // Which computer, when there's more than one.
        let machines = self.store.machines();
        let body = match machines.iter().find(|m| m.id == machine_id) {
            Some(m) if machines.len() > 1 => if n.body.is_empty() { m.name.clone() } else { format!("{}: {}", m.name, n.body) },
            _ => n.body.clone(),
        };
        (self.notifier)(&n.title, &body);
    }

    /// An event from a computer (`m-event`).
    pub fn on_event(&self, machine_id: &str, event: &str, p: &Value) {
        if event != "agent-notify" {
            return;
        }
        let Ok(n) = serde_json::from_value::<Note>(p.clone()) else { return };
        if !n.test {
            let mut seen = self.seen.lock();
            if seen.get(machine_id).is_some_and(|(boot, seq)| *boot == n.boot && *seq >= n.seq) {
                return; // already shown (caught up after reconnecting)
            }
            seen.insert(machine_id.to_string(), (n.boot.clone(), n.seq));
        }
        self.show(machine_id, &n);
    }

    /// The computer's latest notifications (its `notify_recent` answer), after connecting:
    /// show the ones sent while this phone wasn't connected.
    pub fn sync(&self, machine_id: &str, r: &Value) {
        let Some(boot) = r.get("boot").and_then(Value::as_str) else { return };
        let notes: Vec<Note> = r.get("notes").and_then(|n| serde_json::from_value(n.clone()).ok()).unwrap_or_default();
        let newest = notes.iter().map(|n| n.seq).max().unwrap_or(0);
        let missed: Vec<Note> = {
            let mut seen = self.seen.lock();
            let missed = match seen.get(machine_id) {
                // first contact since this app started: nothing to catch up on
                None => vec![],
                Some((b, last)) if b == boot => notes.into_iter().filter(|n| n.seq > *last).collect(),
                // DevCat restarted on the computer: everything it sent since is new here
                Some(_) => notes,
            };
            let last = seen.get(machine_id).filter(|(b, _)| b == boot).map_or(0, |(_, s)| *s);
            seen.insert(machine_id.to_string(), (boot.to_string(), newest.max(last)));
            missed
        };
        let skip = missed.len().saturating_sub(MAX_CATCH_UP);
        for n in missed.iter().skip(skip) {
            self.show(machine_id, n);
        }
    }

    pub fn forget(&self, machine_id: &str) {
        self.seen.lock().remove(machine_id);
    }

    /// A computer came online: catch up on what it notified while it was out of reach.
    pub fn connected(self: &Arc<Self>, client: Arc<Client>, machine_id: String) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(r) = client.call(&machine_id, "notify_recent", json!({})).await {
                me.sync(&machine_id, &r);
            }
        });
    }

    /// While "Stay connected" is on, reconnect every paired computer that dropped.
    pub async fn keep_connected(self: Arc<Self>, client: Arc<Client>) {
        loop {
            if self.keep() {
                let online = client.online();
                for m in self.store.machines() {
                    if !online.contains(&m.id) {
                        let _ = client.session(&m.id).await;
                    }
                }
            }
            let _ = tokio::time::timeout(RECONNECT_EVERY, self.wake.notified()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(name: &str) -> (Arc<Watch>, Arc<Mutex<Vec<(String, String)>>>) {
        let dir = std::env::temp_dir().join(format!("devcat-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let got: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let g = got.clone();
        let w = Watch::new(Arc::new(Store::new(dir)), Arc::new(move |t: &str, b: &str| g.lock().push((t.into(), b.into()))));
        w.set_visible(false);
        (w, got)
    }

    fn note(boot: &str, seq: u64, title: &str) -> Value {
        json!({ "boot": boot, "seq": seq, "at": 0, "title": title, "body": "b", "needsYou": false, "test": false })
    }

    fn titles(got: &Mutex<Vec<(String, String)>>) -> Vec<String> {
        got.lock().iter().map(|(t, _)| t.clone()).collect()
    }

    #[test]
    fn shows_live_notes_once_and_only_off_screen() {
        let (w, got) = watch("live");
        w.on_event("M", "agent-notify", &note("A", 1, "api: finished"));
        w.on_event("M", "agent-notify", &note("A", 1, "api: finished"));
        w.on_event("M", "agent-status", &json!({ "ptyId": "T1", "status": "idle" }));
        w.set_visible(true);
        w.on_event("M", "agent-notify", &note("A", 2, "web: needs you"));
        assert_eq!(titles(&got), vec!["api: finished"]);
        // a test shows on screen too
        w.on_event("M", "agent-notify", &json!({ "boot": "A", "seq": 3, "title": "DevCat", "body": "", "test": true }));
        assert_eq!(titles(&got), vec!["api: finished", "DevCat"]);
    }

    #[test]
    fn catches_up_after_reconnecting() {
        let (w, got) = watch("sync");
        // first contact: old notes aren't news
        w.sync("M", &json!({ "boot": "A", "notes": [note("A", 1, "old"), note("A", 2, "older")] }));
        assert!(got.lock().is_empty());
        // reconnected: 3 and 4 went out meanwhile
        w.sync("M", &json!({ "boot": "A", "notes": [note("A", 2, "older"), note("A", 3, "x: finished"), note("A", 4, "y: needs you")] }));
        assert_eq!(titles(&got), vec!["x: finished", "y: needs you"]);
        // and the live event of 4, arriving late, isn't shown twice
        w.on_event("M", "agent-notify", &note("A", 4, "y: needs you"));
        assert_eq!(got.lock().len(), 2);
    }

    #[test]
    fn computer_restarted_everything_since_is_new() {
        let (w, got) = watch("boot");
        w.on_event("M", "agent-notify", &note("A", 9, "before"));
        w.sync("M", &json!({ "boot": "B", "notes": [note("B", 1, "after restart")] }));
        assert_eq!(titles(&got), vec!["before", "after restart"]);
    }

    #[test]
    fn catch_up_is_capped() {
        let (w, got) = watch("cap");
        w.sync("M", &json!({ "boot": "A", "notes": [] }));
        let notes: Vec<Value> = (1..=8).map(|i| note("A", i, &format!("n{i}"))).collect();
        w.sync("M", &json!({ "boot": "A", "notes": notes }));
        assert_eq!(titles(&got), vec!["n4", "n5", "n6", "n7", "n8"]);
    }
}
