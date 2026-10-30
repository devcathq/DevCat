//! Integrations: DevCat in the user's chat apps.
//!
//! - **Notification channels**: Discord / Slack incoming webhooks and plain webhooks get "finished"
//!   and "needs you", next to ntfy (`notify`).
//! - **Bots** (Telegram, Discord) run from this computer (no server, no public address). A chat is
//!   *linked* to a folder (its project, and so its memory), a DevCat Agent connection and what it
//!   may do ([`Perms`]): get notifications, answer approvals and questions, send new requests,
//!   receive files. Linking takes a one-time code shown in DevCat (`/link CODE` in the chat) and
//!   then the user's OK in DevCat, like pairing a phone.
//!
//! What comes from a chat app is someone typing on another device, so: only linked chats, and in
//! them only the people the link lists, are heard; tokens and webhook URLs stay in the keychain;
//! files go out only from the linked folder and never key / password files (`text::sendable`);
//! what can't be undone (dropping a database, `Tool::confirm`) is confirmed on the computer only;
//! ghost chats are never sent; DevCat connections are off for a link unless the user turns them on
//! (their rows would end up in the chat app). Like the phone, a link never gets the SSH workbench,
//! rooms or memory sync, and its folder is on this computer. See AGENTS.md.
pub mod bridge;
pub mod text;
pub mod transport;

use crate::error::{AppError, AppResult};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use transport::{Discord, Telegram, Transport};

pub use bridge::agent_event;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotKind {
    Telegram,
    Discord,
}

impl BotKind {
    pub fn label(self) -> &'static str {
        match self {
            BotKind::Telegram => "Telegram",
            BotKind::Discord => "Discord",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bot {
    pub id: String,
    pub kind: BotKind,
    /// The bot's name in the app (`@devcat_ana_bot`).
    pub name: String,
    pub enabled: bool,
}

/// What a linked chat may do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Perms {
    /// "Finished" / "needs you" for agents working in the folder.
    pub notify: bool,
    /// Allow / Deny the agents' requests, and answer their questions.
    pub approve: bool,
    /// Messages are requests to the agent (a chat of its own, in the folder).
    pub requests: bool,
    /// Send files from the folder (`/files`, `/file`).
    pub files: bool,
    /// The agent gets the DevCat connections (databases, clusters): their data goes to the chat app.
    pub connections: bool,
    /// Requests edit files in the folder without asking (commands still ask).
    pub auto_edit: bool,
}

impl Default for Perms {
    fn default() -> Self {
        Perms { notify: true, approve: true, requests: true, files: false, connections: false, auto_edit: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkUser {
    pub id: String,
    pub name: String,
}

/// A chat linked to a folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub id: String,
    pub bot_id: String,
    pub chat_id: String,
    pub chat_name: String,
    /// Who in the chat DevCat listens to (the one who linked it). Others are ignored.
    pub users: Vec<LinkUser>,
    /// Where the agent works (and so its project and memory). On this computer.
    pub folder: String,
    /// The DevCat Agent connection that runs its requests.
    pub provider_id: String,
    pub perms: Perms,
    /// The chat's current DevCat Agent chat (`/new` starts another).
    #[serde(default)]
    pub session_id: Option<String>,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Discord,
    Slack,
    Webhook,
}

/// A notification channel: a webhook that gets "finished" / "needs you" (its URL is in the keychain).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    pub enabled: bool,
    pub on_done: bool,
    pub on_input: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    bots: Vec<Bot>,
    links: Vec<Link>,
    channels: Vec<Channel>,
}

static DIR: OnceCell<PathBuf> = OnceCell::new();
static SETTINGS: Lazy<Mutex<Option<Settings>>> = Lazy::new(Mutex::default);

fn path() -> AppResult<PathBuf> {
    DIR.get().map(|d| d.join("integrations.json")).ok_or_else(|| AppError::msg("Agent bots aren't initialized"))
}

fn load() -> Settings {
    let mut cache = SETTINGS.lock();
    if let Some(s) = cache.as_ref() {
        return s.clone();
    }
    let s: Settings = path().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *cache = Some(s.clone());
    s
}

/// Change the settings and save them.
fn update<T>(f: impl FnOnce(&mut Settings) -> T) -> AppResult<T> {
    let mut s = load();
    let out = f(&mut s);
    let p = path()?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, p)?;
    *SETTINGS.lock() = Some(s);
    Ok(out)
}

fn bot_secret(id: &str) -> String {
    format!("integrations.bot.{id}")
}

fn channel_secret(id: &str) -> String {
    format!("integrations.channel.{id}")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn links() -> Vec<Link> {
    load().links
}

pub(crate) fn link(id: &str) -> Option<Link> {
    load().links.into_iter().find(|l| l.id == id)
}

/// Remember a link's current agent chat.
pub(crate) fn set_session(link_id: &str, session: Option<String>) {
    let _ = update(|s| {
        if let Some(l) = s.links.iter_mut().find(|l| l.id == link_id) {
            l.session_id = session;
        }
    });
}

/// Whether `path` is `folder` or inside it.
pub(crate) fn inside(path: &str, folder: &str) -> bool {
    let norm = |p: &str| {
        let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
        if cfg!(windows) { p.to_lowercase() } else { p }
    };
    let (p, f) = (norm(path), norm(folder));
    !f.is_empty() && (p == f || p.starts_with(&format!("{f}/")))
}

// ---------------------------------------------------------------- running bots

struct Running {
    transport: Arc<dyn Transport>,
    /// Stops its task (Tauri's runtime in the app, tokio's in `devcat serve`).
    stop: Box<dyn Fn() + Send>,
}

static RUNNING: Lazy<Mutex<HashMap<String, Running>>> = Lazy::new(Mutex::default);
/// Why a bot stopped (shown in DevCat).
static ERRORS: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(Mutex::default);
/// Discord's "turn on Message Content Intent" (shown in DevCat).
static WARNINGS: Lazy<Mutex<HashMap<String, Arc<Discord>>>> = Lazy::new(Mutex::default);

/// A running bot, to send through.
pub(crate) fn transport(bot_id: &str) -> Option<Arc<dyn Transport>> {
    RUNNING.lock().get(bot_id).map(|r| r.transport.clone())
}

fn stop_bot(id: &str) {
    if let Some(r) = RUNNING.lock().remove(id) {
        (r.stop)();
    }
    WARNINGS.lock().remove(id);
}

fn start_bot(bot: &Bot) {
    stop_bot(&bot.id);
    ERRORS.lock().remove(&bot.id);
    if !bot.enabled {
        return;
    }
    let Some(token) = crate::secrets::get(&bot_secret(&bot.id)) else {
        ERRORS.lock().insert(bot.id.clone(), "Its token is missing from the keychain: add the bot again.".into());
        return;
    };
    let transport: Arc<dyn Transport> = match bot.kind {
        BotKind::Telegram => Arc::new(Telegram::new(&token)),
        BotKind::Discord => {
            let d = Arc::new(Discord::new(&token));
            WARNINGS.lock().insert(bot.id.clone(), d.clone());
            d
        }
    };
    let (id, t) = (bot.id.clone(), transport.clone());
    let task = crate::rt::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let (id2, t2) = (id.clone(), t.clone());
        let handler = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                let (id, t) = (id2.clone(), t2.clone());
                // one at a time would let a slow file hold up an approval
                tokio::spawn(async move { bridge::handle(&id, t, inbound).await });
            }
        });
        let res = t.run(tx).await;
        handler.abort();
        if let Err(e) = res {
            ERRORS.lock().insert(id.clone(), e.to_string());
            RUNNING.lock().remove(&id);
            push_status();
        }
    });
    RUNNING.lock().insert(bot.id.clone(), Running { transport, stop: Box::new(move || task.abort()) });
}

pub fn init(data_dir: PathBuf) {
    let _ = DIR.set(data_dir);
    crate::rt::spawn(async {
        for b in load().bots {
            start_bot(&b);
        }
    });
}

// ---------------------------------------------------------------- linking

/// A code shown in DevCat: the chat that sends `/link CODE` asks to be linked with these settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkCode {
    pub code: String,
    pub bot_id: String,
    pub folder: String,
    pub provider_id: String,
    pub perms: Perms,
    pub expires_ms: i64,
}

/// A chat that sent a valid code, waiting for the user's OK in DevCat.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequest {
    pub request_id: String,
    pub bot_id: String,
    pub kind: BotKind,
    pub bot_name: String,
    pub chat_id: String,
    pub chat_name: String,
    pub private: bool,
    pub user: LinkUser,
    pub folder: String,
    pub provider_id: String,
    pub perms: Perms,
    pub expires_ms: i64,
}

static CODE: Lazy<Mutex<Option<LinkCode>>> = Lazy::new(Mutex::default);
static REQUESTS: Lazy<Mutex<Vec<LinkRequest>>> = Lazy::new(Mutex::default);
/// Wrong codes per chat (bot:chat → (count, since)): after a few, `/link` is ignored for a while.
static TRIES: Lazy<Mutex<HashMap<String, (u32, i64)>>> = Lazy::new(Mutex::default);

const CODE_TTL_MS: i64 = 10 * 60_000;
const REQUEST_TTL_MS: i64 = 5 * 60_000;

/// `/link CODE` from a chat that isn't linked (or is, to link it again): a request when the code
/// is right. What to tell the chat.
pub(crate) fn link_asked(bot_id: &str, chat_id: &str, chat_name: &str, private: bool, user: LinkUser, code: &str) -> String {
    let key = format!("{bot_id}:{chat_id}");
    let now = now_ms();
    {
        let mut tries = TRIES.lock();
        let t = tries.entry(key.clone()).or_insert((0, now));
        if now - t.1 > CODE_TTL_MS {
            *t = (0, now);
        }
        if t.0 >= 5 {
            return "Too many wrong codes. Wait a few minutes, then get a new code in DevCat.".into();
        }
    }
    let want = code.trim().to_ascii_uppercase().replace(' ', "");
    let found = {
        let mut c = CODE.lock();
        match c.as_ref() {
            Some(lc) if lc.bot_id == bot_id && lc.expires_ms > now && remote_eq(&lc.code, &want) => c.take(),
            _ => None,
        }
    };
    let Some(lc) = found else {
        TRIES.lock().entry(key).or_insert((0, now)).0 += 1;
        return "That code isn't right (or has expired). In DevCat: Settings → Agent bots → Link a chat, then send /link and the code it shows.".into();
    };
    let bot = load().bots.into_iter().find(|b| b.id == bot_id);
    REQUESTS.lock().push(LinkRequest {
        request_id: text::token(16),
        bot_id: bot_id.into(),
        kind: bot.as_ref().map(|b| b.kind).unwrap_or(BotKind::Telegram),
        bot_name: bot.map(|b| b.name).unwrap_or_default(),
        chat_id: chat_id.into(),
        chat_name: chat_name.into(),
        private,
        user,
        folder: lc.folder,
        provider_id: lc.provider_id,
        perms: lc.perms,
        expires_ms: now + REQUEST_TTL_MS,
    });
    push_status();
    "Almost there: allow this chat in DevCat on the computer.".into()
}

fn remote_eq(a: &str, b: &str) -> bool {
    crate::remote::constant_time_eq(a.as_bytes(), b.as_bytes())
}

// ---------------------------------------------------------------- notifications

/// "Finished" or "needs you", for notification channels and linked chats.
pub struct Note {
    pub title: String,
    pub body: String,
    pub needs_you: bool,
    /// The agent's folder: linked chats of that folder get it.
    pub cwd: Option<String>,
    /// Links that got this already another way (their own chat's result, an approval).
    pub skip_links: Vec<String>,
    /// Opens the chat in DevCat Mobile.
    pub url: Option<String>,
}

/// Send a note to the notification channels and the linked chats that want it.
pub async fn deliver(note: Note) {
    let s = load();
    for c in s.channels.iter().filter(|c| c.enabled && if note.needs_you { c.on_input } else { c.on_done }) {
        if let Some(url) = crate::secrets::get(&channel_secret(&c.id))
            && let Err(e) = post_channel(c.kind, &url, &note).await
        {
            eprintln!("notification to {} failed: {e}", c.name);
        }
    }
    let Some(cwd) = note.cwd.as_deref() else { return };
    for l in s.links.iter().filter(|l| l.perms.notify && !note.skip_links.contains(&l.id) && inside(cwd, &l.folder)) {
        let Some(t) = transport(&l.bot_id) else { continue };
        let mut msg = transport::Outgoing::text(format!("{} {}\n{}", if note.needs_you { "🙋" } else { "✅" }, note.title, text::clip(&note.body, 1500)));
        if let Some(u) = &note.url {
            msg.buttons.push(vec![transport::Button::Url { label: "Open in DevCat".into(), url: u.clone() }]);
        }
        let _ = t.send(&l.chat_id, &msg).await;
    }
}

async fn post_channel(kind: ChannelKind, url: &str, note: &Note) -> AppResult<()> {
    let link = note.url.as_deref().map(|u| format!("\n{u}")).unwrap_or_default();
    let body = match kind {
        ChannelKind::Discord => json!({ "content": text::clip(&format!("**{}**\n{}{link}", note.title, note.body), 1900), "allowed_mentions": { "parse": [] } }),
        ChannelKind::Slack => json!({ "text": text::clip(&format!("*{}*\n{}{link}", note.title, note.body), 3000) }),
        ChannelKind::Webhook => json!({ "title": note.title, "message": note.body, "needsYou": note.needs_you, "url": note.url }),
    };
    let resp = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?.post(url).json(&body).send().await?;
    if !resp.status().is_success() {
        return Err(AppError::msg(format!("the webhook answered {}", resp.status())));
    }
    Ok(())
}

/// Webhook URLs DevCat accepts for a channel kind.
fn check_channel_url(kind: ChannelKind, url: &str) -> AppResult<()> {
    let ok = match kind {
        ChannelKind::Discord => url.starts_with("https://discord.com/api/webhooks/") || url.starts_with("https://discordapp.com/api/webhooks/"),
        ChannelKind::Slack => url.starts_with("https://hooks.slack.com/"),
        ChannelKind::Webhook => url.starts_with("https://") || url.starts_with("http://"),
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::msg(match kind {
            ChannelKind::Discord => "A Discord webhook URL starts with https://discord.com/api/webhooks/ (channel → Edit → Integrations → Webhooks).",
            ChannelKind::Slack => "A Slack webhook URL starts with https://hooks.slack.com/ (an app's Incoming Webhooks).",
            ChannelKind::Webhook => "The webhook URL must start with https://",
        }))
    }
}

// ---------------------------------------------------------------- status

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotStatus {
    #[serde(flatten)]
    pub bot: Bot,
    pub running: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub bots: Vec<BotStatus>,
    pub links: Vec<Link>,
    pub channels: Vec<Channel>,
    pub code: Option<LinkCode>,
    pub requests: Vec<LinkRequest>,
}

fn status_now() -> Status {
    let now = now_ms();
    REQUESTS.lock().retain(|r| r.expires_ms > now);
    {
        let mut c = CODE.lock();
        if c.as_ref().is_some_and(|c| c.expires_ms <= now) {
            *c = None;
        }
    }
    let s = load();
    let running = RUNNING.lock();
    let bots = s
        .bots
        .iter()
        .map(|b| BotStatus {
            bot: b.clone(),
            running: running.contains_key(&b.id),
            error: ERRORS.lock().get(&b.id).cloned(),
            warning: WARNINGS.lock().get(&b.id).and_then(|d| d.warning.lock().clone()),
        })
        .collect();
    Status { bots, links: s.links, channels: s.channels, code: CODE.lock().clone(), requests: REQUESTS.lock().clone() }
}

pub(crate) fn push_status() {
    crate::events::emit("integrations", serde_json::to_value(status_now()).unwrap_or(Value::Null));
}

// ---------------------------------------------------------------- commands (desktop only)

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_status() -> AppResult<Status> {
    Ok(status_now())
}

/// Add a bot: its token is checked with the app, then kept in the keychain.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_bot_add(kind: BotKind, token: String) -> AppResult<Status> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(AppError::msg("Paste the bot's token"));
    }
    let name = match kind {
        BotKind::Telegram => format!("@{}", Telegram::new(&token).check().await?),
        BotKind::Discord => Discord::new(&token).check().await?,
    };
    let bot = Bot { id: text::token(12), kind, name, enabled: true };
    crate::secrets::set(&bot_secret(&bot.id), &token)?;
    update(|s| s.bots.push(bot.clone()))?;
    start_bot(&bot);
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_bot_enable(id: String, enabled: bool) -> AppResult<Status> {
    let bot = update(|s| {
        s.bots.iter_mut().find(|b| b.id == id).map(|b| {
            b.enabled = enabled;
            b.clone()
        })
    })?
    .ok_or_else(|| AppError::msg("No such bot"))?;
    start_bot(&bot);
    push_status();
    Ok(status_now())
}

/// Remove a bot, its token and its linked chats (they're told).
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_bot_remove(id: String) -> AppResult<Status> {
    if let Some(t) = transport(&id) {
        for l in links().into_iter().filter(|l| l.bot_id == id) {
            let _ = t.send(&l.chat_id, &transport::Outgoing::text("This chat was unlinked from DevCat: the bot was removed there.")).await;
        }
    }
    stop_bot(&id);
    crate::secrets::delete(&bot_secret(&id));
    update(|s| {
        s.bots.retain(|b| b.id != id);
        s.links.retain(|l| l.bot_id != id);
    })?;
    ERRORS.lock().remove(&id);
    push_status();
    Ok(status_now())
}

/// A one-time code to link a chat of `bot_id` to `folder`, run by `provider_id`, allowed `perms`.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_code(bot_id: String, folder: String, provider_id: String, perms: Perms) -> AppResult<Status> {
    if !load().bots.iter().any(|b| b.id == bot_id) {
        return Err(AppError::msg("Add the bot first"));
    }
    check_folder(&folder)?;
    let (provider, _) = crate::ai::agent_config(&provider_id).map_err(|_| AppError::msg("Pick a DevCat Agent AI connection to run the requests"))?;
    let _ = provider;
    *CODE.lock() = Some(LinkCode { code: text::link_code(), bot_id, folder: folder.trim().to_string(), provider_id, perms, expires_ms: now_ms() + CODE_TTL_MS });
    push_status();
    Ok(status_now())
}

/// A link's folder: a folder on this computer (not an SSH host: see AGENTS.md).
fn check_folder(folder: &str) -> AppResult<()> {
    let f = folder.trim();
    if crate::claude::remote::Target::parse(f).is_some() {
        return Err(AppError::msg("A chat app can't be linked to a folder on an SSH host: pick a folder on this computer."));
    }
    let p = Path::new(f);
    if !p.is_absolute() || !p.is_dir() {
        return Err(AppError::msg(format!("{f} isn't a folder on this computer")));
    }
    Ok(())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_cancel_code() -> AppResult<Status> {
    *CODE.lock() = None;
    push_status();
    Ok(status_now())
}

/// Allow (or not) a chat that sent the code.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_answer(request_id: String, allow: bool) -> AppResult<Status> {
    let req = {
        let mut r = REQUESTS.lock();
        let i = r.iter().position(|x| x.request_id == request_id).ok_or_else(|| AppError::msg("That request has expired"))?;
        r.remove(i)
    };
    let t = transport(&req.bot_id);
    if !allow {
        if let Some(t) = t {
            let _ = t.send(&req.chat_id, &transport::Outgoing::text("DevCat didn't allow linking this chat.")).await;
        }
        push_status();
        return Ok(status_now());
    }
    let link = Link {
        id: text::token(12),
        bot_id: req.bot_id.clone(),
        chat_id: req.chat_id.clone(),
        chat_name: req.chat_name.clone(),
        users: vec![req.user.clone()],
        folder: req.folder.clone(),
        provider_id: req.provider_id.clone(),
        perms: req.perms.clone(),
        session_id: None,
        created_ms: now_ms(),
    };
    // one link per chat: linking again replaces it
    update(|s| {
        s.links.retain(|l| !(l.bot_id == link.bot_id && l.chat_id == link.chat_id));
        s.links.push(link.clone());
    })?;
    if let Some(t) = t {
        let _ = t.send(&link.chat_id, &transport::Outgoing::text(bridge::welcome(&link))).await;
    }
    push_status();
    Ok(status_now())
}

/// Change what a linked chat may do, or which DevCat Agent connection runs it.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_update(id: String, perms: Perms, provider_id: Option<String>) -> AppResult<Status> {
    if let Some(p) = &provider_id {
        crate::ai::agent_config(p).map_err(|_| AppError::msg("Pick a DevCat Agent AI connection"))?;
    }
    let changed_connections = update(|s| {
        s.links.iter_mut().find(|l| l.id == id).map(|l| {
            let changed = l.perms.connections != perms.connections || provider_id.as_ref().is_some_and(|p| *p != l.provider_id);
            l.perms = perms;
            if let Some(p) = provider_id {
                l.provider_id = p;
            }
            // the agent's tools are set when its chat opens: a new chat gets the new ones
            if changed {
                l.session_id = None;
            }
            changed
        })
    })?
    .ok_or_else(|| AppError::msg("No such linked chat"))?;
    let _ = changed_connections;
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_remove(id: String) -> AppResult<Status> {
    if let Some(l) = link(&id)
        && let Some(t) = transport(&l.bot_id)
    {
        let _ = t.send(&l.chat_id, &transport::Outgoing::text("This chat was unlinked from DevCat.")).await;
    }
    update(|s| s.links.retain(|l| l.id != id))?;
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_link_test(id: String) -> AppResult<()> {
    let l = link(&id).ok_or_else(|| AppError::msg("No such linked chat"))?;
    let t = transport(&l.bot_id).ok_or_else(|| AppError::msg("The bot isn't running"))?;
    t.send(&l.chat_id, &transport::Outgoing::text(format!("👋 Test from DevCat: this chat is linked to {}.", l.folder))).await
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_channel_add(kind: ChannelKind, name: String, url: String) -> AppResult<Status> {
    let url = url.trim().to_string();
    check_channel_url(kind, &url)?;
    let c = Channel {
        id: text::token(12),
        kind,
        name: if name.trim().is_empty() { match kind { ChannelKind::Discord => "Discord", ChannelKind::Slack => "Slack", ChannelKind::Webhook => "Webhook" }.to_string() } else { name.trim().to_string() },
        enabled: true,
        on_done: true,
        on_input: true,
    };
    crate::secrets::set(&channel_secret(&c.id), &url)?;
    update(|s| s.channels.push(c))?;
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_channel_update(id: String, enabled: bool, on_done: bool, on_input: bool) -> AppResult<Status> {
    update(|s| {
        if let Some(c) = s.channels.iter_mut().find(|c| c.id == id) {
            (c.enabled, c.on_done, c.on_input) = (enabled, on_done, on_input);
        }
    })?;
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_channel_remove(id: String) -> AppResult<Status> {
    crate::secrets::delete(&channel_secret(&id));
    update(|s| s.channels.retain(|c| c.id != id))?;
    push_status();
    Ok(status_now())
}

#[cfg_attr(feature = "gui", tauri::command)]
pub async fn integrations_channel_test(id: String) -> AppResult<()> {
    let c = load().channels.into_iter().find(|c| c.id == id).ok_or_else(|| AppError::msg("No such channel"))?;
    let url = crate::secrets::get(&channel_secret(&id)).ok_or_else(|| AppError::msg("Its URL is missing from the keychain: add it again"))?;
    let note = Note { title: "DevCat".into(), body: "Test notification: you'll get these when an agent finishes or needs you.".into(), needs_you: false, cwd: None, skip_links: vec![], url: None };
    post_channel(c.kind, &url, &note).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_contain_their_subfolders_only() {
        assert!(inside("/src/shop", "/src/shop"));
        assert!(inside("/src/shop/api", "/src/shop/"));
        assert!(!inside("/src/shopping", "/src/shop"), "a folder, not a name prefix");
        assert!(!inside("/src", "/src/shop"));
        assert!(!inside("/src/shop", ""));
    }

    #[test]
    fn webhook_urls_are_checked() {
        assert!(check_channel_url(ChannelKind::Discord, "https://discord.com/api/webhooks/1/abc").is_ok());
        assert!(check_channel_url(ChannelKind::Discord, "https://evil.example/api/webhooks/1").is_err());
        assert!(check_channel_url(ChannelKind::Slack, "https://hooks.slack.com/services/T/B/x").is_ok());
        assert!(check_channel_url(ChannelKind::Webhook, "ftp://x").is_err());
    }

    #[test]
    fn new_links_only_notify_ask_and_take_requests() {
        let p = Perms::default();
        assert!(p.notify && p.approve && p.requests);
        assert!(!p.files && !p.connections && !p.auto_edit, "what sends data out, or acts without asking, is off");
        // saved before a setting existed: off
        let old: Perms = serde_json::from_value(json!({ "notify": true })).unwrap();
        assert!(old.notify && old.approve && !old.files);
    }
}
