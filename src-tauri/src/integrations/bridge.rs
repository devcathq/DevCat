//! Between chat apps and DevCat Agent: what a linked chat says (commands, requests, button presses)
//! and what the agents do (approvals, questions, results) for the chats linked to their folder.
//! Who may do what is decided here, on every message: only a linked chat, only its listed people,
//! only what its link allows (`Perms`).
use super::transport::{Ack, Button, Inbound, Outgoing, Sender, Transport};
use super::{Link, LinkUser, Note, inside, link, links, set_session, text, transport};
use crate::agent;
use devcat_harness::{Decision, Policy};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// What a button does when pressed (its data is `x:<token>`), and in which link.
#[derive(Debug, Clone)]
enum Action {
    Approve { session: String, tool_use_id: String, allow: bool },
    Answer { session: String, tool_use_id: String, answer: String },
    SendFile { path: String },
    Files,
    Summary,
}

struct Pending {
    link_id: String,
    action: Action,
    at: i64,
}

static ACTIONS: Lazy<Mutex<HashMap<String, Pending>>> = Lazy::new(Mutex::default);
/// The question each link was asked last (session, tool use): a plain reply answers it.
static QUESTIONS: Lazy<Mutex<HashMap<String, (String, String)>>> = Lazy::new(Mutex::default);

const ACTION_TTL_MS: i64 = 24 * 3_600_000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn button(link_id: &str, label: &str, action: Action) -> Button {
    let mut actions = ACTIONS.lock();
    let now = now_ms();
    actions.retain(|_, p| now - p.at < ACTION_TTL_MS);
    let token = text::token(12);
    actions.insert(token.clone(), Pending { link_id: link_id.into(), action, at: now });
    Button::action(label, &format!("x:{token}"))
}

fn project(folder: &str) -> String {
    Path::new(folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.to_string())
}

/// What a newly linked chat is told.
pub fn welcome(l: &Link) -> String {
    let mut can = Vec::new();
    if l.perms.requests {
        can.push("send me requests: just write what you want done");
    }
    if l.perms.approve {
        can.push("allow or deny what the agent asks");
    }
    if l.perms.notify {
        can.push("get told when agents in this folder finish or need you");
    }
    if l.perms.files {
        can.push("get files from the folder (/files, /file)");
    }
    format!("✅ Linked to {} ({}).\nHere you can {}.\n\n/help lists the commands.", project(&l.folder), l.folder, can.join("; "))
}

fn help(l: &Link) -> String {
    let mut lines = vec![format!("DevCat · {} ({})", project(&l.folder), l.folder)];
    if l.perms.requests {
        lines.push("Write a message: the agent works on it in this folder.".into());
        lines.push("/new [request] — start a fresh agent chat".into());
        lines.push("/stop — stop what the agent is doing".into());
    }
    lines.push("/status — what the agent is doing".into());
    lines.push("/summary — the agent's last answer and the files it changed".into());
    if l.perms.files {
        lines.push("/files — files the agent changed (tap one to get it)".into());
        lines.push("/file <path> — get a file from the folder".into());
    }
    lines.push("/open — open this chat in DevCat Mobile (on a phone paired with the computer)".into());
    lines.push("/unlink — unlink this chat from DevCat".into());
    lines.join("\n")
}

/// The link of a chat, if `from` is one of the people it listens to.
fn linked(bot_id: &str, chat: &str, from: &Sender) -> Option<Link> {
    links().into_iter().find(|l| l.bot_id == bot_id && l.chat_id == chat && l.users.iter().any(|u| u.id == from.id))
}

async fn say(t: &Arc<dyn Transport>, chat: &str, msg: impl Into<String>) {
    for part in text::split(&msg.into(), t.max_text()) {
        let _ = t.send(chat, &Outgoing::text(part)).await;
    }
}

/// Something came in from a chat app.
pub async fn handle(bot_id: &str, t: Arc<dyn Transport>, inbound: Inbound) {
    match inbound {
        Inbound::Message { chat, chat_name, private, from, text: body } => {
            let cmd = text::command(&body);
            // linking: from any chat, with the code DevCat shows
            if let Some((name, args)) = &cmd
                && name == "link"
            {
                // a Discord server channel has no name in the message: ask for it
                let discord = if t.kind() == super::BotKind::Discord && !private { super::WARNINGS.lock().get(bot_id).cloned() } else { None };
                let chat_name = match discord {
                    Some(d) => d.channel_name(&chat).await.unwrap_or(chat_name),
                    None => chat_name,
                };
                let reply = super::link_asked(bot_id, &chat, &chat_name, private, LinkUser { id: from.id.clone(), name: from.name.clone() }, args);
                say(&t, &chat, reply).await;
                return;
            }
            let Some(l) = linked(bot_id, &chat, &from) else {
                // someone else: only a hint, in a private chat, to /start
                if private && matches!(&cmd, Some((n, _)) if n == "start" || n == "help") {
                    say(&t, &chat, "This bot belongs to a DevCat. To link this chat: in DevCat open Settings → Agent bots → Link a chat, then send /link and the code it shows.").await;
                }
                return;
            };
            match cmd {
                Some((name, args)) => command(&t, &l, &name, &args).await,
                None => message(&t, &l, &body).await,
            }
        }
        Inbound::Press { chat, from, data, ack } => {
            let Some(token) = data.strip_prefix("x:") else { return };
            let pending = ACTIONS.lock().get(token).map(|p| (p.link_id.clone(), p.action.clone()));
            let Some((link_id, action)) = pending else {
                let _ = t.ack(&ack, "(This button has expired.)").await;
                return;
            };
            // only in its chat, and only the people the link listens to (others can't press for them)
            let Some(l) = link(&link_id).filter(|l| l.chat_id == chat && l.users.iter().any(|u| u.id == from.id)) else { return };
            press(&t, &l, &from, &ack, token, action).await;
        }
    }
}

async fn press(t: &Arc<dyn Transport>, l: &Link, from: &Sender, ack: &Ack, token: &str, action: Action) {
    match action {
        Action::Approve { session, tool_use_id, allow } => {
            if !l.perms.approve {
                let _ = t.ack(ack, "(This chat can't answer approvals anymore.)").await;
                return;
            }
            let decision = if allow { Decision::Allow } else { Decision::Deny { reason: Some(format!("Denied from {} by {}", t.kind().label(), from.name)) } };
            let done = match agent::agent_approve(session, tool_use_id, decision).await {
                Ok(()) if allow => format!("✅ Allowed by {}", from.name),
                Ok(()) => format!("❌ Denied by {}", from.name),
                Err(_) => "(Already answered, or the chat is closed.)".into(),
            };
            ACTIONS.lock().remove(token);
            let _ = t.ack(ack, &done).await;
        }
        Action::Answer { session, tool_use_id, answer } => {
            if !l.perms.approve {
                return;
            }
            QUESTIONS.lock().remove(&l.id);
            let done = match agent::agent_answer(session, tool_use_id, answer.clone()).await {
                Ok(()) => format!("💬 {} answered: {answer}", from.name),
                Err(_) => "(Already answered, or the chat is closed.)".into(),
            };
            let _ = t.ack(ack, &done).await;
        }
        Action::SendFile { path } => {
            let _ = t.ack(ack, &format!("📎 {}", Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).await;
            send_file(t, l, &path).await;
        }
        Action::Files => {
            let _ = t.ack(ack, "📁").await;
            files(t, l).await;
        }
        Action::Summary => {
            let _ = t.ack(ack, "📝").await;
            summary(t, l).await;
        }
    }
}

async fn command(t: &Arc<dyn Transport>, l: &Link, name: &str, args: &str) {
    match name {
        "start" | "help" => say(t, &l.chat_id, help(l)).await,
        "new" => {
            if !l.perms.requests {
                return say(t, &l.chat_id, "This chat can't send requests (DevCat → Agent bots).").await;
            }
            set_session(&l.id, None);
            let l = link(&l.id).unwrap_or_else(|| l.clone());
            if args.is_empty() {
                say(t, &l.chat_id, "Fresh chat: what should the agent do?").await;
            } else {
                request(t, &l, args).await;
            }
        }
        "stop" => match current(l) {
            Some(s) if l.perms.requests => {
                let _ = agent::agent_stop(s).await;
                say(t, &l.chat_id, "⏹ Stopped.").await;
            }
            _ => say(t, &l.chat_id, "Nothing to stop.").await,
        },
        "status" => {
            let msg = match current(l).and_then(|s| agent::meta(&s)) {
                Some(m) if m.busy => format!("⏳ Working: {}", if m.title.is_empty() { "the last request" } else { &m.title }),
                Some(m) => format!("💤 Idle. Last chat: {}", if m.title.is_empty() { "(untitled)" } else { &m.title }),
                None => "No agent chat yet: send a request to start one.".into(),
            };
            say(t, &l.chat_id, msg).await;
        }
        "summary" => summary(t, l).await,
        "files" => files(t, l).await,
        "file" => {
            if !l.perms.files {
                return say(t, &l.chat_id, "This chat can't get files (DevCat → Agent bots).").await;
            }
            match text::sendable(Path::new(&l.folder), args, t.max_file()) {
                Ok(p) => send_file(t, l, &p.to_string_lossy()).await,
                Err(e) => say(t, &l.chat_id, e).await,
            }
        }
        "open" => {
            let msg = match current(l).and_then(|s| agent::meta(&s).map(|m| (s, m))).and_then(|(s, m)| crate::remote::chat_link(&s, &m.cwd, &m.provider_id)) {
                Some(url) => Outgoing { text: "Open the agent's chat in DevCat Mobile (on a phone or browser already paired with this computer).".into(), buttons: vec![vec![Button::Url { label: "Open in DevCat".into(), url }]] },
                None if current(l).is_none() => Outgoing::text("No agent chat yet: send a request first."),
                None => Outgoing::text("To get a link, turn on Allow phone browsers in DevCat → Mobile pairing, and pair your phone there."),
            };
            let _ = t.send(&l.chat_id, &msg).await;
        }
        "unlink" => {
            let _ = super::integrations_link_remove(l.id.clone()).await;
        }
        "link" => {}
        _ => say(t, &l.chat_id, format!("I don't know /{name}. /help lists the commands.")).await,
    }
}

/// Plain text: the answer to the agent's open question, else a request.
async fn message(t: &Arc<dyn Transport>, l: &Link, body: &str) {
    let question = QUESTIONS.lock().get(&l.id).cloned();
    if let Some((session, tool_use_id)) = question.filter(|_| l.perms.approve) {
        QUESTIONS.lock().remove(&l.id);
        return match agent::agent_answer(session, tool_use_id, body.to_string()).await {
            Ok(()) => say(t, &l.chat_id, "💬 Answered.").await,
            // answered on the computer meanwhile: not a request nobody meant to send
            Err(_) => say(t, &l.chat_id, "That question was already answered. If this is a new request, send it again.").await,
        };
    }
    if !l.perms.requests {
        return say(t, &l.chat_id, "This chat gets notifications only. To send requests, allow it in DevCat → Agent bots.").await;
    }
    request(t, l, body).await;
}

/// The link's agent chat, if it's open (or saved: it opens again on the next request).
fn current(l: &Link) -> Option<String> {
    link(&l.id).and_then(|l| l.session_id)
}

/// Send a request to the link's agent chat, opening it (or a new one) first.
async fn request(t: &Arc<dyn Transport>, l: &Link, body: &str) {
    let origin = format!("{}:{}", t.kind().label().to_lowercase(), l.chat_name);
    let owner = format!("integration:{}", l.id);
    let saved = current(l);
    let opened = agent::open(l.folder.clone(), l.provider_id.clone(), saved.clone(), &origin, &owner, None, None, l.perms.connections).await;
    let session = match opened {
        Ok(o) => o.session_id,
        Err(e) => return say(t, &l.chat_id, format!("⚠️ Couldn't start the agent: {e}")).await,
    };
    if saved.as_deref() != Some(session.as_str()) {
        set_session(&l.id, Some(session.clone()));
    }
    let policy = if l.perms.auto_edit { Policy::AutoEdit } else { Policy::Ask };
    match agent::agent_send(session, body.to_string(), policy, None, None, None, None).await {
        Ok(()) => {
            let _ = t.send(&l.chat_id, &Outgoing::text("⏳ On it.")).await;
        }
        Err(e) => say(t, &l.chat_id, format!("⚠️ {e}")).await,
    }
}

async fn summary(t: &Arc<dyn Transport>, l: &Link) {
    let Some(s) = current(l) else { return say(t, &l.chat_id, "No agent chat yet.").await };
    let answer = agent::last_answer(&s).unwrap_or_else(|| "(No answer yet.)".into());
    let changed = changed_in(l, &s);
    let mut msg = answer;
    if !changed.is_empty() {
        msg.push_str(&format!("\n\n📝 Changed {} file{}: {}", changed.len(), if changed.len() == 1 { "" } else { "s" }, changed.iter().map(|p| rel(l, p)).collect::<Vec<_>>().join(", ")));
    }
    say(t, &l.chat_id, msg).await;
}

/// Files the chat's agent changed, in the link's folder.
fn changed_in(l: &Link, session: &str) -> Vec<String> {
    agent::changed_files(session).into_iter().filter(|p| inside(p, &l.folder)).collect()
}

fn rel(l: &Link, p: &str) -> String {
    Path::new(p).strip_prefix(&l.folder).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
}

async fn files(t: &Arc<dyn Transport>, l: &Link) {
    if !l.perms.files {
        return say(t, &l.chat_id, "This chat can't get files (DevCat → Agent bots).").await;
    }
    let changed = current(l).map(|s| changed_in(l, &s)).unwrap_or_default();
    if changed.is_empty() {
        return say(t, &l.chat_id, "The agent hasn't changed files in this chat. /file <path> gets any file in the folder.").await;
    }
    let rows: Vec<Vec<Button>> = changed.iter().take(10).map(|p| vec![button(&l.id, &format!("📎 {}", text::clip(&rel(l, p), 60)), Action::SendFile { path: p.clone() })]).collect();
    let more = if changed.len() > 10 { format!(" (the first 10 of {})", changed.len()) } else { String::new() };
    let _ = t.send(&l.chat_id, &Outgoing { text: format!("Files the agent changed{more}: tap one to get it."), buttons: rows }).await;
}

async fn send_file(t: &Arc<dyn Transport>, l: &Link, path: &str) {
    if !l.perms.files {
        return;
    }
    // checked again when sent: the folder, size and secrets may have changed since the button
    let p = match text::sendable(Path::new(&l.folder), path, t.max_file()) {
        Ok(p) => p,
        Err(e) => return say(t, &l.chat_id, e).await,
    };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    match tokio::fs::read(&p).await {
        Ok(bytes) => {
            if let Err(e) = t.send_file(&l.chat_id, &name, bytes, &rel(l, &p.to_string_lossy())).await {
                say(t, &l.chat_id, format!("⚠️ Couldn't send {name}: {e}")).await;
            }
        }
        Err(e) => say(t, &l.chat_id, format!("⚠️ Couldn't read {name}: {e}")).await,
    }
}

// ---------------------------------------------------------------- agent events → chats

/// An event of a DevCat Agent chat (from `agent.rs`, every event of every chat): approvals,
/// questions and finished turns go to the chats linked to its folder, and "finished" / "needs you"
/// to the notification channels.
pub fn agent_event(session_id: &str, event: &Value) {
    if !matches!(event["type"].as_str(), Some("approval" | "question" | "turnEnd")) {
        return;
    }
    let (session, event) = (session_id.to_string(), event.clone());
    crate::rt::spawn(async move { on_agent(&session, &event).await });
}

async fn on_agent(session: &str, event: &Value) {
    let Some(m) = agent::meta(session) else { return };
    if m.ghost {
        return; // nothing of a ghost chat leaves its window
    }
    let all = links();
    // the link the chat belongs to (its requests), and the ones linked to its folder
    let own: Option<Link> = m.owner.strip_prefix("integration:").and_then(|id| all.iter().find(|l| l.id == id).cloned());
    let in_folder: Vec<Link> = all.iter().filter(|l| inside(&m.cwd, &l.folder) && own.as_ref().is_none_or(|o| o.id != l.id)).cloned().collect();
    let name = project(&m.cwd);
    let url = crate::remote::chat_link(session, &m.cwd, &m.provider_id);
    let notify = crate::notify::load();
    // a scheduled job's chat notifies through agent_jobs.rs (`notify::job`): not twice
    let away = (!notify.only_when_away || !crate::notify::app_focused()) && m.owner != agent::SCHEDULE;
    match event["type"].as_str() {
        Some("approval") => {
            let req = &event["request"];
            let approvers: Vec<&Link> = own.iter().chain(in_folder.iter()).filter(|l| l.perms.approve).collect();
            for l in &approvers {
                ask_approval(l, session, &name, req, url.as_deref()).await;
            }
            if own.is_none() && away {
                let what = req["summary"].as_str().unwrap_or("");
                super::deliver(Note {
                    title: format!("{name}: needs you"),
                    body: format!("Allow {}? {}", req["tool"].as_str().unwrap_or("this"), text::clip(what, 300)),
                    needs_you: true,
                    cwd: Some(m.cwd.clone()),
                    skip_links: approvers.iter().map(|l| l.id.clone()).collect(),
                    url,
                })
                .await;
            }
        }
        Some("question") => {
            let q = &event["question"];
            let askers: Vec<&Link> = own.iter().chain(in_folder.iter()).filter(|l| l.perms.approve).collect();
            for l in &askers {
                ask_question(l, session, &name, q).await;
            }
            if own.is_none() && away {
                super::deliver(Note {
                    title: format!("{name}: asks you"),
                    body: text::clip(q["question"].as_str().unwrap_or(""), 500),
                    needs_you: true,
                    cwd: Some(m.cwd.clone()),
                    skip_links: askers.iter().map(|l| l.id.clone()).collect(),
                    url,
                })
                .await;
            }
        }
        Some("turnEnd") => {
            let elapsed = event["elapsedMs"].as_i64().unwrap_or(0);
            if let Some(l) = &own {
                result(l, session, event, url.clone()).await;
                return; // asked from a chat app: the answer goes there, not to every channel
            }
            if notify.on_done && away && elapsed >= i64::from(notify.min_seconds) * 1000 {
                let answer = agent::last_answer(session).unwrap_or_default();
                let title = if m.title.is_empty() { format!("{name}: finished") } else { format!("{name}: finished · {}", text::clip(&m.title, 60)) };
                let body = match event["end"].as_str() {
                    Some("error") => format!("⚠️ {}", event["error"].as_str().unwrap_or("It stopped with an error.")),
                    Some("stopped") => "Stopped.".into(),
                    _ => text::clip(&answer, 600),
                };
                super::deliver(Note { title, body, needs_you: false, cwd: Some(m.cwd.clone()), skip_links: vec![], url }).await;
            }
        }
        _ => {}
    }
}

async fn ask_approval(l: &Link, session: &str, name: &str, req: &Value, url: Option<&str>) {
    let Some(t) = transport(&l.bot_id) else { return };
    let tool = req["tool"].as_str().unwrap_or("a tool");
    let summary = text::clip(req["summary"].as_str().unwrap_or(""), 1200);
    // what can't be undone is confirmed on the computer (agent.rs, `Tool::confirm`)
    if req.get("confirm").is_some_and(|c| !c.is_null()) {
        let msg = format!("🛑 {name}: the agent wants to {tool}\n{summary}\n\nThis can't be undone, so it's confirmed in DevCat on the computer (typing the name).");
        let _ = t.send(&l.chat_id, &Outgoing::text(msg)).await;
        return;
    }
    let tool_use_id = req["toolUseId"].as_str().unwrap_or("").to_string();
    let mut buttons = vec![vec![
        button(&l.id, "Allow", Action::Approve { session: session.into(), tool_use_id: tool_use_id.clone(), allow: true }),
        button(&l.id, "Deny", Action::Approve { session: session.into(), tool_use_id, allow: false }),
    ]];
    if let Some(u) = url {
        buttons.push(vec![Button::Url { label: "Open in DevCat".into(), url: u.into() }]);
    }
    let _ = t.send(&l.chat_id, &Outgoing { text: format!("🔐 {name}: allow {tool}?\n{summary}"), buttons }).await;
}

async fn ask_question(l: &Link, session: &str, name: &str, q: &Value) {
    let Some(t) = transport(&l.bot_id) else { return };
    let tool_use_id = q["toolUseId"].as_str().unwrap_or("").to_string();
    QUESTIONS.lock().insert(l.id.clone(), (session.to_string(), tool_use_id.clone()));
    let buttons: Vec<Vec<Button>> = q["options"]
        .as_array()
        .map(|o| o.iter().filter_map(Value::as_str).take(6).map(|opt| vec![button(&l.id, &text::clip(opt, 60), Action::Answer { session: session.into(), tool_use_id: tool_use_id.clone(), answer: opt.into() })]).collect())
        .unwrap_or_default();
    let msg = format!("❓ {name} asks:\n{}\n\n{}", q["question"].as_str().unwrap_or(""), if buttons.is_empty() { "Reply with your answer." } else { "Tap an answer, or reply in your own words." });
    let _ = t.send(&l.chat_id, &Outgoing { text: msg, buttons }).await;
}

/// The end of a request from this chat: how it went, the answer, what changed.
async fn result(l: &Link, session: &str, event: &Value, url: Option<String>) {
    let Some(t) = transport(&l.bot_id) else { return };
    let took = crate::notify::fmt_duration(event["elapsedMs"].as_i64().unwrap_or(0));
    let head = match event["end"].as_str() {
        Some("error") => format!("⚠️ Stopped with an error after {took}: {}", event["error"].as_str().unwrap_or("")),
        Some("stopped") => format!("⏹ Stopped after {took}."),
        Some("stepLimit") => format!("⏸ Paused after {took}: it hit the step limit. Say \"go on\" to continue."),
        _ => format!("✅ Done in {took}."),
    };
    let answer = agent::last_answer(session).unwrap_or_default();
    let changed = changed_in(l, session);
    let mut body = format!("{head}\n\n{answer}");
    if !changed.is_empty() {
        let names: Vec<String> = changed.iter().take(8).map(|p| rel(l, p)).collect();
        body.push_str(&format!("\n\n📝 Changed {} file{}: {}{}", changed.len(), if changed.len() == 1 { "" } else { "s" }, names.join(", "), if changed.len() > 8 { ", …" } else { "" }));
    }
    let mut parts = text::split(body.trim(), t.max_text());
    let last = parts.pop().unwrap_or_default();
    for p in parts {
        let _ = t.send(&l.chat_id, &Outgoing::text(p)).await;
    }
    let mut row = Vec::new();
    if l.perms.files && !changed.is_empty() {
        row.push(button(&l.id, "📁 Files", Action::Files));
    }
    row.push(button(&l.id, "📝 Summary", Action::Summary));
    if let Some(u) = url {
        row.push(Button::Url { label: "Open in DevCat".into(), url: u });
    }
    let _ = t.send(&l.chat_id, &Outgoing { text: last, buttons: vec![row] }).await;
}
