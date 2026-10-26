# AI connections

Choose which coding agent DevCat runs, add your own command, and give an agent an API key.

An *AI connection* tells DevCat which agent CLI to start in the AI workspace. Claude Code is built in. You can add:

| Agent | Command | API key variable |
| --- | --- | --- |
| Claude Code (built in) | `claude` | `ANTHROPIC_API_KEY` |
| OpenAI Codex | `codex` | `OPENAI_API_KEY` |
| Gemini CLI | `gemini` | `GEMINI_API_KEY` |
| GitHub Copilot | `copilot` | `GH_TOKEN` |
| Aider | `aider` | the key of the model's vendor |
| opencode | `opencode` | `OPENAI_API_KEY` or another vendor's |
| Custom | any CLI that runs in a terminal | your choice |
| DevCat Agent (built in) | none: it talks to a model API | the provider's API key |

## Add an AI connection

1. In the AI workspace, open the agent menu at the top of the sidebar (it shows *Claude Code* at first).
2. Choose **Manage AI connections…**, then **Add AI connection**.
3. Pick the agent. DevCat shows whether its CLI is installed, with the install command if it isn't.
4. Optionally add arguments, **Environment variables** or an **API key**.
5. Click **Use for new chats** so the next **New chat** starts this agent.

## DevCat Agent

DevCat Agent is built into DevCat: no CLI to install. It talks to a model API directly and has its own chat view.

1. **Add AI connection** → **DevCat Agent**.
2. Pick a **Provider** (Z.ai GLM, DeepSeek, OpenAI, Anthropic, OpenRouter, Kimi, Ollama, LM Studio), or set the
   **API style** and **API address** of any OpenAI- or Anthropic-compatible endpoint. For OpenAI's newer models,
   including the ones only on its Responses API (such as `gpt-5-codex`), pick **OpenAI (Responses API)**.
3. Enter the **API key** and pick a **Model** from the provider's list.

What it asks before doing, in each permission mode (the menu under the chat box):

| Tool | Plan | Ask first | Auto-edit | Full access |
| --- | --- | --- | --- | --- |
| Read and search files, `git` status / diff / log / show / blame | never | never | never | never |
| Edit files in the project | not run | asks | — | — |
| Commands, `git` commands that change the repository, files outside the project | not run | asks | asks | — |
| Web pages | asks | asks | asks | — |
| Queries and cluster reads on your DevCat connections, reading your DevCat notes | asks | asks | — | — |
| Saving a DevCat note | not run | asks | asks | — |
| Helpers (see below) | never | never | never | never |

**Plan** is for looking before doing: the agent reads and searches, then answers with a plan, and edits and commands
don't run, not even with a yes. Under the plan, **Start, ask first** or **Start, auto-edit** switches the mode and
tells the agent to go ahead; or keep chatting to change the plan.

Approvals and the agent's questions also show up on DevCat Mobile. **Always allow … in this chat** stops asking for
that tool until the chat closes.

- The agent follows `AGENTS.md`, `CLAUDE.md` and the other agents' rule files, and uses the MCP servers from your
  other agents' configs (`.mcp.json`, `~/.claude.json`, Cursor).
- Long tool output (over 30,000 characters) is saved to a file next to the chat; the agent reads it in parts.
- The same tool call a 4th time in a row is refused, so a looping model tries something else.
- A `git` command runs without asking only when it just reads the repository, with that command's usual options
  written out in full (`--verbose`, not `--verb`). Anything else asks, and so does an option that could make git
  start a program (`--ext-diff`, `--textconv`, `--output`, `describe --dirty`). Reading commands also don't run
  the programs a repository's config names (fsmonitor hooks, external diff tools, textconv and clean filters), and
  don't fetch a missing object from a partial clone's remote.
- When the context fills up, the conversation is summarized and the chat carries on (or type `/compact`).
- The agent can search and read your [notes](notes.md), and save one when you ask: the approval shows its text.
- For research that takes many reads, the agent asks **helpers**: each gets one question and a fresh context, looks
  around on its own and answers the agent. Several run at the same time. Each is a row in the chat showing what it's
  doing; click it for its question, its steps and its answer. A helper never edits files, runs commands or asks you:
  whatever would need your OK in the current mode is skipped. Its tokens count in the chat's total and cost.
- **Rename** a chat: right-click it in the sidebar's chat list → **Rename…** (<kbd>Enter</kbd> saves,
  <kbd>Esc</kbd> cancels).
- Next to the token meter, **≈ $** estimates what the chat's tokens cost at the model's list price (from the
  provider's model list, or else OpenRouter's catalog). Hover it for the prices used.
- **Trace** (next to Chat and Diff) shows how the chat went, turn by turn: each model step with its time and
  tokens, each tool call with its input, output and time, who approved it, hook messages and summaries. A bar
  shows where each step falls in its turn. Click a helper's call to see its own run. **Log** opens the session file.

### Sandbox

**Sandbox** (next to the permission mode under the chat box) runs the agent's commands (`shell`, `git`) in the
operating system's sandbox. They can still read any file, but they can write only in the project folder, the temp
folders and `/dev`, so a command you allow can't change your home folder or other projects. Commands still ask in
**Ask first** and **Auto-edit**: the sandbox limits what a command writes, not what it reads or sends.

- macOS uses Seatbelt (`sandbox-exec`); Linux uses Landlock (Linux 5.13 or newer). Windows isn't supported yet, so
  the button doesn't show there.
- It limits writing, not reading or the network: a command can still read your files and download or send things.
- On macOS a command can't write in the project's `.git` either (a hook or a setting planted there would run on
  the next git call, outside the sandbox); the agent's `git` tool can, for commits. On Linux `.git` stays writable.
- Tools that write outside the project fail inside it: package caches (`~/.npm`, `~/.cargo`), global installs, your
  git config. Turn the sandbox off for those, or run them yourself.
- If the sandbox can't be used, the command isn't run (it doesn't run unsandboxed instead).
- A scheduled job can run its commands in the sandbox too (an option in its editor). The sandbox is set on the
  computer: a chat you continue on your phone keeps it, also after DevCat restarts.

### Scheduled jobs

A scheduled job runs a prompt on a schedule in a folder: a morning report, a nightly check of the tests, a weekly
look at outdated dependencies. In the AI workspace sidebar, with a DevCat Agent connection selected, click **+**
next to **Scheduled** (above the folder's chats) and set:

- what the agent should do (the prompt);
- when: at a time of day on the days you pick, or every so many minutes or hours (at most every 5 minutes);
- the DevCat Agent connection and the permission mode the run uses.

Each run is a new chat in the folder, named after the job and the time ("Morning report · Oct 23, 09:00"). Click a
job to open its last run; right-click it to run it now, edit, pause or delete it.

- Jobs run while DevCat is open. A run that came due while DevCat was closed runs once when it opens. A run that
  DevCat was closed in the middle of shows as stopped.
- Opening a run's chat and closing its tab doesn't stop the run. **Stop** in the chat does.
- Jobs are kept in `agent-jobs.json` in DevCat's data folder. If that file can't be read (a hand edit with a
  mistake, or jobs from a newer DevCat), the list says so and DevCat doesn't write over it: fix the file or move it
  away.
- A run asks for approvals like any chat, in its chat and on your phone. A toast tells you when a run finishes or
  needs your OK, and so does your phone if [notifications](mobile.md#notifications) are on. **Plan** runs never need
  you: they only read and answer.
- The folder's hooks run in scheduled runs too, once you've allowed them.

### Hooks

DevCat Agent runs the hooks you wrote for Claude Code or Codex: commands that run before a tool call, after it,
when you send a message or when the agent is done. It reads them from the same files:

- Claude Code: `hooks` in `~/.claude/settings.json`, `.claude/settings.json` and `.claude/settings.local.json`;
- Codex: `~/.codex/hooks.json` and `.codex/hooks.json`.

Hooks run commands on your computer, so they're **off until you allow them** for the folder. A chat in a folder
with hooks lists the commands and shows **Run hooks**, which allows exactly what it lists: if a hook file changed
in the meantime, nothing is allowed and the chat shows the hooks as they are now. Allowing them is done on the
computer; DevCat Mobile shows that the folder has hooks waiting.

What you allow includes the scripts the hooks run (`.claude/hooks/check.sh`, or the file after `bash`, `python3`,
`node` and the like). When a hook or one of its scripts changes, you're asked again, and a changed script doesn't
run until then, not even in a chat that's already open.

A hook gets the event as JSON on stdin and answers as it does for Claude Code: exit code 2 blocks (its stderr is
the reason), or a JSON answer on stdout (`decision`, `hookSpecificOutput`, `systemMessage`, `continue`). Tools have
the names and fields those agents use (`Bash` with `command`, `Edit` / `Write` / `Read` with `file_path`, `Task`,
`mcp__server__tool`), so their matchers and scripts work as they are; DevCat's own names (`shell`, `edit`, …)
match too. `git` calls are seen as `Bash` commands.

| Event | When | What a hook can do |
| --- | --- | --- |
| `SessionStart` | The first message of a chat (`startup`, or `resume` for a saved chat) | Add context for the agent |
| `UserPromptSubmit` | You send a message | Add context, or block the message |
| `PreToolUse` | Before a tool call | Block it, allow it without asking, make it ask, change its input, add context |
| `PermissionRequest` | A call is about to ask you | Allow or decline it in your place |
| `PostToolUse` / `PostToolUseFailure` | After a call that worked / failed | Give the agent feedback, add context, replace the result |
| `Stop` / `SubagentStop` | The agent (or a helper) is done | Send it back to work with a reason |
| `PreCompact` / `PostCompact` | Before / after the chat is summarized | Stop the summary |

Hook messages (`systemMessage`, a hook that failed, or why a Stop hook sent the agent on) show up in the chat.
**Stop** doesn't wait for a running hook: it ends the hook and what it started. A hook that runs past its `timeout`
(600 seconds unless it sets one; 30 for `UserPromptSubmit`) is ended the same way. When a hook answers
`"continue": false` after a tool call, that call's result stands, the step's other calls don't run, and the agent
stops.

- On Windows, hooks run in Git Bash when it's installed (most hooks are shell scripts), else in PowerShell. A
  hook with `"shell": "powershell"` or Codex's `commandWindows` runs in PowerShell.
- Only `command` hooks run, not `http`, `prompt`, `agent` or `mcp_tool` ones, and only the events above. The chat
  says which hooks it skips.
- Codex hooks written in `config.toml` (`[hooks]`) aren't read; put them in `hooks.json`.
- `"disableAllHooks": true` in any Claude Code settings file turns off the hooks of all of them, as in Claude Code.
  Codex's `hooks.json` isn't affected.

## Logins and API keys

Each agent uses its own login by default: run the CLI once in a terminal and sign in the way it asks.

If you'd rather use an API key, enter it in the connection. DevCat keeps it in the system keychain (macOS Keychain,
Windows Credential Manager) and passes it to the agent as the variable shown under **Passed as**. It is never
written to a plain file.

## What differs between agents

- **Claude Code** gets the session list, the launch options and the **Code Review** view.
- **Other agents** get a **Review** button that asks the agent itself to review the changes in its terminal.
- **DevCat Agent** has a chat view instead of a terminal, its saved chats in the sidebar, and approvals on the phone.
- The **Diff** view and extra terminals work the same for every agent.
