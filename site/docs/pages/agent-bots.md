# Agent bots

Work with your agents from Telegram or Discord: approve what they ask, send new requests and get the results back.

An agent bot is a Telegram or Discord bot that runs from DevCat on your computer. There's no server in between, and
the bot's token stays in your system keychain. You link a chat to a project folder, and that chat gets the folder's
agents: their "allow this?" with buttons to tap, their questions, and their answers when they finish.

![Approving an agent's command from a chat app](../img/chat-bot.png)

## Add a bot

Open ⚙️ **Settings → Agent bots → Add a Telegram or Discord bot**.

**Telegram**

1. In Telegram open [@BotFather](https://t.me/BotFather) and send `/newbot`; pick a name.
2. Copy the token it gives you (like `123456:ABC…`) and paste it into DevCat.
3. Use the bot for this DevCat only: another program reading its messages stops it here.

**Discord**

1. In the [Discord Developer Portal](https://discord.com/developers/applications) create an application, open
   **Bot** and **Reset Token**; paste the token into DevCat.
2. To use it in a server, turn on **Message Content Intent** on the same page, then invite it (OAuth2 → URL
   Generator: scope **bot**, permissions **Send Messages**, **Attach Files**, **Read Message History**). Direct
   messages need neither.

DevCat checks the token with Telegram or Discord before it saves it.

## Link a chat

1. Under **Link a chat**, pick the **Bot**, the **DevCat Agent connection** that runs the chat's requests, and the
   **Folder** the agent works in (its project, rules and memory).
2. Choose what the chat may do (see below) and click **Get a link code**.
3. Send `/link CODE` to the bot: in a direct message, or in the group or channel you want to link.
4. DevCat asks you to allow that chat. Click **Allow** only if you just sent the code.

Only the person who sent the code is listened to in that chat. Anyone else gets no answer.

## What a chat may do

Each link has its own settings, and DevCat checks them again for every message and every button press.

| Setting | Default |
| --- | --- |
| Notify: finished / needs you | on |
| Allow / Deny, and answer questions | on |
| Send new requests to the agent | on |
| Edit files without asking (commands still ask) | off |
| Get files from the folder (`/files`, `/file`) | off |
| Use DevCat connections (their data goes to the chat app) | off |

## In the chat

Write a message and the agent works on it in the linked folder, as a DevCat Agent chat with the project's rules and
memory. `/help` lists the commands the link allows:

| Command | What it does |
| --- | --- |
| `/new [request]` | Start a fresh agent chat |
| `/stop` | Stop what the agent is doing |
| `/status` | What the agent is doing |
| `/summary` | The agent's last answer and the files it changed |
| `/files` | Files the agent changed (tap one to get it) |
| `/file <path>` | Get a file from the folder |
| `/open` | Open this chat in [DevCat Mobile](mobile.md), on a phone already paired with the computer |
| `/unlink` | Unlink this chat from DevCat |

## What stays on the computer

- Files are sent only from the linked folder, and never key or password files.
- Dropping a database, and anything else that can't be undone, is confirmed in DevCat on the computer, never from
  the chat.
- [Ghost chats](ghost-chats.md) are never sent to a chat app.
- The `/open` link only opens the chat on a phone or browser that's already paired. DevCat never sends a pairing
  link or code to a chat app.

To stop a chat, click **Unlink** next to it in Settings → Agent bots, or send `/unlink`. Removing a bot unlinks all
its chats.

## Notifications without a bot

If you only want "finished" and "needs you", you don't need a bot: [DevCat Mobile](mobile.md#notifications) sends
them to ntfy, Discord, Slack or any webhook.
