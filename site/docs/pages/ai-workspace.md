# AI workspace

Run your coding agent in a real terminal next to the project's sessions, files, diff and extra terminals.

![A Claude Code session in the AI workspace](../img/ai.png)

DevCat doesn't ship its own model. It runs the agent CLI you already have installed (Claude Code by default),
with your own login, in a real terminal like iTerm2 or Windows Terminal. Everything the CLI can do works as usual.

## Start a chat

1. Click **AI Workspace** in the left bar (the top icon).
2. Click **Open folder…** and pick your project's folder. Recent folders and projects with Claude history are
   listed in the folder menu at the top of the sidebar.
3. Click **New chat**. The agent starts in that folder in a new tab.
4. Type to the agent as you would in any terminal.

> **Note:** If Claude Code isn't installed, DevCat shows the install command with a copy button. Run it once in a
> terminal and log in, then click **Check again**.

## Sessions

The **Sessions** list in the sidebar shows your earlier Claude Code conversations for the open folder, newest
first. Click one to resume it in a tab. **Refresh sessions** reloads the list.

To **rename** a session, right-click it → **Rename…**, type the name and press <kbd>Enter</kbd> (<kbd>Esc</kbd>
cancels). DevCat saves it the way Claude Code's `/rename` does, so `claude --resume` shows the new name too,
and a Claude Code that has the session open keeps it instead of writing its old name back. Clear the name to go
back to the title Claude gave it.

## Launch options (Claude Code)

The toolbar above the chat has two menus:

- **Model**: the model Claude starts with. You can still switch with `/model` during the session.
- **Permission mode**: for example *Default mode* or *Bypass permissions*.

After changing either, click **Restart claude**: Claude starts again with the new options and resumes this
tab's conversation. A chat you haven't written in yet starts fresh; it never picks up another tab's
conversation in the same folder.

## Views in a chat tab

| View | What it shows | Shortcut |
| --- | --- | --- |
| **Chat** | The agent's terminal | <kbd>⌥⇧⌘1</kbd> |
| **Diff** | What changed in the folder, ready to stage and commit | <kbd>⌥⇧⌘2</kbd> |
| **Code Review** | A read-only review of the changes by Claude | <kbd>⌥⇧⌘3</kbd> |

On Windows and Linux use <kbd>Ctrl+Alt+Shift</kbd> instead of <kbd>⌥⇧⌘</kbd>. See [Review and commit](review-and-commit.md).

## Extra terminals

Click **Terminal** (or press <kbd>⌥⇧⌘T</kbd>) to open a shell in the same folder, in its own tab next to the chat.
Use it for test runs, servers or anything you don't want the agent to touch.

## Working with the terminal

- **Copy and paste:** <kbd>⌘C</kbd> / <kbd>⌘V</kbd> on macOS, <kbd>Ctrl+Shift+C</kbd> / <kbd>Ctrl+Shift+V</kbd>
  on Windows and Linux.
- **New line in the prompt:** <kbd>Shift+Enter</kbd>.
- **Links and files:** web links and file paths like `src/app.ts:12` are clickable. Files open in a preview tab.
  A file name with spaces (`exports/Akun Simulasi 26 Okt.xlsx`) is clickable too when Claude Code shows it as code
  or a tool prints it in quotes, and the file exists on this computer.
- **Font size:** <kbd>⌘=</kbd> / <kbd>⌘-</kbd> / <kbd>⌘0</kbd> (<kbd>Ctrl</kbd> on Windows and Linux). The font
  and line height are in [Appearance](appearance.md).
- **After the agent exits**, press <kbd>Enter</kbd> to start it again.

## Other agents

Pick another agent (Codex, Gemini CLI, Copilot, Aider, opencode or your own command) in the menu at the top of the
sidebar. See [AI connections](ai-connections.md).
