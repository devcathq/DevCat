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
   **API style** and **API address** of any OpenAI- or Anthropic-compatible endpoint.
3. Enter the **API key** and pick a **Model** from the provider's list.

What it asks before doing, in each permission mode (the menu under the chat box):

| Tool | Ask first | Auto-edit | Full access |
| --- | --- | --- | --- |
| Read and search files, `git` status / diff / log / show / blame | never | never | never |
| Edit files in the project | asks | — | — |
| Commands, `git` commands that change the repository, web pages, files outside the project | asks | asks | — |
| Queries and cluster reads on your DevCat connections, reading your DevCat notes | asks | — | — |
| Saving a DevCat note | asks | asks | — |

Approvals and the agent's questions also show up on DevCat Mobile. **Always allow … in this chat** stops asking for
that tool until the chat closes.

- The agent follows `AGENTS.md`, `CLAUDE.md` and the other agents' rule files, and uses the MCP servers from your
  other agents' configs (`.mcp.json`, `~/.claude.json`, Cursor).
- Long tool output (over 30,000 characters) is saved to a file next to the chat; the agent reads it in parts.
- The same tool call a 4th time in a row is refused, so a looping model tries something else.
- A `git` command runs without asking only when it just reads the repository, with that command's usual options
  written out in full (`--verbose`, not `--verb`). Anything else asks, and so does an option that could make git
  start a program (`--ext-diff`, `--textconv`, `--output`, `describe --dirty`). Reading commands also don't run
  the programs a repository's config names: fsmonitor hooks, external diff tools, textconv and clean filters.
- When the context fills up, the conversation is summarized and the chat carries on (or type `/compact`).
- The agent can search and read your [notes](notes.md), and save one when you ask: the approval shows its text.
- **Rename** a chat: right-click it in the sidebar's chat list → **Rename…** (<kbd>Enter</kbd> saves,
  <kbd>Esc</kbd> cancels).
- Next to the token meter, **≈ $** estimates what the chat's tokens cost at the model's list price (from the
  provider's model list, or else OpenRouter's catalog). Hover it for the prices used.

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
