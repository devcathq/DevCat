# DevCat: notes for developers and coding agents

## Issues and pull requests

Use the templates in `.github/` (GitHub fills them in; with `gh issue create` / `gh pr create --body`,
follow the same sections):

- **Bug** (`bug` label): Problem (steps, example, environment) · Why (from reading the code) · Proposal · Notes.
- **Feature** (`enhancement` label): What · Expected · Notes (code pointers; what it means for SSH folders,
  the phone / browser version, agent tools, security).
- **Pull request**: Summary · `Closes #N` · Test plan (tick what you ran, leave manual checks unticked) · Notes.
- Titles start with the area: `Page review: …`, `DevCat Agent: …`, `Terminal: …`.
- Plain words. Name files with their paths. A big feature gets one issue per part that can ship on its own,
  linked to each other.

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

Agents get what DevCat manages through the tools in `src-tauri/src/agent_tools/` (one file per area: `mod.rs`
connections, schema, query, dump, export, drop, k8s get / logs, `brief`; `db.rs`, `k8s.rs`, `docker.rs`,
`rabbitmq.rs`, `network.rs` VPNs and tunnels, `workspace.rs` adding connections, history, scheduled jobs, tickets):
built into the DevCat Agent, and served to other agents by `devcat mcp` (`mcp_server.rs`, `tools_for`), which Sync
MCP (`mcp_sync.rs`) writes into Claude Code / Codex / Cursor / Gemini configs. Keep one implementation: a new tool
goes in `agent_tools/` and both get it. `for_scope` only offers tools for what the folder has (no Docker tools
without a Docker connection), and `devcat mcp` tells the agent when that changes (`notifications/tools/list_changed`).
Which connections, network profiles, notes and ticket boards an agent sees follows its folder (`agent_tools::Scope`,
`in_reach`, a project's `folders`). Each connection's AI access (`McpAccess`: off / read / full,
`ConnectionConfig::agent_access`) decides the rest: off hides it from every agent, and over MCP changes need full
(`may_change`; connections without the setting follow the project's `mcp_writes`; a read-only connection is never
full). Network profiles have their own (`NetProfile::agent_access`, default read: status, logs and connect; full
also disconnect and edit). Rules that hold whatever the caller: a connection an agent adds gets AI access read;
agents never change a connection's or profile's access; VPN configs an agent writes can't run programs
(`network::agent_config_problem`, the passwordless helper's checks); jobs stay in the agent's folder and never Bypass;
secret-looking values are hidden (`redact_secrets`). A run fed outside text (a ticket's agent: `Scope::unattended`)
gets no tool that reaches boards, jobs, connection setup, profile changes, notes or rooms, and tests only existing
connections; a ticket's Claude Code gets the same through `devcat mcp --unattended` (`mcp_sync::run_config`) and the
bridge (`Call::unattended`: a call can narrow its scope, never widen it). `devcat mcp` logs every call in
`mcp_audit.rs` (the UI's MCP activity). `devcat_open_file` (`workspace.rs`) shows the user a file in the window
through `opened::show`, the same queue as a file opened with DevCat: it returns only where the file opened, never its
content, opens a `.ovpn` as text rather than in the VPN import form (`OpenedFile::agent`), and an unattended run
doesn't get it. `db_open_query` (`workspace.rs`, `opened::show_query`, "opened-queries") opens SQL in a query tab
the same way and may run it there once (`autoRun`): a read by its words runs on a read-only session
(`db_execute_read_only`), anything else only where `may_change` allows (`run_mode`), since the window's own session
would otherwise run what the agent may not.
`agent_delegate` (`workspace.rs`) hands a task to a DevCat Agent connection only if the user turned
on `AgentSettings::delegate` for it (`ai::delegates`; off by default, and no tool at all without one): a chat in the
caller's folder (or below, like jobs), never Bypass, where whatever would ask the user is refused at once
(`agent_jobs::is_delegated`: nobody watches it, and the caller would wait forever), with no DevCat tools (`connections: false`, so it can't
delegate on), a task capped to a share of the model's window (`task_fits`), and only a short answer and the changed
files back. It needs the open DevCat and isn't offered to unattended runs.

Judging a query by its words can be fooled, so a query DevCat lets through only because it looks like a read (an
agent's `Kind::Query`, read access over MCP, `db_export`, a read-only phone) runs on a read-only session
(`St::read_only_driver`), where the driver and, where it can, the server refuse writes. A kubeconfig an agent names
may not log in by running a program (`infra::k8s::agent_kubeconfig_problem`). The ticket worker runs git without
the repository's hooks unless the board allows commands, and no git at all in a repository whose git settings
(config, attributes, hooks) changed while the agent worked (`tickets::worker::git_setup`), nor in a worktree
board's clone whose shared settings changed since its job started (`shared_setup`: another run's agent).

One exception to "both get it": what can't be undone is for the DevCat Agent only (`db_drop_database`,
`db_restore`, `k8s_delete`, `docker_remove`, `rmq_delete`, `net_profile_delete`, `db_import` emptying the table
first), because the user confirms it by typing its name (`Tool::confirm`, which the harness asks for whatever the
policy, hooks or "always allow"), and over MCP DevCat can't ask; the phone dispatcher refuses that confirmation too.
Such a tool isn't offered when `Scope::mcp` and refuses to run there. Keep any new tool that can't be undone the same
way. A chat opened from a connection (`agent_open` with `connection_id`, "Ask DevCat Agent") starts with
`agent_tools::brief`: that connection's structure, never its rows.

VPNs, tunnels, scheduled jobs, ticket runs and rooms live in the open DevCat, not in the `devcat mcp` process each
agent starts. So with DevCat open (app or `devcat serve`), `devcat mcp` hands each call to it over `agent_bridge.rs`
(127.0.0.1, a random port and token in `<data>/agent-bridge.json`, one JSON line each way) and DevCat runs the same
tool with the same scope (`tools_for(…, in_app)`); with DevCat closed it runs the call itself, passive
(`network::set_passive`: it never starts networks), and tools that need DevCat say so. Sync MCP and the bridge are
about this computer, so they stay out of the phone dispatcher, like the SSH workbench above.

## Several Claude Code accounts

A Claude Code AI connection with "its own login" (`AiProvider::own_login`, never the built-in one) runs with
`CLAUDE_CONFIG_DIR=<data>/claude-accounts/<id>` (`ai::launch`, `ai/claude_account.rs`), where Claude Code keeps that
account's login. The folder links back to the main config (`~/.claude`) for `projects`, settings, `CLAUDE.md`,
skills, agents, commands and plugins, so the Sessions list, resume, memory sync and settings stay one; only the login
and `.claude.json` are the account's own, and Sync MCP writes DevCat's server into each account's `.claude.json` too.
Deleting the connection deletes its folder: `claude_account::remove` unlinks the shared entries first and refuses a
shared one that isn't a link, so nothing in `~/.claude` can go with it. Anything new that reads Claude Code's config
must still find the transcripts under the main `projects`. The account's email (`AiProvider::login`) is read only for
the window and the phone (`ai_providers_list`); DevCat's own lookups use `ai::providers()`. On SSH hosts the host's
own login is used (only `DEVCAT_` variables go there).

Plan usage (`ai/usage.rs`, `ai_usage`: the title bar, the AI connections list and the phone) reads each Claude Code
account's OAuth login where Claude Code keeps it (the keychain entry named after the config folder, through
`/usr/bin/security`; `.credentials.json` elsewhere) and only reads it: it never refreshes or writes a token back, so
Claude Code stays the login's one writer. Antigravity's (`agy`) comes from `agy --print /usage`. The phone gets the
percentages only, never a token or an email beyond what `ai_providers_list` already shows.

## JDBC connections

A `jdbc` connection (`src-tauri/src/db/jdbc.rs`) reaches a database through a driver jar the user supplies. DevCat has
no Java in it: it embeds a small helper (`src-tauri/jdbc-bridge/DevcatJdbc.java`, built into the committed
`devcat-jdbc.jar` by `build.sh`, Java 11 bytecode, no dependencies) and starts it with a Java that's already there,
speaking one JSON line each way over its stdin / stdout. With SSH on, DevCat logs in itself, copies the helper to
`~/.devcat/jdbc/` on that host and runs it there: the database is reached from that host (how a computer without Java,
or outside the database's network, uses JDBC). Keep these rules:

- **A JDBC connection runs programs**: its Java path and its driver jars. So agents can't add one
  (`agent_tools::workspace::KINDS`), as with a kubeconfig or VPN config an agent names, and nothing that comes from
  outside (a room, a chat app, the phone) may create or change one without the user in the window.
- **Driver jars are pinned**: saving the connection (`save_connection` → `jdbc::pin_jars`) records the sha-256 of
  every jar on this computer, and a jar that changed or appeared in a jar folder since isn't run (`pinned_jars`),
  so an agent that can write next to the jars can't get its code run by a query. The Java program isn't pinned
  (a JDK updates itself); jars on an SSH host are that host's.
- **Read-only can't trust one database's lexing**: `jdbc::read_only_sql` refuses `$`, `\`, `` ` ``, `[`, a `;`
  between statements and any writing or transaction word outside quotes (agents, a read-only phone and read-only
  connections all go through it). The helper's read-only session also rolls back after every statement, but that's
  a backstop only: DDL and an embedded `COMMIT` commit anyway.
- **Several databases go through `{db}`**: a driver that can't list a server's databases (SQLBase's) gets the names
  from the connection (`options.databases`, `jdbc::databases_of`), put into the URL one at a time, one helper each
  (`JdbcDriver` over `JdbcOne`). The names may only be letters, digits and `_-$#@`: nothing that could end the name
  or add a URL property. A request naming another database is refused, not tried.
- **Old drivers bend JDBC**: SQLBase's `isValid` always says false, `setSchema` / `getSchemas(c, p)` throw a plain
  `SQLException`, it decodes text with Java's default charset (the connection's text encoding sets it), and from
  server 11.5 on sends SQL holding non-ASCII as UTF-8, which a Shift-JIS database drops the connection over (a
  Japanese string literal): with a text encoding set, the helper makes it send SQL in that encoding
  (`sqlbaseLegacyText`). Fall back
  on any `SQLException` where a call is optional, and test against such a driver, not only H2.
- **Never ship a driver jar**: drivers are the vendors' (SQLBase's has its own licence). The user points at one.
- **Change the Java, rebuild the jar**: run `src-tauri/jdbc-bridge/build.sh` (needs a JDK 17+) and commit the jar
  with the source; bump `PROTOCOL` on both sides when the requests change. Building DevCat itself needs no Java.
- **stdout is the protocol**: the helper points `System.out` at stderr so a driver that prints can't break it, and
  DevCat skips any line that isn't JSON (a login banner over SSH).

## Ghost chats

A ghost chat (`agent_open` with `ghost_window`, `features/agent/ghost.ts`, a window with only the chat:
`GhostWindow.tsx`) starts from nothing: it ignores the folder it's given and works in an empty scratch folder of its
own, without rule files or skills (`system_prompt_with_home(…, None)`), MCP servers, hooks or DevCat connections. It
keeps nothing once it closes: its session and scratch folder live in a temporary folder (`agent.rs` `ghost_dir`) that's deleted when its tab or window closes
(`windows::on_destroyed` → `agent::close_ghosts_of`), when the app quits, and at the next start after a crash. It has
no memory, notes or rooms tools and no memory brief, isn't in `agent::list` or the Sessions list, saves no sandbox
choice and no ↑ history, and the phone dispatcher refuses every `agent_*` call on it. Anything new that records a
chat (titles, logs, notifications with its text) must skip ghost chats too.

A phone has ghost chats of its own (`agent_open` with `ghost`): tied to the phone (`agent::phone_ghost`, its device
id) instead of a window, so only that phone can use one (`dispatch.rs` checks `agent::ghost_of` on every `agent_*`
call) and a window's stay refused. One ends when the phone leaves its screen (`agent_close`) or stays away for
`PHONE_GHOST_GRACE` (`remote::disconnected`), and calls on it aren't in the phone activity log.

## DevCat Agent: turns that run for hours

The harness (`crates/harness`: `agent.rs` the loop, `session.rs` the log, `llm/` the model adapters) runs turns of
hundreds of steps, where anything that can go wrong once will. Keep these rules:

- **Nothing ends a turn that another try can fix.** `Llm::step` tries a step again when the provider is busy, the
  connection breaks, or the stream stops before the answer is complete (`Broken`, `RETRIES`), and says so
  (`Delta::Retry` → `AgentEvent::Retry`: what the step streamed before is void, and the chat takes it out,
  `model.ts`). An adapter must tell a complete answer from a cut one (`finished` / `stopped`); only a server that
  ends whole answers without saying so twice in a row, and never said "done", is believed (`quiet_end`,
  Chat Completions only). An answer that was complete stays, whatever the connection does after it.
- **Every loop is bounded.** Each `continue` in `Agent::run` has its own counter or flag (an empty answer is asked
  about once, a cut-off one goes on `MAX_CUT_OFF` times, a refused-as-too-long conversation is summarized once per
  step and at most `MAX_SQUEEZED` times in a row). A new one needs the same.
- **Summarizing must leave the model where it was.** The summary carries the user's messages and the open checklist
  word for word (`user_words`, `open_todos`), and the messages nothing answered yet (the one that starts the turn,
  with its images) stay whole after it (`Compacted::kept`). It isn't done again until the conversation grew
  (`WORTH_COMPACTING`): with a small window that would be every step. A summary that fails doesn't end the turn,
  and a conversation too long to summarize is cut down for the request (`shrink`), never the log.
- **A summary's `tail` replays events as they were** (`Compacted::tail`, `Session::tail_within`): it starts at one of
  the model's steps after the first since the last summary, so every call keeps its result; it takes in the
  unanswered messages (`kept` is 0 then); and a conversation that still doesn't fit is summarized again without it.
  Anything that copies or cuts a log (`Session::fork`) keeps every event before a `Compacted`, titles too: `tail`
  counts them.
- **The context is weighed without trusting the provider**: no token count means characters (`Session::weight`),
  a window guessed too large shows as the provider's "too long" (`llm::is_overflow`), and that is cured by a
  summary, not shown as an error.
- **`Session::messages` is what providers accept**: roles alternate, every tool call has its result, no message is
  empty (a step that said nothing is left out), and a turn that didn't finish is told to the model with the next
  message (`interrupted_note`), so "continue" continues.
- **What the harness tells the model itself** (an empty or cut-off answer, a helper out of steps) is a `User` event
  with `hook: NOTE` ("DevCat"): not the user's message (titles, counts, `user_words` skip it), and shown in the
  chat as "DevCat told the agent". A helper's stay in the helper's log.
- **A helper's work comes back.** At `HELPER_MAX_STEPS` it's asked for what it found (`wrap_up`) and that's its
  answer, marked as partial: an error there makes the agent ask the same helper again from the start. Whoever reads
  "the answer" of a chat reads `answer_of`: one cut off at the output limit goes on over several steps.
- **A tool can't take the turn down**: a panic in one fails its call (`catch_unwind` in `Agent::call`), a panic
  anywhere else ends the turn with an error and leaves the chat usable (`agent.rs` `crashed`), and a command that
  times out or is stopped is killed with what it started (`hooks::Tree`).

## DevCat Agent: tools, commands and settings

- **Calls run side by side only when that can't change the outcome** (`Tool::concurrent`): read, glob, grep and
  read-only git; approvals and PreToolUse hooks still go one at a time (`Agent::prepare`), results stay in call order,
  and with PostToolUse hooks nothing runs side by side (a hook that stops the agent must find the later calls not run).
  A new tool opts in only if that holds for every input it says yes to.
- **What the model is sent about tools comes from the log** (`agent::active_tools`, `Registry::declared`): with many
  MCP tools only the ones found with `tool_search` or used go along, in the registry's order. Don't keep that state in
  the agent: a chat opened again must declare the same tools.
- **Slash commands are text, never programs** (`commands.rs`): a template from a repository doesn't run its
  `` !`command` `` lines; what it asks for goes to the model as the message's context, and the log keeps what the user
  typed. Ghost chats have none.
- **Editing a message forks the chat** (`agent_fork`, `Session::fork`): the original stays as it was. Not for ghost
  chats (a copy would outlive them), not while the agent works, and not from the phone dispatcher.
- **Effort is only sent when the user chose it** (`LlmConfig::effort`): Auto sends nothing, because servers that don't
  reason may refuse the field. `None` in `agent_send` keeps the chat's choice (phone, jobs, chat apps).

## Agent bots and notification channels (`integrations/`)

`src-tauri/src/integrations/` puts DevCat in the user's chat apps: notification webhooks (Discord, Slack, plain) next
to the phones' own notifications, and bots (Telegram long polling, Discord gateway) that run from this computer. A
chat is *linked* to a folder (its project and memory), a DevCat Agent connection and `Perms` (notify, approve,
requests, files, connections, auto-edit), with a one-time code from DevCat (`/link CODE`) **and** the user's OK in
DevCat (`LinkPrompt`). What comes from a chat app is someone on another device, so keep these rules:

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

## Phone notifications

The computer decides what to notify ("finished", "needs you": the rules in `notify/mod.rs`) and `integrations::deliver`
sends it to the channels and linked chats, and to paired phones through `notify::to_phones` (event `agent-notify`, and
`notify_recent` for a phone that was offline). DevCat Mobile shows them itself (`mobile/src-tauri/src/watch.rs`);
there's no push service and no server. On Android the app stays running in the background only while the user's
"Stay connected" is on (a foreground service in `mobile/src-tauri/plugins/keepalive`). Anything new that should
notify goes through `deliver`, so phones and channels follow the same rules, and ghost chats never reach it.

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
- **Claude Code as the room agent has no shell** (`rooms_agent.rs` `run_claude`, `claude/headless.rs`
  `locked_args`): `--tools ""`, built-ins denied by name, `--strict-mcp-config` with only `devcat mcp --room <id>`
  (`Scope::room`: that room's agent tools, nothing else), none of the user's settings, hooks or plugins, and
  `dontAsk` with exactly the room tools allowed. An instruction not to use Bash isn't enough: keep the tools
  absent, and keep `Scope::room` offering nothing beyond the room tools.
- **A room linked to a project lends the room agent that project's memory, read-only** (`rooms_memory.rs`,
  `Prefs::folder` / `Prefs::project`, local, never shared; no `ssh://` folder). Its turn lists that place's
  memories (`memory::place_brief`: never the ones for every project, they're about the user), and it gets
  `memory_search` / `memory_read` for that place only (`memory::tools::read_only_for`) unless the user chose
  "brief only". It never gets `memory_save` / `memory_forget`: `memory_propose` leaves a proposal the user saves
  or dismisses in the window. No files of the folder either.
- **Nothing copies this computer's files into a room from an agent.** Sharing files and folders is a person's act
  in the window (`room_paths_add`, `room_file_add_bytes`); don't add a tool for it. Likewise nothing from a room is
  written on the computer (skills, memory, notes, files) without a person looking at it and confirming.
- **`devcat mcp` reaches the rooms through the app** (`agent_bridge.rs`, the one channel for every tool: 127.0.0.1,
  a fresh token in `<data>/agent-bridge.json`); `devcat mcp` can't open the rooms itself (one iroh key, one writer of
  the document), and `devcat serve` leaves them to the app (`mcp_server::tools_for`). Keep requests token-checked
  and loopback-only.
- **Root container names never contain `/`**: Loro panics when it imports a change to such a root (in the window and
  in Rust). Files in folders use `doc.rs` `root_part` (`/` → `%2F`, `%` → `%25`), and the window the same
  (`replica.ts` `rootPart`). Any new per-file container must go through it.
- **A room is deleted from a DevCat only on proof**: a member list the owner signed without that DevCat
  (`Room::wipe`, from `adopt`). Never on a peer's word: a `Denied { removed }` without a signed list only marks it
  read-only. And never send document changes to a peer that isn't a reader in the newest list.
- **Who shared, moved and took out what** goes in the room's `activity` (`doc.log`, or `rename` for moves); a new
  way into or out of a room records itself there. Who *edited* comes from the document's history (`doc.edits`):
  an agent's changes must be committed with `commit_by` so they show as the agent.
- **A room's GitHub** (`rooms_github.rs`, registered through `rooms_agent.rs` like every room tool): the
  repository and switches are local `Prefs::github` (Read, Write: off by default), the token is in the keychain
  (`room-github-token:<room>`), never in `room.json`, a prompt, a tool result or the room. Every post on GitHub
  goes through `rooms_github::confirm`: the user sees the exact text (secrets scrubbed) in the window and nothing is
  posted without their OK (no window: refused). Only the configured API host is called, without redirects.
- Rooms are desktop-only, like the SSH workbench: keep them out of the phone dispatcher.

## Page review

The Review tab (`src-tauri/src/review/`, `src/features/review/`) shows a web page (a dev server, a deployed site or a
local `.html`) in a native child webview named `review-<id>`, with `inspector.js` injected. **The reviewed page is
untrusted**: anyone's code can run in it. Keep these rules:

- **The page webview gets no capability except one command**: the remote-scoped `plugin:devcat-review|msg`
  (`channel.rs`), checked against the calling webview's label and top-level URL, then the per-review secret. Accepted
  risk: a page that patches `fetch` can read the secret and forge notes, but only in its own review.
- **No plugin page scripts that act on clicks.** A plugin's `js_init_script` goes into every webview, the review's
  too, and runs before the inspector: the opener plugin's link-click script took every `target="_blank"` / Ctrl
  click there and did nothing with it. It stays off (`open_js_links_on_click(false)`, `lib.rs`).
- **`capabilities/default.json` matches `webviews`, not `windows`**: a capability matched by window reaches the child
  webviews too, and with it every DevCat command. Keep it that way.
- **Never serve review content from a custom URI scheme.** Tauri treats a page from a scheme the app registered as a
  local origin with full IPC. Local files go through the loopback file server (`serve.rs`: 127.0.0.1, an unguessable
  review id in the path, Host check, no CORS, no dotfiles or secret-like files). An `.html` on an SSH host goes
  through the same server: its review folder is the `ssh://` URI, and `remote.rs` reads each file over one `ssh` per
  review, which the host checks again (folder resolved with `pwd -P`, must stay inside without passing a hidden
  folder, no symlinked files). Keep both checks for any new way of reading review files.
- **Refuse local targets** (`channel::is_local_target`: every scheme but http(s) and the inert `about:` / `data:` /
  `blob:`, any `*.localhost` host, and the app's dev URL) when a review starts and on every navigation.
- **Everything from the inspector is data** (`intake.rs`, `MAX_NOTES` in `review/mod.rs`): lengths capped, control characters stripped,
  backticks escaped (`format.rs`). Nothing is saved until Finish, and Finish saves exactly the text the user saw
  (`rev` must match), through `notes::save_checked`.
- **The inspector stays inert on other sites**: it acts only on the review's own site, never on a page that merely
  embeds it. The site is the origin the review started on, plus the redirects nearly every site makes on load:
  `http` → `https` and adding or removing `www.` (`channel::same_site`, `sameSite` in `inspector.js`; same port, never
  another host or a downgrade). Don't widen it further (subdomains, other hosts): an address on another site typed in
  the tab starts a new review instead (`review_nav` returns `false`). So DevCat calls into the page (`webview::call`: notes, highlight) only
  while the webview's top-level URL is the review's own (`channel::may_tell_page`): another site could define
  `window.__devcatReview` itself and read the notes.
- **Desktop only**, like the SSH workbench and rooms: keep `review_*` out of `src-tauri/src/remote/dispatch.rs`,
  `mobile/` and the browser build.

## Animations that run for a long time

A "working" label or a spinner can stay on screen for hours, and the window draws every frame of it (160 times a
second on a 160 Hz screen). An animation the GPU can't run on its own makes the page restyle and repaint on every
one of those frames: about 10% of a CPU core for a single spinner in WebView2. So an endless animation:

- **animates only `opacity` or `transform`.** Not `background-position` (the old "working" shimmer), `left`,
  `width`, `height` or colors; moving `left` also lays the page out again on every frame.
- **is never on an `<svg>`.** WebView2 runs a spinning svg on the main thread. Spin icons with `<Spin icon={…}>`
  (`components/ui`), which turns a span around the icon; `Spinner` uses it. Don't put `animate-spin` on an icon.
- **runs on the page, not in JavaScript.** No `requestAnimationFrame` loops or fast timers for something that
  should keep moving.

To check one, read Chromium's counters for the page over CDP (`Performance.getMetrics`): while it shows,
`RecalcStyleCount` should barely rise.
