# Changelog

What changed in each DevCat release. The release notes on GitHub and the "new version" dialog in
the app show the section of that version.

When a pull request changes something users notice, add a line under **Unreleased**. At release
time, rename **Unreleased** to the version (`## 0.8.2 - 2026-10-23`) and start a new empty
**Unreleased** above it, in a pull request from a branch named `release/v0.8.2` (with the version
bumped in `package.json`, `src-tauri/tauri.conf.json` and `mobile/src-tauri/tauri.conf.json`).
Merging it tags `v0.8.2` and publishes the release (`.github/workflows/release.yml`). A version without a section gets the list of merged pull requests instead.
Plain Markdown only: the app's dialog doesn't render HTML tags.

## Unreleased

- **Memory on all your computers:** pair your laptop, desktop or a server running `devcat serve` once (**Memory →
  Other computers**: show a code on one, paste it on the other and allow it), and their agents share one memory.
  Changes reach the other computers within seconds while they're online, and a computer that was off catches up
  when it's back. Computers connect directly or through your relay, end-to-end encrypted, with no account or
  server. A project's memories follow it to computers that have the same project (matched by its git remote). When
  both computers changed the same memory, the newer change wins and the other is kept as a conflict copy, and
  secrets are removed before anything is shared (#12).

## 0.12.0 - 2026-10-24

- **Tickets:** take only tickets assigned to you (or to nobody yet). A board can point at a folder of
  repositories, not just one: the agent changes the ones the ticket needs, on the same branch in each, and a
  repository with your uncommitted changes is left alone (before, such a folder failed with "is not a git
  repository"). The agent also sees the ticket's screenshots: Jira and Trello attachments, and images in a
  Linear description (#82).
- **Memory:** one memory for all your coding agents, with nothing else to install. Claude Code, Codex, Gemini,
  Cursor and the DevCat Agent read and save it through DevCat's MCP server, and each new session starts with a
  short brief of it, so you can switch models mid-task and the next one knows what the last one learned. An agent
  that stops mid-task can leave a handoff that the next agent picks up. **Sync Claude memory to all agents** (the
  brain button at the top right) copies Claude Code's own memory in and can keep it in sync. Browse, edit and
  forget memories in the same dialog (#81).
- **Sync MCP:** your coding agents can now use your DevCat connections without setup. The plug button above
  the connections (or a project's menu) adds DevCat's MCP server to Claude Code, Codex, Cursor and Gemini CLI,
  replacing DevCat's earlier entry instead of adding a second one. You can also tick other MCP servers that reach
  the same databases to have DevCat replace them. Agents then read schemas and run queries (SQL, MongoDB, Redis)
  and read Kubernetes clusters, with passwords staying in DevCat. Give a project its **code folders** (project
  menu → Code folders & MCP…) and its connections go only to agents working in those folders, DevCat Agent
  included; connections in no project, or in a project without folders, go to every folder. Over MCP, agents
  only read unless the project has **Allow writes from MCP** turned on. After the first sync, DevCat keeps the
  agents' configs up to date as your projects change. `devcat mcp` runs the server by hand (#80).

## 0.11.0 - 2026-10-24

- **Tickets:** connect a Jira (Cloud or Data Center), Trello or Linear board and let the agent work through a
  column. Pick tickets (or let DevCat watch the column) and the agent fixes each on its own `devcat/<ticket>`
  branch. DevCat commits the work, comments on the ticket with what changed and how to test it, and moves it
  on (say, To Do → In Progress → Testing, or to Blocked when it needs a person). Works with Claude Code or a
  DevCat Agent connection; results can be posted to Slack. Open it from **Tickets** in the title bar (#78).

## 0.10.0 - 2026-10-24

- **Rooms:** share documents with other people and let everyone's DevCat Agent join in, peer to peer
  (no account, no server holding your documents). Make a room from **Rooms** in the title bar and send an invite
  link; you let each person in. Everyone edits the same files live (edits made at the same time merge; undo only
  undoes yours), talks in a thread, and mentions agents (`@arif-agent`, `@agents`). Agents read the room,
  claim the part they work on and suggest changes that a person accepts; every applied change can be reverted.
  Removing someone switches the room to a new key. `devcat keeper` keeps changes for whoever is offline,
  encrypted so it can't read them (#72).
- **DevCat Mobile from other networks:** connects faster and more often goes direct instead of through the
  relay. The computer asks your router to open its port (UPnP / NAT-PMP / PCP), the pairing QR and the
  computer's details carry the relay server it's on so the phone goes straight there, and the phone waits
  longer before giving up on mobile data. Phones paired before keep working. DevCat → Mobile and the phone show
  whether each connection is direct or through the relay (#71).
- **Docker connections:** add a Docker engine (the current `docker` context, another context, or a host over
  `ssh://` / `tcp://`) and open it in a tab: containers grouped by Compose project with their CPU and memory,
  logs that follow (and carry on after a restart), a shell in a terminal tab, inspect, and start / stop /
  restart / pause / kill / remove; images, volumes and networks with inspect and remove. A read-only
  connection only looks (#70).
- **`devcat stop`:** stops a `devcat serve` that's still holding the port: one in the background, in a closed
  terminal or from before an update. Starting `devcat serve` while another DevCat has the port now says which
  one and offers to stop it and take over (`--replace` does it without asking).
- **Paste in the file tree:** in the AI workspace, paste files and folders copied in Explorer / Finder (or with
  the tree's own Copy) into a folder: right-click → **Paste**, or **Ctrl+V** / **⌘V**. A name
  that's taken gets "copy", so nothing is overwritten, and an image on the clipboard is saved as `pasted.png`
  (#54).
- **DevCat Agent on Windows:** a console window no longer flashes up and closes for every command the agent runs
  (#68).
- **DevCat Agent's Stop button** shows its square with window glass on (it was a plain white circle), and so do the
  search highlights in Kubernetes logs (#68).
- **DevCat Agent's plan** (the checklist above the chat box) goes away when the agent is done, also when it left an
  item unticked; each update stays in the conversation (#68).

- **SSH sessions on Windows:** the saved password is typed again (the Windows console's own replies no longer
  count as typing); answering ssh's own questions (host key, a jump host's password, a one-time code) no longer
  stops it; pasting a screenshot (Alt+V) works; selecting text in a terminal copies it; terminal tabs follow the
  program's title (#50).
- **Open folder on SSH host:** pick one of your saved SSH sessions (with its saved password) instead of typing a
  host (#66).
- **Terminal tables with emoji** (like Claude Code's ⚠️ rows) line up: characters are measured by grapheme
  cluster, as current terminals do (#65).
- **Browser version of DevCat Mobile:** a pairing the computer no longer knows is forgotten instead of failing
  forever, and scanning a new QR code for the same computer pairs again (#60).

## 0.9.1 - 2026-10-23

- **Terminal on Windows:** **Ctrl+C** copies the selection (and still interrupts when nothing is
  selected) and **Ctrl+V** pastes, like Windows Terminal. This works in the AI workspace, the
  Terminal tab, SSH sessions and the pod shell (#47).
- **Clickable Windows paths:** `C:/x/y.html`, `C:\x\y.html` and `src\a.ts` (with `:line:col`)
  open in DevCat's file tab, with HTML rendered in Preview. **Ctrl+click** (**⌘-click** on macOS)
  opens a local file in its default app. A path wrapped over several rows is one link, and dragging
  over a link now selects it instead of opening it (#47).

## 0.9.0 - 2026-10-22

- **SSH sessions:** a new "SSH session" connection with host, user, password or key, jump host,
  start folder and a command to run on login. Double-click opens a terminal; the menu also opens
  SFTP or an AI workspace on the host. Sessions can save port forwards (`-L`, `-R`, SOCKS) that
  you start and stop from the sidebar. You can paste images into Claude Code running on the host
  (#39).
- **SFTP:** your computer on one side, the host on the other. Copy with buttons, the menu or drag
  and drop (also from Finder), with a transfer queue showing progress (#39).
- **Quick commands:** a bar of saved commands under terminals and agent chats, for every session
  or just one (#39).
- **Go to File:** **Ctrl+P** / **⌘P** fuzzy-searches every file in the folder,
  with recently opened files first, like VS Code (#28).
- **Browser and phone version:** DevCat Agent chats open there with the same chat view as the
  desktop, including approvals, questions, attachments and the model picker. The activity log says
  who approved what (#31, #35).
- **VPN:** OpenVPN profiles with `user` / `group` no longer leave a route behind when they
  disconnect, which could stop every app, OpenVPN Connect included, from reaching that VPN server
  after the Wi-Fi address changed (#48).
- **Fixes** (#38):
  - Importing in replace mode no longer empties the table when the file can't be read.
  - Exporting a query result no longer runs `INSERT … RETURNING` / `DELETE … RETURNING` twice.
  - Saving a file no longer overwrites newer changes made in the same instant.
  - Read-only connections now also block SQL Server dynamic SQL and PostgreSQL / MySQL read-write
    transactions.

## 0.8.2 - 2026-10-22

- **DevCat Agent:**
  - Long chats are summarized automatically when the context window fills up, or on demand with
    `/compact` (#32).
  - The agent can fetch web pages, and keeps a plan you can follow above the message box (#32).
  - It can read your DevCat connections: database schemas and queries, Kubernetes resources and
    logs, with your approval first (#33).
- **Browser version:** the on-screen keyboard no longer hides the terminal, and you can pair by
  taking a photo of the QR code (#25).
- **HTML preview:** a page that needs JavaScript says so, with a **Run scripts** button (#37).
- **AI workspace:** the header fits narrow windows, and nothing wraps or spills over (#29).
- **Updates:** the "new version" dialog shows what changed in that version, with a link to the
  whole changelog.

## 0.8.1 - 2026-10-22

- **DevCat has a new home:** [github.com/devcathq/DevCat](https://github.com/devcathq/DevCat).
  Updates, downloads and the pairing page for the browser version
  ([devcathq.github.io/DevCat](https://devcathq.github.io/DevCat/)) come from there. Old links
  forward to it (#41).
- **Website and docs:** [devcat.ai.id](https://devcat.ai.id) (#40).
- A phone that paired through the browser at the old address pairs once more at the new one.

## 0.8.0 - 2026-10-22

- **DevCat Agent:** a coding agent built into DevCat, with its own chat in the AI workspace. It
  asks before edits and commands, asks you questions, takes attachments, and uses your MCP
  servers, rules and skills (#22, #24).
- **Bookmarks** in the Connections tab, for databases, schemas, tables and other objects (#16).
- **Zoom:** ⌘/Ctrl `+` `-` `0` for the whole window, and for terminals when one has focus. There's
  also a setting under Appearance (#14, #17).
- **Fonts:**
  - code fonts are bundled, so Claude Code's status line looks right on Windows;
  - the font pickers list your installed fonts;
  - dropdowns are readable on Windows (#13, #14, #21).
- **Copy files** to the clipboard from the Files tree and the file preview, to paste into
  Finder, Explorer or Slack (#19).
- **Browser version on phones:** pair once and install it as an app, with an easier terminal (#23).
- **Pod shell:** copy and paste with the keyboard (#30).
- **Website and user docs**, with step-by-step guides (#20).

## 0.7.1 - 2026-10-20

- **Tabs** can be dragged to reorder them.
- **New chat** always opens a new tab.
- **macOS:** DevCat appears under *Open With* for code and text files, and opens them in an editor
  tab.

## 0.7.0 - 2026-10-19

- **DevCat CLI (`devcat serve`):** pair DevCat Mobile and phone browsers with a computer that has
  no window, such as a server or a machine you reach over SSH. The pairing QR code shows in the
  terminal.
- **Install from a terminal:**
  - `curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh` (macOS, Linux);
  - `irm https://devcathq.github.io/DevCat/install.ps1 | iex` (Windows);
  - Homebrew: `brew install devileya/devcat/devcat-cli`, or `--cask devileya/devcat/devcat` for
    the app.
