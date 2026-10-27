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

- **File names with spaces are clickable in terminals and Claude Code tabs:** a path like
  `exports/Akun Simulasi Internal 26 Okt 2026.xlsx`, shown as code by Claude Code or in quotes by a tool, opens
  on a click like any other path (Ctrl/⌘-click: in its default app), once DevCat has checked the file is there (#105).
- **AI connections:** saving a DevCat Agent connection no longer fails with "Invalid API key variable name" when
  another type was picked first and its "Passed as" field filled in. A Claude Code, Codex or Gemini connection
  set up the same way now passes its key in its own variable, not the other type's hidden one (#98).
- **New chat stays new:** changing the model or permission mode of a new Claude Code chat and pressing **Restart
  claude** no longer opens another tab's conversation in the same folder. A chat with no message yet starts
  fresh again; one you've written in picks up where it was (#101).

## 0.14.0 - 2026-10-27

- **Tickets in the left bar:** Tickets has its own view in the left bar instead of an entry in the Settings menu. It
  lists your boards and what the agent is doing on each (a dot on the icon while it works); click one to see its
  tickets. In focus mode only the focused project's boards are listed.
- **Tickets: the project's memory.** A board belongs to a DevCat project (chosen, or the one whose code folder holds
  its repository), and the agent working on its tickets, Claude Code or DevCat Agent, gets that project's shared
  memory and connections, the same ones a chat there uses.
- **Tickets: prompts.** A board's **Default prompt** (formerly *Instructions for every ticket*) goes with every
  ticket; **With a prompt…** (or the speech bubble on a ticket) fixes the tickets you pick with a prompt of your own
  instead, that time only. The history shows the prompt used.
- **Tickets: Jira screenshots.** Images attached to Jira tickets often didn't reach the agent ("error sending
  request"): DevCat now downloads them from the Jira site itself instead of Atlassian's media server, tries again
  once, and the history says why when an attachment still can't be read.

## 0.13.0 - 2026-10-26

- **Sync MCP:** each connection now has its own **AI / MCP access** (Edit connection → Advanced): **Off** hides it
  from every agent, **Read-only** lets agents only read, **Full** lets agents over MCP change data too. New
  connections start read-only, and a read-only connection can never be full. DevCat enforces it, whatever the
  agent asks. Connections saved before keep following their project's **Allow writes from MCP** until you pick
  one. **MCP activity** (Sync MCP → Activity…, or the connections menu) lists every call agents made through
  DevCat's MCP server: the agent, the connection, what it ran and whether DevCat refused it. Agents can also
  attach the connection list and a database's schema as MCP resources (`devcat://project/…/connections`,
  `devcat://conn/…/schema`).
- **Tidier window:** the row of icons at the top right is now one **Settings** menu (Appearance, AI memory, Mobile
  pairing, Tickets, Keep awake, New window); a coffee cup still shows while keep awake is on. **Rooms** moved to
  the left bar, and saved queries and query history now sit under the connection list (**Queries**) instead of
  having their own icon.
- **File links on SSH sessions:** clicking an absolute or `~/…` path in an SSH session's terminal (or in an
  agent's reply there) opens that file from the server, over the same session, instead of looking for it on
  this computer.
- **DevCat Agent: a `git` tool.** Commands that only read the repository (status, diff, log, show, blame, …)
  run without asking; commit, checkout, push and anything else that changes it asks first, like a shell
  command (#53).
- **DevCat Agent: long tool output** (over 30,000 characters) is saved to a file next to the chat, and the
  agent reads it in parts instead of losing the middle (#53).
- **DevCat Agent stops repeating itself:** the same tool call a 4th time in a row is refused, so a looping
  model tries something else (#53).
- **DevCat Agent and your notes:** it searches and reads your DevCat notes (asking first in **Ask first**) and
  saves a note when you ask, showing its text before it does. The Notes list updates at once (#55).
- **Rename a DevCat Agent chat:** right-click it in the sidebar → **Rename…** (#55).
- **Estimated cost** of a DevCat Agent chat next to its token meter, at the model's list price (#55).
- **DevCat Agent plan mode:** pick **Plan** in the permission menu and the agent only looks around and answers
  with a plan; edits and commands don't run. **Start, ask first** or **Start, auto-edit** under the plan carries
  it out (#56).
- **DevCat Agent and the OpenAI Responses API:** a new API style and an **OpenAI (Responses API)** preset, for
  OpenAI's newer models, including the ones only offered there such as `gpt-5-codex` (#57).
- **DevCat Agent helpers:** for research that takes many reads, the agent hands questions to helpers. Each starts
  with a fresh context and only looks: it never edits, runs commands or asks you. Several run at the same time, their
  rows show what they're doing, and their tokens count in the chat's total and cost (#59).
- **DevCat Agent runs your Claude Code and Codex hooks** (`.claude/settings.json`, `.codex/hooks.json` and the ones
  in your home folder): before and after tool calls, when you send a message, when the agent is done, around
  summaries. They're off until you allow them for the folder, and you're asked again when a hook or a script it
  runs changes. Stop ends a running hook (#62).
- **DevCat Agent Trace:** a Trace view next to Chat and Diff shows each turn's model steps and tool calls with
  their times, tokens, inputs and outputs, who approved what, and the helpers' own runs (#63).
- **Scheduled DevCat Agent jobs:** run a prompt in a folder at a time of day or every so many minutes (**+** next to
  **Scheduled** in the AI workspace sidebar). Each run is a chat in the folder, and you get a toast (and a phone
  notification, if they're on) when it finishes or needs your OK. Jobs run while DevCat is open (#64).
- **DevCat Agent sandbox** (macOS and Linux): **Sandbox** under the chat box runs the agent's commands in the
  operating system's sandbox, where they can write only in the project (and, on macOS, not in its `.git`). Commands
  still ask: the sandbox limits writes, not reading or the network (#67).
- **Focus mode hides other folders' tabs too:** chats, terminals, files and Git tabs of folders that aren't the
  focused project's are hidden, like other projects' connections and notes. A folder is the project's once you use
  it while the project is focused. The AI Workspace's recent folders list the project's own; the others are under
  **Other folders**, and **Remove from this project** takes one out (#77).
- **Closing or restarting a Claude Code tab lets Claude Code exit on its own** (Ctrl+C, then up to 3 seconds) instead
  of killing it. Killed while starting, Claude Code counted it as a failed full-screen start, and after two it
  turned full-screen off on the computer, so its agent list could no longer be clicked. Run `/tui fullscreen` once
  to turn it back on (#87).
- **A preview tab keeps up with its file:** opened before the file exists (an agent mentions a path, then writes
  it), it shows the file as soon as it's written, instead of staying on "Cannot open …". Images, PDFs and other
  previews that change on disk are shown again a moment later (#88).
- **Rename a Claude Code session:** right-click it in the sidebar's Sessions list → **Rename…**. It's saved the
  way Claude Code's `/rename` does, so `claude --resume` shows the same name. Clear the name to go back to
  Claude's own title (#89).
- **Send while DevCat Agent works:** a message you send while it's busy goes in before its next step, as in Claude
  Code: no need to stop it first. It shows right away as waiting; if you stop the agent before it reads it, the
  text comes back to the chat box. Your `UserPromptSubmit` hooks check it too (#69).
- **New lines in the DevCat Agent chat box** with **Ctrl+Enter** and **Alt+Enter** too, not only **Shift+Enter** (#69).
- **↑ / ↓ in the DevCat Agent chat box** go through what you sent in that folder, as in Claude Code (#69).
- **The "files changed" bar in a DevCat Agent chat** lists only files git still sees changed, so it goes away when
  you commit or revert them, also from another tool (#69).
- **Rooms:** sharing a large text file (a 2 MB Markdown file, say) froze the window for a minute, and the
  room then froze again each time it was opened. Rooms with such a file open again, and text files over
  200,000 characters are now shared as read-only files (save them to read them) instead of editable documents.
- **Room agents see the room's images:** an agent mentioned in a room now looks at shared PNG, JPEG, GIF and WebP
  images (up to 5 MB) when it reads them, and can read text files shared read-only because of their size.
- **Rooms:** an image open in a room no longer blinks each time someone in the room types, moves or posts.
- **Terminal:** copying from Claude Code works. Text you select in Claude Code (or tmux, vim, a program on an SSH
  host) now reaches the clipboard: DevCat accepts OSC 52 copies, and never lets a program read the clipboard. New
  **Terminal** settings in Appearance: **Selecting text** copies on select (as before) or only with ⌘C / Ctrl+C
  (Ctrl+C with nothing selected still interrupts), and **Allow programs to copy to the clipboard (OSC 52)**. Both
  apply to every terminal at once, open tabs too (#61).
- **Memory on all your computers:** pair your laptop, desktop or a server running `devcat serve` once (**Memory →
  Other computers**: show a code on one, paste it on the other and allow it), and their agents share one memory.
  Changes reach the other computers within seconds while they're online, and a computer that was off catches up
  when it's back. Computers connect directly or through your relay, end-to-end encrypted, with no account or
  server. A project's memories follow it to computers that have the same project (matched by its git remote). When
  both computers changed the same memory, the newer change wins and the other is kept as a conflict copy, and
  secrets are removed before anything is shared (#12).
- **Rooms: drop files to share them.** Drag files onto a room, from your computer or DevCat's Files tree, and
  everyone in it can open them. Dropped on the thread, or pasted into its message box (a screenshot), they're
  also named in your message, and a file's name in a message opens it. A name that's taken gets a number
  (`notes (2).md`) instead of failing (#92).
- **Rooms: read large text files in the room.** A text file shared read-only because of its size now opens in the
  room, read-only, instead of only offering to save it. Agents read long room files in parts, where before they
  got only a file's start and end (#92).

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
