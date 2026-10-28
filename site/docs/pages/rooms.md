# Rooms

Share files, ideas and work with other people: documents everyone edits at once, folders, whiteboards, a thread
with decisions, tasks, and a team memory every member's agent reads. Each person's DevCat Agent can join the
discussion and propose changes, and you can let your own coding agents (Claude Code, Codex, a DevCat Agent chat)
work in the room too. Everyone keeps working from their own computer and network: DevCats connect to each other
directly, or through a relay when the networks don't allow it, and everything is end-to-end encrypted. There's no
account and no server holding your documents.

## Make a room and invite someone

1. Click **Rooms** in the left bar (the two-people icon), then **+** (**New room**). Give it a name and say what
   others should call you.
2. In the room, click **People & invites**, choose **Editor** or **Reads only**, and click
   **Make an invite link**. Send the link to one person (chat, email…).
3. They click **Rooms** in the left bar → **Join with an invite** (the door icon), paste it and click **Ask to join**.
4. You see *Someone wants to join a room*: click **Let in** (or **Refuse**).

An invite works once, for 24 hours, and only lets its holder *ask*: nobody gets in without you letting them in,
and you need DevCat open to do it. The link doesn't contain the room's key, but it does contain the room's name and
your computer's network addresses: send it only to the person you invite.

## Files and folders

The room's files are a tree on the left. Folders are just the `/` in a name (`spec/proposal.md`).

- **Add files**: **+** makes a new file or whiteboard; the share button (**↑**) shares files or a whole folder from
  your computer, or a note, memory, skill or repository's changes from DevCat (see below). You can also drag files
  and folders onto the room, from your computer or from DevCat's Files tree: drop them on a folder in the tree to
  put them there. A shared folder leaves out `.git`, `node_modules` and similar (500 files and 200 MB at most).
- **Right-click** a file or folder for what you can do with it: rename or move it, make a file or whiteboard in a
  folder, save it to your computer, add a skill to your skills, save a file to your memory or copy it to your notes,
  or delete it. You can also drag a file or folder onto another folder to move it.
- **Text files** become shared documents everyone edits at the same time; other files (images, PDFs…) are shared as
  they are: click one to preview it or save it to your computer. A name that's taken gets a number
  (`notes (2).md`). Text too large to edit together (over 200,000 characters, like a long log) is shared read-only:
  everyone reads it in the room, and agents read it in parts. A shared file is 25 MB at most.
- **Markdown** files show the editor and a live preview side by side (**Edit**, **Both**, **Preview** above it).
  The fields at the top of a memory or skill file show as a small table.
- **Editing**: type as usual. Others' changes appear as they make them, and edits made at the same time (even
  offline) merge without overwriting each other. **Cmd/Ctrl+Z** undoes only *your* changes.
- **Who's here**: the circles at the top are the people in the room right now; a green ring means they're in the
  same file. **People** shows what each is looking at, and whether you're connected to them directly or through
  the relay.

A file with suggestions waiting, or one an agent is editing, can't be moved until they're settled.

## Whiteboards

**+** → **New whiteboard** (or right-click a folder → **New whiteboard here**) makes a board for ideas: shapes,
arrows, sticky notes, text and freehand drawing (Excalidraw). Everyone draws on it at once. Agents read the text
on a board, so you can ask one to turn a brainstorm into a plan. **Save to this computer** saves a board as an
`.excalidraw` file.

## Thread, decisions and mentions

Talk about the work on the right, in **Thread**.

- **Reply** to a message, or put a **reaction** on it (👍 ❤️ 🎉 😄 🤔 👀 ✅), from the buttons that show on it.
- **Pin a message as a decision** (the pin button): it shows under **Decisions** at the top of the thread, so the
  group's choices don't get lost in the conversation. Agents see which messages are decisions.
- **Mention a person** with `@name` (`@arif`), or everyone with `@everyone`: they get a notification in DevCat (and
  the taskbar flashes when DevCat isn't in front), and the Rooms button and the room show how many messages named
  them. Mention an *agent* with `@name-agent` (below). The chips under the message box insert them.
- Drop files on the thread, or paste a screenshot into the message box, to share them and name them in your
  message. A file's name in a message is a link that opens it.

## Tasks

**Tasks** (next to Thread) lists the work: add a task, give it to someone or to someone's agent, and tick it when
it's done. Giving a task to someone says so in the thread, naming them, so they're told; giving it to an agent
wakes that agent (as a mention does), and it takes the task, works on it and marks it done.

## Share from DevCat, and take things out

Share what you already have in DevCat with the room (the share button → **From DevCat**):

| What | Goes to | |
| --- | --- | --- |
| A note | `notes/` | also **Share with a room** in the note's toolbar |
| A memory | `memory/`, the room's team memory | also the share button on a memory in **Memory** |
| A skill (a folder with a `SKILL.md`) | `skills/` | from `~/.claude/skills`, `~/.agents/skills`, `~/.codex/skills` or a project's code folders |
| A repository's changes | `changes/`, as a `.diff` | for the others, and their agents, to review |

Take things out of a room by right-clicking them. Nothing is written on your computer until you've looked at it and
confirmed:

- **Add to my skills** (a skill folder, or its `SKILL.md`): pick where it goes (`~/.claude/skills` for Claude Code
  and the DevCat Agent in every project, `~/.codex/skills`, or a project's folder). A skill is instructions your
  coding agents follow with their access to your computer: read it before adding it, and only add skills from
  people you trust.
- **Save to my memory** (a Markdown file): check its name, description and kind, and whether it's for every project
  or one; your coding agents read it from then on.
- **Copy to my notes** (a text file): into the focused project's notes, or General.
- **Save to this computer**: a file, a whiteboard, or a whole folder.

## Team memory

The room's `memory/` folder is its team memory: what the group decided to remember, like conventions, decisions and
why, and who does what. Each entry is a Markdown file with the same fields Claude Code's memory uses (`name`,
`description`, `type`). Every member's agent reads the list before answering and opens the entries that matter.
Agents propose new entries (`room_remember`) as suggestions, so a wrong memory doesn't spread to everyone's agents
until a person accepts it. Share one of your own memories into it from **Memory**, or save one of the room's to
yours (above).

## Agents in a room

Each person's DevCat Agent can join in. Mention it in the thread: `@arif-agent` for Arif's agent, `@agents` for
every agent in the room. The mentioned agent reads the room's files, whiteboards, team memory, open tasks and the
thread, and answers in the thread. It also looks at the room's images (PNG, JPEG, GIF, WebP, up to 5 MB) when it
reads them, if its model can see images (most current ones do).

- **It suggests, you decide.** An agent proposes changes in **Suggestions**: the text it wants to replace and
  the replacement. **Accept** applies it, **Reject** drops it. If someone changed that text meanwhile, the
  suggestion is *out of date* and can't be applied; ask the agent to propose it again.
- **It claims before changing.** An agent marks the part it's working on (highlighted in the editor), so two
  agents don't rewrite the same paragraph at once. Claims end when the agent finishes, or after 5 minutes.
- **Direct edits** (optional): the room's owner can let agents edit directly inside the part they claimed
  (**Room settings → Agents may edit directly…**). Otherwise they only suggest.
- **Tasks and memory**: agents create and update tasks, take the ones given to them, and propose team memory.
- **History** lists every accepted suggestion and agent edit; **Revert** undoes one, even later, as long as
  nobody changed that text since.

Your agent runs on your computer, with your AI connection (a *DevCat Agent* connection, see
*AI connections*), so mentioning it costs you, not the person who mentioned it. In
**Room settings** you choose who may wake it (anyone in the room, only you, or nobody) and which connection it
uses. Agents answering agents stop after three rounds, and an agent answers at most 20 times an hour per room.

In a room, agents only have room tools: they can't run commands, read your files, use your database
connections or browse the web, whatever someone writes in a document or the thread.

## Your coding agents in a room

You can also work on a room from Claude Code, Codex, Cursor or a DevCat Agent chat: "read the spec in our room and
build the login page", "post a summary of what I changed to the room", "mark my task done". Turn it on per room, in
**Room settings → Let my coding agents use this room**. Only you choose this, only for you, and the others don't
see it.

- Claude Code, Codex and the others get the room tools through DevCat's MCP server: run **Sync MCP** once
  (Connections → the plug button, see [Sync MCP](sync-mcp.md)), and keep DevCat open while they work. A DevCat
  Agent chat started after you turned it on has them too.
- They act as *your agent*, shown as "Akbar's agent (Claude Code)", with the room agent's limits: `rooms_list`,
  `room_read`, `room_files`, `room_search`, `room_thread` and `room_tasks` read; `room_post` writes in the thread;
  `room_suggest`, `room_claim`, `room_task` and `room_remember` work like the room agent's; `room_edit` only where the
  owner lets agents edit. In a DevCat Agent chat, every write asks you first.
- There's no tool that copies files from your computer into a room.
- **Only in rooms with people you trust.** What people write in a room reaches an agent that can run commands on
  your computer. Agents are told that room content is information, not instructions, but that's not a guarantee.

## Who shared and edited it

Every file shows who shared or made it, and from where, above it ("shared by Arif from this computer"). Click
**Who edited it** (or right-click a file → **Who shared and edited it…**) to see:

- **Shared, moved, taken out**: who shared it in (from their computer, a note, a memory, a skill…), who moved or
  renamed it, and who took it out: saved it to their computer, added it to their skills, saved it to their memory,
  copied it to their notes or the clipboard.
- **Edited by**: each person's (or agent's) stretches of editing, with when and how much they added and removed,
  following the file across renames. An agent's edits and accepted suggestions show as that agent ("Arif's agent
  (Claude Code)").

**History → Who shared what** lists the same for the whole room, newest first.

All of it is in the room's own document, which every member's DevCat keeps: there's no server keeping a log. Who
edited comes from the document's history itself (each change is made by a member's DevCat, and the room knows
whose); sharing and taking out are recorded by members' DevCats when they do it, so a DevCat changed not to record
them could skip it.

## Members

The owner (who made the room) can change someone between **Editor** and **Reads only**, or remove them. People
who read only can still write in the thread and react.

**When you remove someone, their copy goes.** The room switches to a new key, so they get nothing new, and their
DevCat deletes its copy of the room: the files, whiteboards, thread, tasks, team memory and keys. It happens at
once if they're online, otherwise the next time their DevCat reaches anyone in the room (or its keeper). They see
*You were removed from…*. Their DevCat only does it on proof: the member list you signed, without them, so nobody
else in the room can make it delete a copy.

What this can't take back: what they already saved outside the room (to their computer, notes, memory or skills),
copied, or kept with a DevCat changed not to delete it. Share with people you trust.

**Room settings → Leave on this DevCat** deletes your copy; the others keep the room.

## When nobody else is online: a keeper

Changes travel when the people who made them and the people who need them are online at the same time. If your
group rarely overlaps, run a **keeper** on a computer that is always on (a small server, a spare machine):

```sh
devcat keeper
```

It prints its id. The room's owner pastes it in **Room settings → Add a keeper**. From then on, members leave
their changes with the keeper and pick up what they missed from it. The keeper stores them encrypted with the
room's key, which it never has: it can't read anything. `devcat keeper --owner <your id>` keeps only your
rooms (your id is at the bottom of **Room settings**).

## Connections and relays

Rooms use the same relay setting as DevCat Mobile (*Mobile → Reach from other networks*); a change applies the
next time DevCat starts. For the
fastest connections between networks, run your own relay close to the people in your rooms; see
`docs/relay.md` in the repository.

Everyone in a room should be on the same DevCat version: an older one can't open files in folders, and doesn't
show whiteboards, tasks, decisions or reactions.
