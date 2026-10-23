# Changelog

What changed in each DevCat release. The release notes on GitHub and the "new version" dialog in
the app show the section of that version.

When a pull request changes something users notice, add a line under **Unreleased**. At release
time, rename **Unreleased** to the version (`## 0.8.2 - 2026-10-23`) and start a new empty
**Unreleased** above it. A version without a section gets the list of merged pull requests instead.
Plain Markdown only: the app's dialog doesn't render HTML tags.

## Unreleased

- **`devcat stop`:** stops a `devcat serve` that's still holding the port: one in the background, in a closed
  terminal or from before an update. Starting `devcat serve` while another DevCat has the port now says which
  one and offers to stop it and take over (`--replace` does it without asking).
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
