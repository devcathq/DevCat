//! Chat apps DevCat talks to through a bot: Telegram (Bot API, long polling) and Discord (gateway
//! websocket for what comes in, REST for what goes out). Both run from this computer: nothing to
//! host, no public address. What comes in goes to `bridge::handle`; nothing here decides who may
//! do what.
use super::BotKind;
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

/// A button under a message: an action (its data comes back when pressed) or a link.
#[derive(Debug, Clone)]
pub enum Button {
    Action { label: String, data: String },
    Url { label: String, url: String },
}

impl Button {
    pub fn action(label: &str, data: &str) -> Button {
        Button::Action { label: label.into(), data: data.into() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub text: String,
    /// Rows of buttons.
    pub buttons: Vec<Vec<Button>>,
}

impl Outgoing {
    pub fn text(t: impl Into<String>) -> Outgoing {
        Outgoing { text: t.into(), buttons: vec![] }
    }
}

/// Someone in a chat.
#[derive(Debug, Clone)]
pub struct Sender {
    pub id: String,
    pub name: String,
}

/// A button press to answer (the app shows a spinner until then).
#[derive(Debug, Clone)]
pub struct Ack {
    pub id: String,
    /// Discord: the interaction token.
    pub token: String,
    pub chat: String,
    pub message_id: String,
    /// The message the button was under.
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum Inbound {
    Message { chat: String, chat_name: String, private: bool, from: Sender, text: String },
    Press { chat: String, from: Sender, data: String, ack: Ack },
}

#[async_trait]
pub trait Transport: Send + Sync {
    fn kind(&self) -> BotKind;
    /// Longest message, and largest file, the app takes.
    fn max_text(&self) -> usize;
    fn max_file(&self) -> u64;
    async fn send(&self, chat: &str, msg: &Outgoing) -> AppResult<()>;
    async fn send_file(&self, chat: &str, name: &str, bytes: Vec<u8>, caption: &str) -> AppResult<()>;
    /// Answer a press: the message under it gets `done` added and loses its buttons.
    async fn ack(&self, ack: &Ack, done: &str) -> AppResult<()>;
    /// Receive until the channel closes or the bot fails for good (the error then).
    async fn run(&self, tx: mpsc::Sender<Inbound>) -> AppResult<()>;
}

fn http(timeout: Duration) -> AppResult<reqwest::Client> {
    Ok(reqwest::Client::builder().timeout(timeout).build()?)
}

/// What the app said when it refused, in a sentence.
async fn refused(what: &str, resp: reqwest::Response) -> AppError {
    let code = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let why = body["description"].as_str().or(body["message"].as_str()).unwrap_or("").to_string();
    AppError::msg(format!("{what}: {code} {why}").trim().to_string())
}

// ---------------------------------------------------------------- Telegram

pub struct Telegram {
    token: String,
}

impl Telegram {
    pub fn new(token: &str) -> Telegram {
        Telegram { token: token.trim().to_string() }
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    async fn call(&self, method: &str, body: Value) -> AppResult<Value> {
        let resp = http(Duration::from_secs(20))?.post(self.url(method)).json(&body).send().await.map_err(|e| AppError::msg(scrub(&e.to_string(), &self.token)))?;
        if !resp.status().is_success() {
            return Err(refused(&format!("Telegram {method}"), resp).await);
        }
        let v: Value = resp.json().await?;
        Ok(v["result"].clone())
    }

    /// The bot's @name, if the token works.
    pub async fn check(&self) -> AppResult<String> {
        let me = self.call("getMe", json!({})).await.map_err(|e| AppError::msg(format!("Telegram didn't accept that token ({e})")))?;
        Ok(me["username"].as_str().unwrap_or("bot").to_string())
    }

    fn keyboard(buttons: &[Vec<Button>]) -> Value {
        json!({ "inline_keyboard": buttons.iter().map(|row| row.iter().map(|b| match b {
            Button::Action { label, data } => json!({ "text": label, "callback_data": data }),
            Button::Url { label, url } => json!({ "text": label, "url": url }),
        }).collect::<Vec<_>>()).collect::<Vec<_>>() })
    }
}

/// The token never shows up in messages (reqwest puts the URL in its errors).
fn scrub(s: &str, token: &str) -> String {
    if token.is_empty() { s.to_string() } else { s.replace(token, "<token>") }
}

fn tg_name(u: &Value) -> String {
    u["username"].as_str().map(|n| format!("@{n}")).or_else(|| u["first_name"].as_str().map(String::from)).unwrap_or_else(|| "someone".into())
}

#[async_trait]
impl Transport for Telegram {
    fn kind(&self) -> BotKind {
        BotKind::Telegram
    }
    fn max_text(&self) -> usize {
        4000
    }
    fn max_file(&self) -> u64 {
        45_000_000
    }
    async fn send(&self, chat: &str, msg: &Outgoing) -> AppResult<()> {
        let mut body = json!({ "chat_id": chat, "text": msg.text, "link_preview_options": { "is_disabled": true } });
        if !msg.buttons.is_empty() {
            body["reply_markup"] = Telegram::keyboard(&msg.buttons);
        }
        self.call("sendMessage", body).await.map(|_| ())
    }
    async fn send_file(&self, chat: &str, name: &str, bytes: Vec<u8>, caption: &str) -> AppResult<()> {
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat.to_string())
            .text("caption", caption.to_string())
            .part("document", reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()));
        let resp = http(Duration::from_secs(120))?.post(self.url("sendDocument")).multipart(form).send().await.map_err(|e| AppError::msg(scrub(&e.to_string(), &self.token)))?;
        if !resp.status().is_success() {
            return Err(refused("Telegram sendDocument", resp).await);
        }
        Ok(())
    }
    async fn ack(&self, ack: &Ack, done: &str) -> AppResult<()> {
        let _ = self.call("answerCallbackQuery", json!({ "callback_query_id": ack.id })).await;
        let text = super::text::clip(&format!("{}\n\n{done}", ack.text), self.max_text());
        self.call("editMessageText", json!({ "chat_id": ack.chat, "message_id": ack.message_id.parse::<i64>().unwrap_or(0), "text": text })).await.map(|_| ())
    }
    async fn run(&self, tx: mpsc::Sender<Inbound>) -> AppResult<()> {
        let client = http(Duration::from_secs(45))?;
        let mut offset: i64 = 0;
        let mut failures = 0u32;
        loop {
            let resp = client
                .post(self.url("getUpdates"))
                .json(&json!({ "offset": offset, "timeout": 30, "allowed_updates": ["message", "callback_query"] }))
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(_) => {
                    // offline (asleep, no network): try again a bit later
                    failures += 1;
                    tokio::time::sleep(Duration::from_secs(u64::from(failures.min(6)) * 5)).await;
                    continue;
                }
            };
            match resp.status().as_u16() {
                200 => failures = 0,
                401 | 404 => return Err(AppError::msg("Telegram doesn't accept this bot's token anymore: add the bot again with a new token.")),
                409 => return Err(AppError::msg("Another program is reading this bot's messages (a webhook, or DevCat on another computer). Use a bot only for this DevCat.")),
                _ => {
                    failures += 1;
                    tokio::time::sleep(Duration::from_secs(u64::from(failures.min(6)) * 5)).await;
                    continue;
                }
            }
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            for u in v["result"].as_array().cloned().unwrap_or_default() {
                offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                let inbound = if let Some(m) = u.get("message") {
                    let (Some(text), Some(from)) = (m["text"].as_str(), m.get("from")) else { continue };
                    if from["is_bot"].as_bool() == Some(true) {
                        continue;
                    }
                    let chat = &m["chat"];
                    let private = chat["type"] == "private";
                    let chat_name = if private { format!("Chat with {}", tg_name(from)) } else { chat["title"].as_str().unwrap_or("group").to_string() };
                    Inbound::Message {
                        chat: chat["id"].to_string(),
                        chat_name,
                        private,
                        from: Sender { id: from["id"].to_string(), name: tg_name(from) },
                        text: text.to_string(),
                    }
                } else if let Some(q) = u.get("callback_query") {
                    let m = &q["message"];
                    Inbound::Press {
                        chat: m["chat"]["id"].to_string(),
                        from: Sender { id: q["from"]["id"].to_string(), name: tg_name(&q["from"]) },
                        data: q["data"].as_str().unwrap_or("").to_string(),
                        ack: Ack {
                            id: q["id"].as_str().unwrap_or("").to_string(),
                            token: String::new(),
                            chat: m["chat"]["id"].to_string(),
                            message_id: m["message_id"].to_string(),
                            text: m["text"].as_str().unwrap_or("").to_string(),
                        },
                    }
                } else {
                    continue;
                };
                if tx.send(inbound).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

// ---------------------------------------------------------------- Discord

const DISCORD_API: &str = "https://discord.com/api/v10";
/// GUILD_MESSAGES, DIRECT_MESSAGES; MESSAGE_CONTENT (privileged: the bot's owner turns it on in
/// the developer portal) to read messages in servers. DMs work without it.
const INTENTS: u64 = (1 << 9) | (1 << 12);
const MESSAGE_CONTENT: u64 = 1 << 15;

pub struct Discord {
    token: String,
    /// Set when the gateway refused MESSAGE_CONTENT (only DMs are read then).
    pub warning: parking_lot::Mutex<Option<String>>,
}

impl Discord {
    pub fn new(token: &str) -> Discord {
        Discord { token: token.trim().to_string(), warning: parking_lot::Mutex::new(None) }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> AppResult<reqwest::RequestBuilder> {
        Ok(http(Duration::from_secs(30))?.request(method, format!("{DISCORD_API}{path}")).header("Authorization", format!("Bot {}", self.token)))
    }

    /// The bot's name, if the token works.
    pub async fn check(&self) -> AppResult<String> {
        let resp = self.req(reqwest::Method::GET, "/users/@me")?.send().await?;
        if !resp.status().is_success() {
            return Err(AppError::msg(format!("Discord didn't accept that token ({})", resp.status())));
        }
        let me: Value = resp.json().await?;
        Ok(me["username"].as_str().unwrap_or("bot").to_string())
    }

    fn components(buttons: &[Vec<Button>]) -> Value {
        json!(buttons.iter().take(5).map(|row| json!({ "type": 1, "components": row.iter().take(5).map(|b| match b {
            Button::Action { label, data } => json!({ "type": 2, "style": if label.starts_with("Deny") { 4 } else if label.starts_with("Allow") { 3 } else { 2 }, "label": super::text::clip(label, 80), "custom_id": data }),
            Button::Url { label, url } => json!({ "type": 2, "style": 5, "label": super::text::clip(label, 80), "url": url }),
        }).collect::<Vec<_>>() })).collect::<Vec<_>>())
    }

    /// The name of a channel (`#general`), for the link prompt.
    pub async fn channel_name(&self, id: &str) -> Option<String> {
        let v: Value = self.req(reqwest::Method::GET, &format!("/channels/{id}")).ok()?.send().await.ok()?.json().await.ok()?;
        v["name"].as_str().map(|n| format!("#{n}"))
    }

    /// One gateway session: until it closes (Ok: connect again) or fails for good (Err).
    async fn session(&self, tx: &mpsc::Sender<Inbound>, intents: u64) -> Result<(), (bool, String)> {
        use tokio_tungstenite::tungstenite::Message;
        let gw: Value = match self.req(reqwest::Method::GET, "/gateway/bot").map_err(|e| (false, e.to_string()))?.send().await {
            Ok(r) if r.status().as_u16() == 401 => return Err((true, "Discord doesn't accept this bot's token anymore: add the bot again with a new token.".into())),
            Ok(r) => r.json().await.map_err(|e| (false, e.to_string()))?,
            Err(e) => return Err((false, e.to_string())),
        };
        let url = format!("{}/?v=10&encoding=json", gw["url"].as_str().unwrap_or("wss://gateway.discord.gg"));
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(|e| (false, e.to_string()))?;
        let (mut write, mut read) = ws.split();
        let mut seq: Option<u64> = None;
        let mut beat = tokio::time::interval(Duration::from_secs(40));
        let mut identified = false;
        loop {
            tokio::select! {
                _ = beat.tick(), if identified => {
                    if write.send(Message::Text(json!({ "op": 1, "d": seq }).to_string().into())).await.is_err() {
                        return Ok(());
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        _ => return Ok(()),
                    };
                    if let Message::Close(frame) = &msg {
                        let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(0);
                        return match code {
                            4004 => Err((true, "Discord doesn't accept this bot's token anymore: add the bot again with a new token.".into())),
                            4014 => Err((false, "intents".into())),
                            _ => Ok(()),
                        };
                    }
                    let Message::Text(t) = msg else { continue };
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if let Some(s) = v["s"].as_u64() {
                        seq = Some(s);
                    }
                    match v["op"].as_u64() {
                        Some(10) => {
                            let ms = v["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250);
                            beat = tokio::time::interval(Duration::from_millis(ms));
                            beat.tick().await;
                            let identify = json!({ "op": 2, "d": { "token": self.token, "intents": intents, "properties": { "os": std::env::consts::OS, "browser": "DevCat", "device": "DevCat" } } });
                            if write.send(Message::Text(identify.to_string().into())).await.is_err() {
                                return Ok(());
                            }
                            identified = true;
                        }
                        Some(1) => {
                            let _ = write.send(Message::Text(json!({ "op": 1, "d": seq }).to_string().into())).await;
                        }
                        Some(7) | Some(9) => return Ok(()), // reconnect / invalid session: start over
                        Some(0) => {
                            if let Some(inbound) = discord_inbound(&v)
                                && tx.send(inbound).await.is_err()
                            {
                                return Err((true, String::new()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn discord_name(u: &Value) -> String {
    u["global_name"].as_str().or(u["username"].as_str()).unwrap_or("someone").to_string()
}

/// A gateway event as something that came in: a message (not from a bot), or a button press.
fn discord_inbound(v: &Value) -> Option<Inbound> {
    let d = &v["d"];
    match v["t"].as_str()? {
        "MESSAGE_CREATE" => {
            let author = &d["author"];
            if author["bot"].as_bool() == Some(true) {
                return None;
            }
            let text = d["content"].as_str().filter(|t| !t.trim().is_empty())?;
            let private = d.get("guild_id").is_none_or(Value::is_null);
            let chat_name = if private { format!("DM with {}", discord_name(author)) } else { "a server channel".into() };
            Some(Inbound::Message {
                chat: d["channel_id"].as_str()?.to_string(),
                chat_name,
                private,
                from: Sender { id: author["id"].as_str()?.to_string(), name: discord_name(author) },
                text: text.to_string(),
            })
        }
        "INTERACTION_CREATE" if d["type"].as_u64() == Some(3) => {
            let user = if d["member"]["user"].is_object() { &d["member"]["user"] } else { &d["user"] };
            Some(Inbound::Press {
                chat: d["channel_id"].as_str()?.to_string(),
                from: Sender { id: user["id"].as_str()?.to_string(), name: discord_name(user) },
                data: d["data"]["custom_id"].as_str()?.to_string(),
                ack: Ack {
                    id: d["id"].as_str()?.to_string(),
                    token: d["token"].as_str()?.to_string(),
                    chat: d["channel_id"].as_str()?.to_string(),
                    message_id: d["message"]["id"].as_str().unwrap_or("").to_string(),
                    text: d["message"]["content"].as_str().unwrap_or("").to_string(),
                },
            })
        }
        _ => None,
    }
}

#[async_trait]
impl Transport for Discord {
    fn kind(&self) -> BotKind {
        BotKind::Discord
    }
    fn max_text(&self) -> usize {
        1900
    }
    fn max_file(&self) -> u64 {
        9_000_000
    }
    async fn send(&self, chat: &str, msg: &Outgoing) -> AppResult<()> {
        let mut body = json!({ "content": msg.text, "allowed_mentions": { "parse": [] } });
        if !msg.buttons.is_empty() {
            body["components"] = Discord::components(&msg.buttons);
        }
        let resp = self.req(reqwest::Method::POST, &format!("/channels/{chat}/messages"))?.json(&body).send().await?;
        if !resp.status().is_success() {
            return Err(refused("Discord", resp).await);
        }
        Ok(())
    }
    async fn send_file(&self, chat: &str, name: &str, bytes: Vec<u8>, caption: &str) -> AppResult<()> {
        let payload = json!({ "content": caption, "allowed_mentions": { "parse": [] } });
        let form = reqwest::multipart::Form::new()
            .text("payload_json", payload.to_string())
            .part("files[0]", reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()));
        let resp = http(Duration::from_secs(120))?
            .post(format!("{DISCORD_API}/channels/{chat}/messages"))
            .header("Authorization", format!("Bot {}", self.token))
            .multipart(form)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(refused("Discord", resp).await);
        }
        Ok(())
    }
    async fn ack(&self, ack: &Ack, done: &str) -> AppResult<()> {
        // UPDATE_MESSAGE: the message gets `done`, without its buttons
        let content = super::text::clip(&format!("{}\n\n{done}", ack.text), self.max_text());
        let resp = http(Duration::from_secs(15))?
            .post(format!("{DISCORD_API}/interactions/{}/{}/callback", ack.id, ack.token))
            .json(&json!({ "type": 7, "data": { "content": content, "components": [] } }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(refused("Discord", resp).await);
        }
        Ok(())
    }
    async fn run(&self, tx: mpsc::Sender<Inbound>) -> AppResult<()> {
        let mut intents = INTENTS | MESSAGE_CONTENT;
        let mut failures = 0u32;
        loop {
            match self.session(&tx, intents).await {
                Ok(()) => failures = 0,
                Err((true, why)) if why.is_empty() => return Ok(()),
                Err((true, why)) => return Err(AppError::msg(why)),
                Err((false, why)) if why == "intents" && intents & MESSAGE_CONTENT != 0 => {
                    intents = INTENTS;
                    *self.warning.lock() = Some("Discord didn't allow reading messages in servers: turn on Message Content Intent for the bot (Developer Portal → Bot), then turn the bot off and on here. Direct messages work meanwhile.".into());
                    continue;
                }
                Err((false, _)) => failures += 1,
            }
            if tx.is_closed() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(u64::from(failures.min(6)) * 5 + 1)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_events_become_messages_and_presses() {
        let dm = json!({ "t": "MESSAGE_CREATE", "d": { "channel_id": "c1", "content": "/status", "author": { "id": "u1", "username": "ana" } } });
        match discord_inbound(&dm) {
            Some(Inbound::Message { chat, private, from, text, .. }) => assert_eq!((chat.as_str(), private, from.id.as_str(), text.as_str()), ("c1", true, "u1", "/status")),
            other => panic!("{other:?}"),
        }
        let from_bot = json!({ "t": "MESSAGE_CREATE", "d": { "channel_id": "c1", "content": "hi", "author": { "id": "b", "bot": true } } });
        assert!(discord_inbound(&from_bot).is_none(), "bots (DevCat itself) are ignored");
        let press = json!({ "t": "INTERACTION_CREATE", "d": { "type": 3, "id": "i1", "token": "t1", "channel_id": "c2", "guild_id": "g",
            "member": { "user": { "id": "u2", "username": "bo" } }, "data": { "custom_id": "x:abc" }, "message": { "id": "m1", "content": "Allow?" } } });
        match discord_inbound(&press) {
            Some(Inbound::Press { chat, from, data, ack }) => assert_eq!((chat.as_str(), from.id.as_str(), data.as_str(), ack.text.as_str()), ("c2", "u2", "x:abc", "Allow?")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_token_never_shows_in_errors() {
        assert_eq!(scrub("error sending request for url (https://api.telegram.org/bot123:ABC/getMe)", "123:ABC"), "error sending request for url (https://api.telegram.org/bot<token>/getMe)");
    }
}
