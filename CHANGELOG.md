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

- **DevCat Agent in folders on SSH hosts**: a chat opened in a folder of an SSH session (or any `ssh://` folder) now
  works on that server. Reading, writing, editing, searching, commands and git all run there over the session's own
  login (key, jump host, saved password), with nothing installed on the server. The agent knows the server's OS and
  shell, reads the project's AGENTS.md / CLAUDE.md there, and files you attach are copied to the server for it. The
  chat says which server it works on. Hooks, skills and the sandbox stay off in such a chat: they belong to this
  computer. Linux and macOS servers. Searching is faster where the server has ripgrep.
- **SSH sessions for agents**: an SSH session's **AI / MCP access** (Edit connection → Advanced) lets agents on this
  computer work on that server. With **Read**, they read and search files there. With **Full**, they also run
  commands and change files. This gives the DevCat Agent and Claude Code, Codex, Cursor or Gemini (through Sync MCP)
  the `ssh_read`, `ssh_glob`, `ssh_grep`, `ssh_run`, `ssh_write` and `ssh_edit` tools. It's **Off** until you turn it
  on, since agents log in as you. Agents never get these tools from a phone, a chat app or a ticket run, and files
  that hold secrets (keys, `.env`) aren't read for them.

## 0.33.2 - 2026-11-20

- **DevCat Agent: long turns hold up** (#223): a helper that reaches its 60 steps now answers with what it found instead
  of failing, so the agent no longer asks the same helper again from the start. A summary of the conversation that
  comes back empty is asked for again, and one that fails no longer ends the turn. A small context window isn't
  summarized after every step anymore, a conversation the provider refuses as too long is summarized and the step
  taken again, and each summary keeps your messages word for word (the one you just sent stays as it is, with its
  images). A connection that drops while the model answers
  is tried again (the chat says so) instead of ending the turn or passing half an answer on, and an answer cut off
  at the output limit goes on where it stopped. Reasoning some providers send twice shows once. After a pause at
  the step limit, "continue" carries on instead of starting over. A command that times out or is stopped takes what
  it started with it (a build, an install), so the next command doesn't wait on its locks. The chat
  points out an MCP server whose tools weigh on every request (hundreds of tools can be half the context window).
- **Terminal: shows through window glass** (#221): with window glass on, terminals were solid black and **Work area
  opacity** didn't change them. They now show the glass like the rest of the work area. Without glass nothing changes.

## 0.33.1 - 2026-11-20

- **Rooms: Claude Code as your room agent reads GitHub** (#219): with Claude Code chosen as the room's agent, it
  now gets the room's GitHub tools (**Room settings → GitHub**, **Read** and **Write**), as the DevCat Agent already
  did. Before, it answered that it had no access to GitHub. A tool that is off says why (no token, or the switch).

## 0.33.0 - 2026-11-20

- **Rooms: Claude Code as your room agent** (#214): **Room settings → AI connection** lists your Claude Code
  connections next to DevCat Agent ones (each Claude account separately). With Claude Code chosen, a mention runs it
  on your Claude plan, with only the room's tools: no shell, files, web, or your own settings, hooks and plugins. It
  shows in the thread as "*your name*'s agent (Claude Code)".
- **Rooms: link a room to your project** (#215): **Room settings → Project folder** links a room to a folder or a
  DevCat project on this computer (only for you, never shared), and your agent there works with that project's
  memory: its list, plus search and read unless you choose the list only. It can't change your memory: it proposes
  entries, and you edit, save or dismiss them from the room header. Whatever it reads can end up in the thread.
- **Terminal: follows the app's background** (#212): terminals show the color and transparency set in **Appearance**
  (theme, window glass, **Work area opacity**) instead of a box of their own, in every place a terminal shows. Colors
  programs set (vim, htop, Claude Code) are unchanged. **Appearance → Terminal → Terminal uses the app background**
  turns it off for a solid terminal.
- **AI connections: Hermes Agent** (#216): add Nous Research's [Hermes Agent](https://hermes-agent.nousresearch.com)
  (`hermes`) as an AI connection and choose its **Provider** and **Model** in the connection: Nous Portal (its free
  plan runs the models ending in `:free`), OpenRouter (its free models are suggested), Anthropic, OpenAI, Gemini,
  DeepSeek, Z.ai, Hugging Face, LM Studio, a local server such as Ollama (**Custom / local**, with its address) or any
  other provider id Hermes knows. Left empty, Hermes uses what you set up with `hermes model`.
- **Rooms: GitHub issues and discussions** (#213): set a room's GitHub repository and a token in **Room settings →
  GitHub**, and your agents there can read its issues and discussions (**Read**) and create issues or answer in
  discussions (**Write**). Every post shows you the exact text first and goes to GitHub only when you confirm it;
  its link goes in the thread. A pinned decision or a task can become an issue from its own button too.
- **Tickets: GitHub boards can take any open issue** (#211): **Take issues labelled** has a new first choice, **Any
  open issue (no label needed)**, for repositories where nobody labels issues. The board then takes every open issue
  without one of its state labels (pull requests left out). The watch takes an issue only if it was opened after the
  board was made by someone who can write to the repository (or is in **Also take issues from**); others are sent to
  the agent by hand. Taking the state label off an issue runs it again. Boards with a trigger label don't change.

## 0.32.0 - 2026-11-19

- **Tickets: Stop stops the agent** (#203): **Stop** on a board's run ends the agent within a few seconds, and the
  run is recorded as stopped, with nothing pushed and the ticket back where it was. Before, the agent worked on to
  the end and its work was pushed. **Stop** and the board's **Time limit** also act while Claude Code sends output
  all the time.
- **Tickets: a worktree for each ticket** (#191): set a board's **Working copy** to **A worktree for each ticket**
  and the agent works in a git worktree in the repository's `.worktrees` folder, made from the latest **Base branch**
  on origin (or from the ticket's own branch when it has one, with any newer commits someone pushed to it on origin;
  when your copy and origin's both have commits the other lacks, the run fails and asks you to bring them
  together). You can keep working in the repository meanwhile, with your own changes and branch. DevCat commits
  there, pushes when the board says so and removes the worktree; the branch stays. Existing boards stay **In
  place**.
- **Tickets: worktree setup** (#192): a board with a worktree for each ticket can copy **Files to copy** (files git
  ignores, such as `.env`) from your repository into each new worktree, and run a **Setup command** (such as
  `pnpm install`) there before the agent starts. The command needs **Let the agent run commands**. If it fails, the
  run fails without the agent, with the command's output in the history and the ticket's comment.
- **Tickets: worktrees after a crash** (#194): when DevCat starts, it removes the worktrees and folders that runs
  left in a worktree board's `.worktrees` folder when DevCat stopped in the middle (a worktree a run kept for you to
  look at stays). A run whose branch is checked out in another worktree fails with a message that names it.
- **Tickets: runs cut short by a crash** (#203): when DevCat quits or crashes during a ticket's run, Claude Code
  stops too (on macOS and Linux) instead of working on unseen. At the next start, DevCat moves the ticket from the
  working column to the board's failed column (`ready-for-human` on GitHub) with a comment that names the branch
  and says how to try again, and adds the run to the history. A ticket someone moved since stays where it is.
- **Tickets: GitHub boards** (#195): connect a GitHub repository (`owner/name`, an API address for GitHub Enterprise
  and a personal access token). Labels are the columns: the agent takes open issues labelled `ready-for-agent` and
  assigned to you (**Assigned to** starts at **Me** on a new GitHub board), labels them `agent-working`, and a fixed issue's branch (`<your login>/<number>-<title>`) is pushed and gets a
  draft pull request that closes the issue, linked in the issue's comment, with the label `agent-review` (a ready
  one, and the comment says so, where the repository has no draft pull requests). When DevCat can't open the pull
  request, the issue gets `ready-for-human` with a comment that says why and names the branch. Saving the board
  checks the token and creates the labels the repository lacks.
- **Tickets: parallel runs** (#193): a board with a worktree for each ticket can work on up to 4 tickets at the same
  time (**Parallel runs** in the board's settings, default 1), each in its own worktree and branch. The **Working**
  panel shows each ticket with its log, and the other tickets wait. Boards that run in place still do one at a time.
- **Tickets: what the agent reads on GitHub** (#197): the agent gets the issue's comments from people with write
  access to the repository (such as an Agent Brief, or answers to its questions), and none from anyone else. The
  images in the issue and in those comments are shown to it, also in a private repository.
- **Tickets: GitHub issues the agent can't finish** (#198): when the agent has questions, its branch is pushed, the
  issue's comment has the questions and the issue gets the label `needs-info` (**When the agent has questions,
  label** in the board's settings). When it fails, its unfinished work is committed, pushed, and the comment has the
  reason and the branch, with the label `ready-for-human`. Neither opens a pull request.
- **Tickets: GitHub issues the agent may take** (#196): a watching GitHub board takes an issue only when the person
  who added its trigger label can write to the repository or is listed in **Also take labels from**, when no open
  issue blocks it (GitHub's issue dependencies or a `Blocked by: #n` line in its body; it starts after its last
  blocker closes), and when the label was added after the board was created. Fix older issues from the list.
- **Tickets: pull request follow-up** (#201): at each check, a watched GitHub board looks at the pull requests DevCat
  opened. When one merges, the issue loses `agent-review` and is closed if it's still open. When one closes without
  a merge, the issue is closed as not planned with the label `wontfix` (**When its pull request closes without a
  merge, label** in the board's settings) and a comment that links the pull request,
  and the branch is deleted on GitHub and in your repository, unless it has newer commits there (or a worktree has
  it checked out). The history shows what became of each pull request.
- **Tickets: pull request base and template** (#200): a GitHub issue's pull request goes into the branch of a
  `Base: <branch>` line in the issue, else in its parent issue, else the board's **Base branch**, else the default
  branch, and its worktree starts from that branch. A pull request into the default branch says `Closes #<number>`,
  into another branch `Part of #<number>`. The agent writes the description in the format of the repository's pull
  request template, and DevCat adds a line that says the DevCat agent opened it. When the description doesn't say how
  the agent checked its work (neither its testing steps nor a **How it was checked** section, at any heading level),
  DevCat adds a **How it was checked** section with the agent's testing steps.
- **Tickets: run a GitHub issue again** (#199): add the trigger label to an issue again (after you answer the
  agent's questions, or to send a `ready-for-human` issue back) and a watched GitHub board runs it again at the next
  check, in a new conversation that reads the new comments, on the issue's branch with the earlier commits. Without
  a new label it doesn't run again. The comment for questions or a failure says how.
- **Tickets: a time limit for each board** (#189): **Time limit** in a board's settings (minutes, default 60) is
  the longest the agent works on one ticket, and a worktree's **Setup command** runs. When it's up, DevCat stops it
  and the run ends with an error that names the limit.
- **Tickets: connecting a GitHub board** (#203): **Let the agent run commands** is now right under **Working copy**,
  above the **Setup command**, and the dialog says before you click **Connect** when a setup command needs it (with
  **Turn it on**). When the token can't see the repository (a private repository answers 404 to such a token), the
  error (on **Connect** and on **Load columns**) says so and how to give the token access. **Create a token** opens GitHub's fine-grained token form with the
  name, owner and permissions filled in; the hint says to choose **Only select repositories** and the repository.
- **`devcat serve` keeps the computer awake, also with the lid closed** (#204): while it runs (in a terminal or as the
  background service), the computer doesn't sleep, also when the screen locks or the lid closes, so phones reach it.
  On macOS that turns system sleep off, which asks for your password once, and back on when it stops. Linux uses a
  systemd inhibitor; on Windows set closing the lid to "Do nothing". Start it with `--no-lid` to let the lid sleep it,
  or `--no-keep-awake` to sleep as usual. The macOS background service also runs at normal priority now.

## 0.31.0 - 2026-11-19

- **Plan usage for Claude Code and Antigravity** (#187): the title bar shows how much of your plan is used (click it
  for every limit and when it resets): Claude Code's 5-hour and weekly limits, for each account, and Antigravity's
  pools. The AI connections list and each connection show it too, and so does the agent picker on your phone.
- **Antigravity in the AI workspace** (#187): add an **Antigravity** AI connection to run Google's terminal agent
  (`agy`), signed in with the Google account agy uses (or a Gemini API key).
- **Tabs on more rows** (#187): in **Appearance → Tabs**, keep one row that scrolls, or let tabs wrap onto new rows
  when the bar is full.
- **Each window picks its own agent** (#187): the agent new chats use is chosen per window, so another window or
  project keeps its own.
- **Terminals match the work area** (#187): terminals and the Claude Code tab use the same background as the rest of
  the work area instead of a darker one (with window glass they stay see-through).
- **DevCat Mobile in light mode** (#187): the app follows your phone's light or dark setting, terminals included;
  choose in **More → Appearance**.
- **Keep awake from the start, and for phones** (#183): keep awake turns on when DevCat starts (turn that off in the
  cup's menu), and while phones are paired the computer stays awake so they can reach it with the screen off.
  On the phone, images can go with a new chat's first message, and terminals have one layout.

## 0.30.0 - 2026-11-19

- **Several Claude Code accounts** (#181): in AI connections, **Add connection → Claude Code (another account)** adds a
  Claude Code connection with a login of its own: start a chat with it and Claude Code asks you to log in. The list
  shows which email each one uses. Chats, settings, `CLAUDE.md`, skills and plugins stay shared, so switching a tab to
  another account goes on with the same chat (handy when one account hits its limit). Folders on SSH hosts use the
  host's own login.
- **Cursor in the AI workspace** (#175): add a **Cursor** AI connection to run Cursor's CLI agent (`cursor-agent`) in a
  workspace tab, like Claude Code: pick its model and mode (Agent, Plan, Ask or Force) in the header, and each tab
  keeps its own Cursor chat, resumed every time the agent restarts (its id shows next to the options). DevCat finds
  the CLI as `cursor-agent` or `agent`; log in with `cursor-agent login` or give the connection an API key
  (`CURSOR_API_KEY`).
- **Ctrl+drag in a terminal no longer leaves boxes over the text** (#174): Ctrl+press, drag and release (say over a
  file link while Claude Code runs) used to leave empty or stale rectangles painted over the terminal until those
  lines redrew.

## 0.29.0 - 2026-11-18

- **Run .sql files in the query editor** (#176): a `.sql` file's tab has a **Query editor** button that opens the script
  (with any unsaved edits) on a connection you pick, either just opened or run right away. Agents can do the same
  with the new `db_open_query` tool: it opens SQL, a `.sql` file or a saved query in a query tab and, when asked, runs
  it there so you see the results in DevCat's grid. A read runs on a read-only session; a script that changes data
  runs only on a connection whose AI access lets agents change data, otherwise the tab opens and you press Run.
- **Hand work to a cheaper or local model** (#177): turn on **Coding agents may hand it work** for a DevCat Agent connection
  (in AI connections), and Claude Code, Codex and other agents connected to DevCat get an `agent_delegate` tool: they
  give that model a focused task in their folder, it reads and edits files there (no commands, web pages or edits
  outside the folder, since nobody is there to approve them), and they get back only a short summary and the list of
  changed files. Only the connections you turn it on for are offered.
  The chat stays in DevCat so you can read what it did. Agents are told about the models you allowed when they
  connect, so they hand work over on their own.
- **Local models report their context window** (#177): DevCat now reads the window llama.cpp-style servers report
  (`meta.n_ctx`), so the DevCat Agent compacts in time on them, and scheduled jobs ask the model's own server before
  guessing.

## 0.28.0 - 2026-11-17

- **DevCat Agent keeps its plan through long work** (#172): when a long chat is summarized to free the context window, the
  agent's checklist now goes into the summary, so it carries on with the same plan instead of forgetting it. The plan
  panel also stays when you say "continue" after the agent paused. And the agent now pauses after 200 steps per
  message instead of 60 (change it with **Steps per turn** in the connection's settings; 0 means no limit).
- **Notifications on the phone without ntfy** (#170): DevCat Mobile now gets the computer's notifications itself (the
  same "Notify me when" rules), and catches up on the ones it missed while offline. On Android, **Stay connected in
  the background** (More → Notifications) keeps the app connected. ntfy is gone from the Notifications tab.
- **A simpler DevCat Mobile** (#170, #171): four tabs (Chats, Terminal, Files, More); Chats puts what needs you on top
  with an "Ask an agent…" box; Terminal opens a plain shell or Claude Code in a folder you pick; tapping a
  notification opens its chat. Terminals scroll by touch and wrap at the phone's width.
- **Ghost chats and project focus on the phone** (#170, #171): start a ghost chat (no project, memory or connections,
  keeps nothing) from the phone, and pick a project to see only its chats, terminals and connections.
- **Android fixes** (#170): DevCat's own launcher icon instead of the placeholder, and scanning the pairing QR code no
  longer crashes release builds.

## 0.27.0 - 2026-11-16

- **JDBC connections** (#164): any database with a JDBC driver (SQLBase, Oracle, Db2, Informix, H2…) as a connection,
  with its tree, table data, grid edits, queries and export, and for agents over MCP like any other database. DevCat
  stays free of Java: its small helper runs on a Java you already have (JAVA_HOME, an installed JDK, the one in
  DBeaver or DataGrip), or, with SSH on, on another computer that has Java and can reach the database. You bring the
  driver's jar. For older databases that don't store Unicode (a Japanese SQLBase in Shift-JIS), set the connection's
  text encoding, such as MS932, and names and values read correctly. For a server whose driver can't list its
  databases (SQLBase), list them on the connection and put `{db}` in the URL: one connection shows them all.
- **New tmux variant** (#167): an SSH session's menu gets **New tmux variant** below **New variant…**. One click
  saves a variant that runs `tmux new -A -s <session>-<n>` on login, with no dialog; rename it with **Edit variant…**.
- **Downloading a file from an SSH host starts right away** (#166): no more long wait before the Replace prompt or the
  progress toast. It logs in once instead of twice, shows *Connecting…* from the click on and then the percentage,
  and a download that fails before any bytes arrive no longer empties a file of the same name already there.
- A query or table filter with an unquoted non-ASCII name (`WHERE 曜日 = '木'`) no longer leaves it running forever.

## 0.26.0 - 2026-11-16

- **Split panes** (#165): put several tabs side by side in one window. Right-click a tab and choose **Split right**
  or **Split down**, or drag a tab onto another pane's edge; a terminal or SFTP tab can also open a second one on the
  same target beside it. `Ctrl+Alt+Shift+\` splits right, `Ctrl+Alt+Shift+-` splits down and `Ctrl+Alt+Shift+Arrow`
  moves between panes (`⌥⇧⌘` on macOS), also from inside a terminal. Moving a tab keeps its shell and unsaved text.
  The layout isn't kept across restarts.

## 0.25.1 - 2026-11-15

- **A new icon** (#162): DevCat's cat now has a `>_` face and a ring of the tools it works with, in the app, the DEV
  build, DevCat Mobile (with a proper home-screen icon on iPhone and Android) and the website.
- **PDFs use far less memory** (#161): pages you scroll past let go of their image and draw again when they come back,
  so a few long PDFs no longer grow DevCat's window to gigabytes.

## 0.25.0 - 2026-11-13

- **Word documents look like Word** (#159): a `.docx` opens as pages laid out the way Word shows them (fonts, headings,
  lists, tables, images, headers and footers) instead of plain text. Zoom with the toolbar or Ctrl+wheel; the text
  button still shows the plain text. Files over 40 MB keep the plain-text view.

## 0.24.2 - 2026-11-12

- **New-tab links in a review open again** (#157): in a reviewed page, a `target="_blank"` link (such as a menu
  item that opens in a new tab) and a Ctrl or Shift click on a link now open a new Review tab. In 0.24.1 they still
  did nothing.

## 0.24.1 - 2026-11-12

- **HTML previews load their own files** (#153): a page's relative stylesheets, images, icons and (with **Run
  scripts** on) scripts now load in the Preview, so pages no longer show unstyled or empty. Only files in the
  page's folder or the project are read, never hidden or secret-looking ones.
- **New-tab links in a review on Windows** (#154): a `target="_blank"` link, a Ctrl or middle click, or
  `window.open()` in a reviewed page now opens a new Review tab on Windows too.

## 0.24.0 - 2026-11-11

- **SSH session variants** (#151): right-click an SSH session and choose **New variant…** to add a child that
  changes only what it sets, such as another address for the same machine (LAN or Tailscale) or another tmux
  session. Anything a variant leaves empty comes from its session. Port forwards are never inherited.
- **Sort SFTP panes** (#149): click Name, Size or Modified to sort a pane, and click again to reverse it. Folders
  stay on top, and each side remembers its choice.
- **Links in HTML previews** (#148): clicking a link to another file opens it in a tab instead of blanking the
  preview, and `http(s)` and `mailto` links open in the browser.
- **New-window links in a review** (#146): a `target="_blank"` link or `window.open()` in a reviewed page opens a
  new Review tab. On Windows it did nothing before.
- **Tabs keep their scroll position** (#145) when you switch away and back, and the tab bar scrolls to show the
  active tab (#147).
- **Terminal after a dropped connection** (#150): when a program like tmux dies without cleaning up, DevCat turns
  off the modes it left on, so the terminal no longer fills with mouse junk and the saved SSH password is typed
  again on reconnect.

## 0.23.0 - 2026-11-09

- **Review tab**: click elements on a page to comment on them, and save the review as a short note to copy to a
  developer or an agent. It works with a dev server, a deployed site, or an `.html` file on this computer or on an
  SSH host, and **Copy review** puts the note on the clipboard.
- **Download button for every file on an SSH host**: a file from an SSH host that DevCat can still preview (an
  HTML or text file too large to edit, an image, a PDF) had no way to download it; the Download button only showed
  for files with no preview. It's now in the file's toolbar too.
- **Pair a phone by typing a code** (#143): on the browser version the computer serves, type the code DevCat shows
  instead of photographing the QR code. A wrong code can't be tested offline, and five wrong tries end the pairing.
- **`devcat serve` in the background** (#143): `devcat service install [serve options]` runs it as a launchd agent
  (macOS) or systemd user unit (Linux) that starts at login and restarts after a crash (also `service uninstall`,
  `start` and `status`). `devcat serve` in a terminal now shows the one already running instead of taking over its
  port, and `detach`, Ctrl+D or Ctrl+C leave it running; `devcat attach` does the same without ever starting one.
  Pairing codes never go to the service's log.
- **Home Screen app on iPhone** (#143): typing the pairing code no longer flickers or hides behind the keyboard, and
  the page no longer sits under the blurred status bar.
- **Links in terminals** (#136): clicking a link printed with an OSC 8 hyperlink opens it in the browser, and
  clicking a path to a file that isn't there says "File not found" instead of opening an empty tab.

## 0.22.0 - 2026-11-05

- **Claude Code sessions on SSH hosts** (#130): the Sessions list of an SSH folder now shows the past Claude Code
  sessions on that host (its own `~/.claude`), and opening one resumes it there in a new Claude tab, so the history
  stays the host's. New Claude chats there get their session id from the start, so restarting one picks it up again.
  Linux and macOS hosts.
- **Git on SSH hosts** (#130): the Git sidebar now lists the repositories in an SSH folder instead of failing with
  "Not a directory", and they open in a full Git tab: history, diffs, blame, staging, commit, branches, tags,
  stash, merge, fetch, pull and push, all run by git on the host (with the host's own credentials for fetch, pull
  and push). Blame and file history also work on files from SSH folders. Linux and macOS hosts.
- **Download files from SSH hosts** (#133): a file on an SSH host that DevCat can't preview (a video, an archive, a file
  too large to show) now has a Download button. Pick a folder on this computer; progress shows in a notification
  you can cancel, and when it's done you can reveal the file.
- **Claude Code sessions list was empty** (#134): a folder whose newest Claude Code session had moved into a
  subfolder early on (`cd backend`) showed "No sessions yet in this folder" although it had many. Its sessions show
  again, and the folder is no longer counted under that subfolder.

## 0.21.0 - 2026-11-03

- **Agents can open files in DevCat** (#131): ask Claude Code, Codex, Cursor, Gemini or the DevCat Agent to "open it in
  DevCat" and the file opens in a DevCat tab, a PDF in the viewer and code in the editor, with the new
  `devcat_open_file` tool. The agent doesn't get the file's content from it. It needs the DevCat app open.

## 0.20.0 - 2026-11-02

- **Security fixes** (#126): a security audit found places where text from outside (a ticket's description, a room
  member, another computer, an agent's answer) could get DevCat to do more than you allowed. Those are closed, and
  the vulnerable dependencies are updated. What you may notice: SSH tunnels for databases and network profiles now
  check the server's host key (from `~/.ssh/known_hosts`, else the first key seen is remembered) and refuse one that
  changed. Images in Markdown that comes from outside (agent answers, files, notes, rooms) no longer load from the
  network by themselves: they show as a button that opens the address. Ticket boards that don't allow commands no
  longer run the repository's commit hooks on the agent's commits, and a ticket's Claude Code gets the same limited
  tools as a ticket's DevCat Agent. Queries that an agent or a read-only phone may run because they only read now go
  through a separate read-only database session, so the database refuses writes too. Chat bots don't send
  `.git-credentials` or similar credential files.
- **SSH terminals on Windows hosts** (#125): a saved SSH session to a Windows computer closed straight away
  (`'exec' is not recognized…`). A plain shell now opens the account's own shell. With a start folder, a startup
  command or an agent terminal (such as `claude`), DevCat runs it as PowerShell, so write a Windows host's startup
  command for PowerShell. The file tree, preview, git and SFTP don't work on Windows hosts yet.
- **DevCat Agent: steps per turn** (#124): the agent no longer stops after a fixed 60 steps ("Paused after many
  steps"). Each DevCat Agent connection has **Steps per turn** (default 60, 0 for no limit), and chats that are
  already open pick up a change.
- **Pasting images into Claude Code over SSH** (#127): a pasted or dropped image now shows as `[Image #1]` instead
  of its path on the host, and a small chip in the terminal's corner shows while it's being copied there.
- **OpenVPN with older servers on Windows and Linux** (#128): a VPN whose server runs OpenVPN older than 2.4.5
  connected but let nothing through, because OpenVPN's Data Channel Offload (DCO) drops that server's packets ("Data
  Channel Offload doesn't support DATA_V1 packets"). DevCat now notices this, turns DCO off for that profile and
  connects again (Windows asks for administrator rights once more). The profile also has a **Turn off Data Channel
  Offload** checkbox.
- **SFTP downloads over an existing copy** (#120): downloading a folder into one that already holds a copy no longer
  stops with "Permission denied" at the first read-only file (git's objects, for example).
- **Rooms** (#121): hovering a message no longer pushes the thread down by a line. **React** now sits in the
  message's hover toolbar, next to **Reply** and **Pin**. The agent and people chips above the message box are
  listed alphabetically.

## 0.19.0 - 2026-11-01

- **Agents can do much more in DevCat** (#122): Claude Code, Codex, Cursor, Gemini CLI (through Sync MCP) and the DevCat
  Agent can now connect your VPNs and tunnels, set up new ones from a `.ovpn` / WireGuard file, add connections (from
  a project's `.env`, say), work with Docker, RabbitMQ and more of Kubernetes (events, metrics, Helm, scale, restart,
  apply), import into and restore databases, read your query history, schedule DevCat Agent jobs and read your ticket
  boards. DevCat holds them to your settings: each connection's **AI / MCP access** as before, and now one for each
  VPN and tunnel too (**Off**, **Read & connect**, **Full**). Connections an agent adds start at read-only access, VPN
  configs from an agent can't run scripts, and secrets stay hidden. What can't be undone (deleting a Kubernetes
  object, a container or image, a queue, a VPN profile, restoring over a database) is for the DevCat Agent only and
  asks you to type its name, like dropping a database. With DevCat open, these calls run in DevCat, so a VPN it
  connects works for the agent, and a new kind of connection brings its tools into the agent's session right away.
  See **What agents can do** and the new **walkthrough** in the docs.
- **Less CPU while agents work or something loads** (#119): the "working" label on AI workspace tabs now pulses
  instead of shimmering, and spinners (loading, refreshing, the ✦ next to "working") turn on the GPU. Both used to
  make the window redraw on every frame for as long as they showed: on a 160 Hz screen, about 18% of a CPU core
  while an agent worked and 10% for a single spinner, now about 3% either way. The SFTP progress bar for a transfer
  of unknown size no longer lays the page out again on every frame either.

## 0.18.0 - 2026-10-30

- **Ask DevCat Agent about a connection:** right-click a database, Redis, MongoDB or Kubernetes connection (or use
  **Ask agent** in a query tab, the robot in a cluster's sidebar) for a DevCat Agent chat about it. It starts out
  knowing the connection: a database's tables and columns, a cluster's namespaces (structure only, never rows), so
  you can ask right away. The agent can also **dump** a database to a .sql file (a folder of JSON lines for MongoDB),
  **export** a table or a query's rows to CSV, TSV, JSON, SQL or Excel (to Downloads unless you say where), and
  **drop a database**. Dropping asks twice, whatever the permission mode: click **Drop…**, then type the database's
  name. It's confirmed on the computer only (not from a phone), never over MCP, and read-only connections and the
  server's own databases can't be dropped.
- **Agent bots (Telegram, Discord).** Settings → **Agent bots** adds a Telegram or Discord bot (it runs from
  this computer: no server) and links a chat to a folder with a one-time code (`/link CODE` in the chat, then your OK
  in DevCat). For agents working in that folder the chat gets **Allow / Deny** buttons and their questions, and the
  result when they finish; it can send **new requests** (they run as a DevCat Agent chat in the folder, with its
  project's memory), `/status`, `/stop`, `/summary`, `/files` and `/file <path>` to get files, and `/open` for a link
  that opens the chat in DevCat Mobile on a phone already paired with the computer. Each link chooses what it may do
  (notifications, approvals, requests, files, editing without asking, DevCat connections). Only the person who
  linked a chat is listened to there; key and password files are never sent; dropping a database is confirmed on
  the computer only; ghost chats are never sent.
- **More notification channels:** Mobile → Notifications sends "finished" / "needs you" to Discord and Slack webhooks
  (or any webhook) as well as ntfy, and DevCat Agent chats now notify too, not only Claude Code.
- **Ghost chats:** the ghost icon in the top bar (or an AI workspace tab's header) opens a window with only a
  DevCat Agent chat that starts from nothing and keeps nothing. No project: it works in an empty scratch folder of its
  own, without rule files, skills, MCP servers, hooks, DevCat connections, memory, notes or rooms. It isn't listed in
  Sessions or on your phone, and what you type isn't kept for ↑. Closing the window deletes the chat and its scratch
  folder. (The model's provider still receives the conversation, as for any chat.)
- **Setup guide and tour:** a fresh install opens a short guide: pick your coding agent (DevCat checks which ones are
  installed and shows how to install the rest, or sets up the built-in DevCat Agent), a project folder, any
  connections, and your phone, agent bots, Sync MCP and memory. Then a tour shows where everything is, one part at a
  time. Skip either at any step; both are in Settings (Set up DevCat…, Take the tour) and on the Welcome tab.
- **A new welcome page:** start an agent chat, a ghost chat or tickets; open Git, Rooms, VPNs, notes and the
  database tools; connect a database, cluster or server; and set up your phone, agent bots, Sync MCP and the shared
  memory, all from the first tab.
- **AI workspace files: search the whole folder.** The search box above the file list finds files in folders you
  haven't opened (with .gitignore respected), showing them with the folders leading to them; type a `/` to match the
  path. Folders on SSH hosts still filter the folders you've opened.
- **AI workspace files: Delete.** Right-click a file or folder → **Delete…** (or Delete, ⌘⌫ on macOS). On this
  computer it goes to the Trash / Recycle Bin; in a folder on an SSH host it's deleted for good (there's no Trash
  there), and the confirmation says so.
- **AI workspace files: Pin.** Pin a file from the file list (right-click → **Pin**), its tab (right-click → **Pin
  file**) or the pin button in the file's toolbar: it stays at the top of **Files** under **Pinned** for that
  folder, to open again in one click. Pins are kept on this computer.
- **Keep awake with the lid closed (macOS):** macOS sleeps when the lid closes whatever apps ask, so agents and
  networks stopped. The cup's menu now has **Keep awake with the lid closed…**: DevCat turns system sleep off
  (`pmset disablesleep`, which asks for your password) and turns it back on when keep awake ends: when you turn it
  off, its time is up, DevCat quits, or at the next start after a crash. If sleep was already off, it's left alone.
- **Title bar:** **Mobile pairing** (the phone) and **Keep awake** (the cup) are icons in the title bar, next to
  Settings, instead of items in the Settings menu. The phone turns blue while phone access is on and shows a dot when
  a phone wants to pair; the cup shows how long the computer is kept awake.

## 0.17.0 - 2026-10-30

- **Tickets: answer the agent** (#114): a board's panel has **Working**, **Chat** and **History** tabs. When the
  agent needs a person (a question, a decision, something it couldn't do), the ticket gets a thread in **Chat**:
  answer there and the agent goes on with the ticket in the same chat, on the branch its last try left. The answer
  stays in DevCat (it isn't posted to the tracker). Clicking a run in **History** opens its chat in the AI workspace.
- **Crash screen** (#112): when the window hits an error, it shows what went wrong (with **Copy details** and
  **Reload**) and a **Report on GitHub** link that fills in an issue for you to check and send, instead of a blank
  window. Nothing is sent by itself.
- **Terminal:** right-clicking a terminal opens DevCat's own menu: **Copy** (the selection), **Paste**, **Select all**
  and **Clear**. The webview's menu could never copy a terminal selection, so its Copy was always greyed out. When
  a program like Claude Code or tmux has the mouse, the menu says how to select in the terminal instead
  (Shift+drag, ⌥+drag on macOS).
- **SSH sessions on Windows:** the tab no longer reads `C:\Windows\System32\OpenSSH\ssh.EXE`; it shows the
  session's name until the host sets a title (a shell's prompt, Claude Code's task).
- **SSH sessions on Windows:** no more console window flashing up for a moment when pasting an image, opening a
  folder on an SSH session, starting a port forward or opening a terminal on a session with a saved password, nor
  when a Kubernetes cluster logs in through its kubeconfig's helper (aws, gke-gcloud-auth-plugin, kubelogin).
- **Tickets on Windows:** a repository two folders down in a board's folder is called `group/deep`, as on macOS and
  Linux, not `group\deep`, in what the agent is told and in the run's notes.

## 0.16.0 - 2026-10-29

- **Rooms: a place to work together** (#108):
  - **Files and folders:** the room's files are a tree. Share whole folders (dropped, or from this computer), rename
    and move files and folders (right-click, or drag onto a folder), save a file or folder to your computer, and read
    Markdown with a live preview next to the editor.
  - **Whiteboards** to sketch ideas together (Excalidraw); agents read the text on them.
  - **Thread:** replies, reactions, decisions pinned at the top, and `@name` / `@everyone` mentions that notify
    people.
  - **Tasks** for a person or an agent: giving one says so in the thread, and an agent takes it and marks it done.
  - **Share from DevCat:** a note, memory, skill or a repository's changes. **Take out:** add a room's skill to your
    skills, save a file to your memory, copy one to your notes. Nothing is written on your computer before you've
    looked at it and confirmed.
  - **Team memory:** the room's `memory/` folder, read by every member's agent; agents propose entries that a person
    accepts.
  - **Your coding agents in a room:** turn it on per room (Room settings) and Claude Code, Codex, Cursor (through
    `devcat mcp`) and DevCat Agent chats can read the room, write in the thread, suggest changes and handle tasks, as
    your agent. No tool copies your files into a room.
  - Files in folders no longer crash the room tab or the document (Loro refuses `/` in a root container's name).
  - **Removing someone deletes their copy:** their DevCat deletes the room (files, whiteboards, thread, tasks,
    keys) as soon as it learns, at once if they're online, else the next time it reaches anyone in the room. Only
    on proof (your signed member list), so nobody else can make a DevCat delete a room. What they saved outside
    the room stays with them.
  - **Who shared and edited it:** each file shows who shared it and from where; **Who edited it** lists who
    shared, moved, took it out (to their computer, skills, memory, notes) and edited it, and when, from the room's
    own document (peer to peer, no server). **History → Who shared what** shows the whole room's.
  - A member you remove no longer receives what was written while DevCat was telling them they were removed.
  - Typing in a room's file works on Windows: the editor used Windows line breaks for a file with plain ones, so
    every keystroke landed in the wrong place, didn't reach the room, and was undone.

## 0.15.0 - 2026-10-28

- **Tickets:** choose the branch the agent works on in the board's settings: a new branch per ticket with your own
  prefix (`devcat/` by default), or an existing branch such as `main` or `staging` that fixed tickets are committed
  straight onto (unfinished work still goes on a branch of its own). Each history entry has **open chat**, which
  opens the agent's conversation for that ticket in the AI workspace (#110).
- **Delete a project from the project menu:** the menu at the top left has **Delete project**, so you can remove a
  project without finding it in the Connections sidebar. DevCat asks first, and the project's connections are kept (#107).
- **File names with spaces are clickable in terminals and Claude Code tabs:** a path like
  `exports/Akun Simulasi Internal 26 Okt 2026.xlsx`, shown as code by Claude Code or in quotes by a tool, opens
  on a click like any other path (Ctrl/⌘-click: in its default app), once DevCat has checked the file is there (#105).
- **AI connections:** saving a DevCat Agent connection no longer fails with "Invalid API key variable name" when
  another type was picked first and its "Passed as" field filled in. A Claude Code, Codex or Gemini connection
  set up the same way now passes its key in its own variable, not the other type's hidden one (#98).
- **New chat stays new:** changing the model or permission mode of a new Claude Code chat and pressing **Restart
  claude** no longer opens another tab's conversation in the same folder. A chat with no message yet starts
  fresh again; one you've written in picks up where it was (#101).
- **Find in spreadsheets and CSV files:** ⌘F (Ctrl+F) in a spreadsheet or CSV preview searches every row, not only
  the ones on screen. It highlights the matching cells and counts them (**3 of 17**); Enter and Shift+Enter go to
  the next and previous match, in the table's sort order. Before, only the rows on screen could be found (#102).
- **Data grids:** moving the selection down with the arrow keys no longer leaves the selected row just below the
  visible part of the table (#102).

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
  - Homebrew: `brew install devcathq/devcat/devcat-cli`, or `--cask devcathq/devcat/devcat` for
    the app.
