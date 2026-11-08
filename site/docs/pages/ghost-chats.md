# Ghost chats

A DevCat Agent chat that starts from nothing and keeps nothing once you close it.

Click the ghost icon in the top bar (or in an AI workspace tab's header) for a window with only a
[DevCat Agent](ai-connections.md#devcat-agent) chat. Use it for a quick question, a throwaway experiment, or anything
you don't want in your projects' history.

![A ghost chat in its own window](../img/ghost.png)

## Starts from nothing

A ghost chat has no project. It works in an empty scratch folder of its own and doesn't get:

- rule files (`AGENTS.md`, `CLAUDE.md`) or skills
- MCP servers or hooks
- your DevCat connections
- the shared [memory](memory.md), [notes](notes.md) or [rooms](rooms.md)

## Keeps nothing

- Closing the window deletes the chat and its scratch folder. So does quitting DevCat, and if DevCat crashes,
  they're deleted the next time it starts.
- It isn't listed in Sessions, on your phone, or in an [agent bot](agent-bots.md)'s chat.
- What you type isn't kept for <kbd>↑</kbd>.

> **Note:** "Keeps nothing" is about your computer. The model's provider still receives the conversation, as it
> does for any chat.
