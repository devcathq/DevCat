# Memory

One memory for all your coding agents. Claude Code, Codex, Gemini, Cursor and the DevCat Agent read and save it
through DevCat's MCP server, and each new session starts with a short brief of it, so you can switch models
mid-task and the next one knows what the last one learned. Open it with the brain button at the top right.

## What's in it

- **Memories**: short notes an agent saves when you ask it to remember something, or when it learns something
  durable (a preference, a correction, a decision and why, where something lives). Each one is for *every
  project* (about you and how you work) or for one project or repository.
- **Handoffs**: an agent that stops before the task is done leaves a note (what was done, what's left, where);
  the next agent, whichever it is, sees it and can pick it up.

Memories are background for the agents, never instructions: your request and the repository's rules files
(CLAUDE.md, AGENTS.md…) win. Secrets (API keys, tokens, passwords in connection strings) are removed when a memory
is saved.

## Sync from Claude

Claude Code keeps its own memory per folder. **Sync from Claude** copies it into the shared memory (a folder's
memories go to its project, memories about you go to every project) and adds DevCat's MCP server to your agents.
With **Keep in sync** on, new and changed Claude memories are copied automatically. Claude's files aren't changed.

## Memories

Browse, search, edit and forget what the agents saved, or write a memory yourself (**New memory**).

## Other computers

Share the memory with your other computers, so what an agent learns on your laptop is known on your desktop and
on a dev server running `devcat serve`. Computers connect to each other directly, or through the relay chosen in
**DevCat → Mobile → Reach from other networks** when they're on different networks. Everything is end-to-end
encrypted, and there's no DevCat account or server.

### Pair two computers

1. On the first computer open **Memory → Other computers** and click **Pair a computer: show a code**. Copy the
   code (`devcat://memory-sync?…`).
2. On the second computer open **Memory → Other computers → Paste a code from another computer**, paste it and
   click **Pair**. On a server, type `sync join <code>` in [devcat serve](devcat-serve.md).
3. The first computer asks *Share your agents' memory with this computer?*: click **Allow**.

A code works once, for 10 minutes, and only lets the other computer *ask*: nothing is shared until you allow it,
and you need DevCat open to answer. Pair each computer once; a third one can pair with either of the first two.

### How it syncs

- While both computers are online, a change on one reaches the other within seconds. A computer that was off
  catches up when it's back (they check every minute). **Sync now** syncs straight away.
- **Every project** memories and handoffs go to every paired computer. A project's memories go to the computers
  that have the same project, matched by its git remote (`github.com/acme/shop`, whatever the folder or user name
  on each computer), or by the project's name when it has no remote. A repository in no project needs a remote to
  be matched. Memories of a project another computer doesn't have stay where they are.
- **Conflicts never lose a memory.** When both computers changed the same memory before syncing, the newer change
  wins and the other is kept next to it as *name (conflict from computer)*: read both, keep what you want and
  forget the other. A memory deleted on one computer is deleted on the other, unless it was changed there
  meanwhile; then the change is kept.
- **Secrets stay home.** Secrets are removed before a memory leaves a computer, and again when one arrives. Only
  memories and open handoffs are shared: not your connections, passwords, notes or other files.
- The list shows each computer with its last sync; **Activity** lists pairings, syncs that changed something,
  and failures. Removing a computer stops sharing with it; what it already has stays there.

This is for your own computers: every paired computer can read and change all of the memory. Sharing memory with
other people (a team, with roles) is planned.
