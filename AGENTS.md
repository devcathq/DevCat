# DevCat: notes for developers and coding agents

## Rule: the SSH workbench is desktop-only

The Xshell-style features (SSH sessions in the sidebar, SFTP tab, quick-command bar, session port
forwards, and pasting or dropping images into a terminal on an SSH host) live in the desktop app
only. **Don't add them to DevCat Mobile** (`mobile/`), the phone dispatcher
(`src-tauri/src/remote/dispatch.rs`) or the browser version, unless you've read the limitations
below and the change handles each one explicitly.

Why the phone can't simply reuse the desktop flow:

- **Files land on the wrong machine.** The phone talks to DevCat on the computer, and the computer
  talks to the SSH host. A file the phone sends ends up on the computer (`paste_dir`, agent
  attachments); an agent running on the SSH host can't read that path. Anything meant for the host
  must be copied there (see `remote::stage_bytes` / `sftp::remote_stage_files`) and the *host's*
  path typed into the terminal.
- **The clipboard is the computer's, not the phone's.** `remote_paste_image` reads the desktop OS
  clipboard. From a phone it would paste whatever happens to be on the computer.
- **Saved passwords.** SSH sessions keep their password in the computer's keychain and DevCat types
  it into the login prompt (`pty.rs`, `AutoPass`). Letting a paired phone open a session would let it
  log in to the server with that password; that needs its own consent and audit design first.
- **Port forwards listen on the computer.** A session's forwards (`claude/ssh_forward.rs`) bind
  127.0.0.1 on the computer, so a phone starting one would open a port it can't reach, and log in
  with the saved password (see above).
- **Transfers are long-running.** SFTP jobs stream over the computer's `ssh` and report progress as
  desktop window events (`sftp-progress`), which the phone protocol doesn't carry.

What the phone side does today: `list_connections` leaves SSH sessions out, and `pty_open` refuses
an SSH-session folder (`ssh://devcat-…`). Keep it that way unless the work above is done.

## Connections over MCP

Agents get the user's connections through the tools in `src-tauri/src/agent_tools.rs`: built into the DevCat
Agent, and served to other agents by `devcat mcp` (`mcp_server.rs`), which Sync MCP (`mcp_sync.rs`) writes into
Claude Code / Codex / Cursor / Gemini configs. Keep one implementation: a new connection tool goes in
`agent_tools.rs` and both get it. Which connections an agent sees follows its folder (`agent_tools::Scope`, a
project's `folders`). Each connection's AI access (`McpAccess`: off / read / full, `ConnectionConfig::agent_access`)
decides the rest: off hides it from every agent, and over MCP data changes need full (connections without the
setting follow the project's `mcp_writes`; a read-only connection is never full). `devcat mcp` logs every call in
`mcp_audit.rs` (the UI's MCP activity). One exception to "both get it": `db_drop_database` is for the DevCat Agent
only, because dropping is confirmed by the user typing the database's name (`Tool::confirm`, which the harness asks
for whatever the policy, hooks or "always allow"), and over MCP DevCat can't ask; the phone dispatcher refuses that
confirmation too. Keep any new tool that can't be undone the same way. A chat opened from a connection
(`agent_open` with `connection_id`, "Ask DevCat Agent") starts with `agent_tools::brief`: that connection's
structure, never its rows. Sync MCP edits files on the
computer, so it stays out of the phone dispatcher, like the SSH workbench above.

## Ghost chats

A ghost chat (`agent_open` with `ghost_window`, `features/agent/ghost.ts`, a window with only the chat:
`GhostWindow.tsx`) starts from nothing: it ignores the folder it's given and works in an empty scratch folder of its
own, without rule files or skills (`system_prompt_with_home(…, None)`), MCP servers, hooks or DevCat connections. It
keeps nothing once it closes: its session and scratch folder live in a temporary folder (`agent.rs` `ghost_dir`) that's deleted when its tab or window closes
(`windows::on_destroyed` → `agent::close_ghosts_of`), when the app quits, and at the next start after a crash. It has
no memory, notes or rooms tools and no memory brief, isn't in `agent::list` or the Sessions list, saves no sandbox
choice and no ↑ history, and the phone dispatcher refuses every `agent_*` call on it. Anything new that records a
chat (titles, logs, notifications with its text) must skip ghost chats too.

## Agent bots and notification channels (`integrations/`)

`src-tauri/src/integrations/` puts DevCat in the user's chat apps: notification webhooks (Discord, Slack, plain) next
to ntfy, and bots (Telegram long polling, Discord gateway) that run from this computer. A chat is *linked* to a
folder (its project and memory), a DevCat Agent connection and `Perms` (notify, approve, requests, files,
connections, auto-edit), with a one-time code from DevCat (`/link CODE`) **and** the user's OK in DevCat
(`LinkPrompt`). What comes from a chat app is someone on another device, so keep these rules:

- **Only a linked chat, and in it only the people its link lists** (`bridge::linked`, and the same check on every
  button press). Strangers get no answer beyond the `/start` hint in a private chat.
- **Decided per message, from the link as it is now** (`Perms`): a button made earlier re-checks the link and, for
  files, the folder, size and secrets again (`text::sendable`). Files leave only from the link's folder, never key
  / password files (`text::secret_like`).
- **What can't be undone is confirmed on the computer**: a call with `Tool::confirm` (dropping a database) gets a
  message, never buttons; the harness refuses a plain allow for it anyway.
- **Ghost chats are never sent** (`bridge::on_agent`), and **DevCat connections are off for a link** unless the user
  turns them on (their rows would go to the chat app): `agent::open(…, connections)`.
- **The "Open in DevCat" link never pairs anything** (`remote::chat_link`): it opens a chat only on a phone or browser
  already paired with this computer. Never send a pairing link or code to a chat app.
- Tokens and webhook URLs live in the keychain (`secrets`), not in `integrations.json`. Integration commands are
  desktop-only: keep them out of the phone dispatcher. A link's folder is on this computer (no `ssh://`), like the
  phone: no SSH workbench, rooms or memory sync through a chat app.

## Shared memory

Every coding agent shares one memory (`src-tauri/src/memory/`): markdown files in `<data>/memory/` with the
frontmatter Claude Code's memory uses, per place (every project, a DevCat project, or a repository in no project).
The tools (`memory/tools.rs`: `memory_search`, `memory_read`, `memory_save`, `memory_forget`, `handoff_write`,
`handoff_take`) are built into the DevCat Agent and served by `devcat mcp`, and both start sessions with
`memory::brief`. Keep one implementation, as for connections. "Sync Claude memory to all agents"
(`memory/claude.rs`) copies `~/.claude/projects/*/memory` in one way only: nothing is written into Claude's
folder. With the user's consent DevCat keeps a marked block in `~/.codex/AGENTS.md` and `~/.gemini/GEMINI.md`
(`memory/rules.rs`) and changes nothing else in those files. Memory reads and writes files on the computer, so it
stays out of the phone dispatcher too, and agents on SSH hosts don't get it (they can't run the local `devcat mcp`).

Memory sync (`memory/sync/`) shares the memory folder between the user's own computers (desktop and `devcat serve`)
over its own iroh endpoint (ALPN `devcat/memory-sync/1`, its own key in `<data>/memory-sync/`), paired with a
one-time code plus approval. Every paired computer reads and writes everything; there are no roles or per-entry
encryption yet, so don't use it to share memory between people. It isn't a phone feature either: a paired phone must
not pair computers or start syncs through the dispatcher. Incoming files go through `plan::clean_incoming` (the same
scrubber and `MAX_BODY` as saving); keep it that way for any new file kind.

## Rooms

Rooms (`crates/rooms`, `src-tauri/src/rooms.rs`, `docs/rooms-architecture.md`) are for sharing with *other people*,
so what comes from a room is untrusted input. Keep these rules:

- **One set of room tools.** The room agent's tools (`rooms_agent.rs`: `spec_of`, `make`, `tools_for`) are also
  what this user's coding agents get (`rooms_tools.rs`: the same tools with a `room` input, for DevCat Agent chats
  and `devcat mcp`). A new room tool goes in `rooms_agent.rs` and both get it.
- **Coding agents only where the user turned it on**, per room (`Prefs::coding_agents`, local, never shared), with
  the room agent's limits (editors' tools for editors, `room_edit` only where the owner allows agent edits). Their
  writes are `Kind::Fetch` in a chat, so they ask: the text goes to other people.
- **Nothing copies this computer's files into a room from an agent.** Sharing files and folders is a person's act
  in the window (`room_paths_add`, `room_file_add_bytes`); don't add a tool for it. Likewise nothing from a room is
  written on the computer (skills, memory, notes, files) without a person looking at it and confirming.
- **The bridge for `devcat mcp`** is the app listening on 127.0.0.1 with a fresh token in `<data>/rooms/agents.json`
  (`rooms_tools::start_bridge`); `devcat mcp` can't open the rooms itself (one iroh key, one writer of the
  document). Keep requests token-checked and loopback-only.
- **Root container names never contain `/`**: Loro panics when it imports a change to such a root (in the window and
  in Rust). Files in folders use `doc.rs` `root_part` (`/` → `%2F`, `%` → `%25`), and the window the same
  (`replica.ts` `rootPart`). Any new per-file container must go through it.
- **A room is deleted from a DevCat only on proof**: a member list the owner signed without that DevCat
  (`Room::wipe`, from `adopt`). Never on a peer's word: a `Denied { removed }` without a signed list only marks it
  read-only. And never send document changes to a peer that isn't a reader in the newest list.
- **Who shared, moved and took out what** goes in the room's `activity` (`doc.log`, or `rename` for moves); a new
  way into or out of a room records itself there. Who *edited* comes from the document's history (`doc.edits`):
  an agent's changes must be committed with `commit_by` so they show as the agent.
- Rooms are desktop-only, like the SSH workbench: keep them out of the phone dispatcher.
