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
project's `folders`). Over MCP, data changes need the project's `mcp_writes`. Sync MCP edits files on the
computer, so it stays out of the phone dispatcher, like the SSH workbench above.

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
