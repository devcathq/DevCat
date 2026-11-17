//! Notifications on this phone: an agent on a paired computer finished or needs the user.
//! The computer decides what to notify (its "Notify me when" rules, `notify::to_phones` in
//! src-tauri) and sends it as the event `agent-notify`; this shows it while the app isn't on
//! screen (on screen the web page shows toasts), from Rust, so it works while Android has
//! paused the web page. With "Stay connected" on (Android, the keepalive plugin) the app keeps
//! running and every paired computer stays connected (`keep_connected`).
//!
//! Notifications sent while a computer was out of reach are fetched after reconnecting
//! (`notify_recent`): each one is numbered (`seq`, per run of DevCat: `boot`).
//!
//! A computer with an older DevCat sends no `agent-notify` (it answers `notify_recent` with an
//! error): for it, this works the changes out itself from its agents' status (`Legacy`), as
//! "finished" and "needs you" without the computer's rules.
use crate::client::Client;
use crate::store::Store;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Shows a notification (title, body, what tapping it opens: `{machine, kind, …}`, App.tsx).
pub type Notifier = Arc<dyn Fn(&str, &str, Option<Value>) + Send + Sync>;

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
    /// What tapping it opens: `{kind: "chat", sessionId, cwd, providerId}` or `{kind: "terminal", ptyId}`.
    #[serde(default)]
    open: Option<Value>,
}

/// Tapping opens this terminal / chat.
fn open_terminal(pty_id: &str) -> Option<Value> {
    Some(json!({ "kind": "terminal", "ptyId": pty_id }))
}
fn open_chat(p: &Value, session_id: &str) -> Option<Value> {
    let provider = text(p, "providerId")?;
    Some(json!({ "kind": "chat", "sessionId": session_id, "cwd": text(p, "cwd").unwrap_or_default(), "providerId": provider }))
}

/// What an older computer's agents were doing, to notice "finished" / "needs you" here.
#[derive(Default)]
struct Legacy {
    /// Terminal agents by pty id: status.
    terms: HashMap<String, String>,
    /// DevCat Agent chats by session id: (busy, waiting).
    chats: HashMap<String, (bool, Option<String>)>,
}

fn folder(cwd: &str) -> String {
    cwd.split(['/', '\\']).filter(|s| !s.is_empty()).last().unwrap_or("agent").to_string()
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// A terminal agent's status changed: worth a notification? (title, body)
fn term_change(prev: Option<&String>, status: &str, cwd: &str, message: Option<&str>) -> Option<(String, String)> {
    if prev.map(String::as_str) == Some(status) {
        return None;
    }
    match status {
        "waiting" => Some((format!("{} needs you", folder(cwd)), message.unwrap_or("").to_string())),
        "idle" if prev.map(String::as_str) == Some("working") => Some((format!("{} finished", folder(cwd)), "The agent is done.".into())),
        _ => None,
    }
}

/// The same for a DevCat Agent chat.
fn chat_change(prev: Option<&(bool, Option<String>)>, busy: bool, waiting: Option<&str>, cwd: &str, title: &str) -> Option<(String, String)> {
    if let Some(w) = waiting {
        return (prev.and_then(|p| p.1.as_deref()) != Some(w)).then(|| (format!("{} needs you", folder(cwd)), w.to_string()));
    }
    (!busy && prev.is_some_and(|p| p.0)).then(|| (format!("{} finished", folder(cwd)), if title.is_empty() { "The agent is done.".into() } else { title.to_string() }))
}

pub struct Watch {
    store: Arc<Store>,
    notifier: Notifier,
    /// Per computer: the newest notification seen (boot, seq).
    seen: Mutex<HashMap<String, (String, u64)>>,
    /// Computers with an older DevCat (no `agent-notify`), and what their agents were doing.
    old: Mutex<HashSet<String>>,
    legacy: Mutex<HashMap<String, Legacy>>,
    visible: AtomicBool,
    keep: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Watch {
    pub fn new(store: Arc<Store>, notifier: Notifier) -> Arc<Self> {
        let keep = store.prefs().background.unwrap_or(cfg!(target_os = "android"));
        Arc::new(Self { store, notifier, seen: Mutex::default(), old: Mutex::default(), legacy: Mutex::default(), visible: AtomicBool::new(true), keep: AtomicBool::new(keep), wake: tokio::sync::Notify::new() })
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
        // which computer it's on goes with what to open
        let open = n.open.clone().map(|mut o| {
            o["machine"] = json!(machine_id);
            o
        });
        (self.notifier)(&n.title, &body, open);
    }

    /// An event from a computer (`m-event`).
    pub fn on_event(&self, machine_id: &str, event: &str, p: &Value) {
        if event != "agent-notify" {
            if self.old.lock().contains(machine_id) {
                self.legacy_event(machine_id, event, p);
            }
            return;
        }
        // it sends notifications itself after all (updated while connected)
        self.old.lock().remove(machine_id);
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
        self.old.lock().remove(machine_id);
        self.legacy.lock().remove(machine_id);
    }

    /// A computer came online: catch up on what it notified while it was out of reach. An older
    /// DevCat doesn't know `notify_recent`: watch its agents' status instead.
    pub fn connected(self: &Arc<Self>, client: Arc<Client>, machine_id: String) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            match client.call(&machine_id, "notify_recent", json!({})).await {
                Ok(r) => {
                    me.old.lock().remove(&machine_id);
                    me.sync(&machine_id, &r);
                }
                Err(_) => {
                    me.old.lock().insert(machine_id.clone());
                    if let Ok(r) = client.call(&machine_id, "agents", Value::Null).await {
                        me.legacy_sync(&machine_id, &r);
                    }
                }
            }
        });
    }

    /// An older computer's agent status event: notify what changed.
    fn legacy_event(&self, machine_id: &str, event: &str, p: &Value) {
        let mut out = None;
        let mut open = None;
        {
            let mut all = self.legacy.lock();
            let seen = all.entry(machine_id.to_string()).or_default();
            match event {
                "agent-status" => {
                    let (Some(id), Some(status)) = (text(p, "ptyId"), text(p, "status")) else { return };
                    out = term_change(seen.terms.get(&id), &status, &text(p, "cwd").unwrap_or_default(), text(p, "message").as_deref());
                    open = open_terminal(&id);
                    seen.terms.insert(id, status);
                }
                "agent-gone" => {
                    if let Some(id) = text(p, "ptyId") {
                        seen.terms.remove(&id);
                    }
                }
                "agent-chat" => {
                    let Some(id) = text(p, "sessionId") else { return };
                    let busy = p.get("busy").and_then(Value::as_bool).unwrap_or(false);
                    let waiting = text(p, "waiting");
                    out = chat_change(seen.chats.get(&id), busy, waiting.as_deref(), &text(p, "cwd").unwrap_or_default(), &text(p, "title").unwrap_or_default());
                    open = open_chat(p, &id);
                    seen.chats.insert(id, (busy, waiting));
                }
                "agent-chat-gone" => {
                    if let Some(id) = text(p, "sessionId") {
                        seen.chats.remove(&id);
                    }
                }
                _ => return,
            }
        }
        if let Some((title, body)) = out {
            self.show(machine_id, &Note { boot: String::new(), seq: 0, title, body, test: false, open });
        }
    }

    /// An older computer's agents (its `agents` answer) after connecting: notify what changed
    /// while away (nothing the first time), then remember them.
    fn legacy_sync(&self, machine_id: &str, r: &Value) {
        let mut found = Vec::new();
        {
            let mut all = self.legacy.lock();
            let had = all.contains_key(machine_id);
            let seen = all.entry(machine_id.to_string()).or_default();
            let mut terms = HashMap::new();
            for a in r.get("agents").and_then(Value::as_array).into_iter().flatten() {
                let (Some(id), Some(status)) = (text(a, "ptyId"), text(a, "status")) else { continue };
                if had && let Some((t, b)) = term_change(seen.terms.get(&id), &status, &text(a, "cwd").unwrap_or_default(), text(a, "message").as_deref()) {
                    found.push((t, b, open_terminal(&id)));
                }
                terms.insert(id, status);
            }
            let mut chats = HashMap::new();
            for c in r.get("chats").and_then(Value::as_array).into_iter().flatten() {
                let Some(id) = text(c, "sessionId") else { continue };
                let busy = c.get("busy").and_then(Value::as_bool).unwrap_or(false);
                let waiting = text(c, "waiting");
                if had && let Some((t, b)) = chat_change(seen.chats.get(&id), busy, waiting.as_deref(), &text(c, "cwd").unwrap_or_default(), &text(c, "title").unwrap_or_default()) {
                    found.push((t, b, open_chat(c, &id)));
                }
                chats.insert(id, (busy, waiting));
            }
            *seen = Legacy { terms, chats };
        }
        for (title, body, open) in found.into_iter().rev().take(MAX_CATCH_UP).rev() {
            self.show(machine_id, &Note { boot: String::new(), seq: 0, title, body, test: false, open });
        }
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
        let w = Watch::new(Arc::new(Store::new(dir)), Arc::new(move |t: &str, b: &str, _: Option<Value>| g.lock().push((t.into(), b.into()))));
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
    fn older_computer_status_changes_notify() {
        let (w, got) = watch("legacy");
        w.old.lock().insert("M".into());
        w.legacy_sync("M", &json!({ "agents": [{ "ptyId": "T1", "status": "working", "cwd": "/src/api" }], "chats": [] }));
        w.on_event("M", "agent-status", &json!({ "ptyId": "T1", "status": "waiting", "cwd": "/src/api", "message": "Allow Bash?" }));
        w.on_event("M", "agent-status", &json!({ "ptyId": "T1", "status": "working", "cwd": "/src/api" }));
        w.on_event("M", "agent-status", &json!({ "ptyId": "T1", "status": "idle", "cwd": "/src/api" }));
        w.on_event("M", "agent-chat", &json!({ "sessionId": "S", "busy": true, "cwd": "/w/web", "title": "Fix login" }));
        w.on_event("M", "agent-chat", &json!({ "sessionId": "S", "busy": false, "cwd": "/w/web", "title": "Fix login" }));
        assert_eq!(titles(&got), vec!["api needs you", "api finished", "web finished"]);
        // a computer that sends notifications itself: status changes don't notify twice
        w.on_event("M", "agent-notify", &note("A", 1, "x: finished"));
        w.on_event("M", "agent-status", &json!({ "ptyId": "T1", "status": "waiting", "cwd": "/src/api" }));
        assert_eq!(titles(&got).len(), 4);
    }

    #[test]
    fn catch_up_is_capped() {
        let (w, got) = watch("cap");
        w.sync("M", &json!({ "boot": "A", "notes": [] }));
        let notes: Vec<Value> = (1..=8).map(|i| note("A", i, &format!("n{i}"))).collect();
        w.sync("M", &json!({ "boot": "A", "notes": notes }));
        assert_eq!(titles(&got), vec!["n4", "n5", "n6", "n7", "n8"]);
    }

    #[test]
    fn tapping_opens_what_the_computer_named_on_that_computer() {
        let dir = std::env::temp_dir().join(format!("devcat-watch-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let got: Arc<Mutex<Vec<Option<Value>>>> = Arc::default();
        let g = got.clone();
        let w = Watch::new(Arc::new(Store::new(dir)), Arc::new(move |_: &str, _: &str, o: Option<Value>| g.lock().push(o)));
        w.set_visible(false);
        w.on_event("M", "agent-notify", &json!({ "boot": "A", "seq": 1, "title": "web: needs you", "body": "", "open": { "kind": "chat", "sessionId": "S", "cwd": "/w", "providerId": "devcat-agent" } }));
        w.on_event("M", "agent-notify", &json!({ "boot": "A", "seq": 2, "title": "job done", "body": "" }));
        let got = got.lock();
        assert_eq!(got[0].as_ref().unwrap()["sessionId"], "S");
        assert_eq!(got[0].as_ref().unwrap()["machine"], "M");
        assert!(got[1].is_none(), "nothing to open: the app just opens");
    }
}