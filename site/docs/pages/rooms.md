# Rooms

Share documents with other people, talk about them, and let each person's DevCat Agent join the discussion
and propose changes. Everyone keeps working from their own computer and network: DevCats connect to each
other directly, or through a relay when the networks don't allow it, and everything is end-to-end encrypted.
There's no account and no server holding your documents.

## Make a room and invite someone

1. Click **Rooms** in the title bar (the two-people icon), then **New room**. Give it a name and say what
   others should call you.
2. In the room, click **People & invites**, choose **Editor** or **Reads only**, and click
   **Make an invite link**. Send the link to one person (chat, email…).
3. They open **Rooms → Join with an invite**, paste it and click **Ask to join**.
4. You see *Someone wants to join a room*: click **Let in** (or **Refuse**).

An invite works once, for 24 hours, and only lets its holder *ask*: nobody gets in without you letting them in,
and you need DevCat open to do it. The link doesn't contain the room's key.

## Work together

- **Files**: make one with the **+** icon, or share one from your computer (**↑**). Text files become
  shared documents everyone edits at the same time; other files (images, PDFs…) are shared as they are:
  click one to preview it or save it to your computer.
- **Editing**: type as usual. Others' changes appear as they make them, and edits made at the same time
  (even offline) merge without overwriting each other. **Cmd/Ctrl+Z** undoes only *your* changes.
- **Who's here**: the circles at the top are the people in the room right now; a green ring means they're in
  the same file. **People** shows what each is looking at, and whether you're connected to them directly or
  through the relay.
- **Thread**: talk about the documents on the right.

## Agents in a room

Each person's DevCat Agent can join in. Mention it in the thread: `@arif-agent` for Arif's agent, `@agents` for
every agent in the room (the chips under the message box insert them). The mentioned agent reads the room's
files and the thread, and answers in the thread.

- **It suggests, you decide.** An agent proposes changes in **Suggestions**: the text it wants to replace and
  the replacement. **Accept** applies it, **Reject** drops it. If someone changed that text meanwhile, the
  suggestion is *out of date* and can't be applied; ask the agent to propose it again.
- **It claims before changing.** An agent marks the part it's working on (highlighted in the editor), so two
  agents don't rewrite the same paragraph at once. Claims end when the agent finishes, or after 5 minutes.
- **Direct edits** (optional): the room's owner can let agents edit directly inside the part they claimed
  (**Room settings → Agents may edit directly…**). Otherwise they only suggest.
- **History** lists every accepted suggestion and agent edit; **Revert** undoes one, even later, as long as
  nobody changed that text since.

Your agent runs on your computer, with your AI connection (a *DevCat Agent* connection, see
*AI connections*), so mentioning it costs you, not the person who mentioned it. In
**Room settings** you choose who may wake it (anyone in the room, only you, or nobody) and which connection it
uses. Agents answering agents stop after three rounds, and an agent answers at most 20 times an hour per room.

In a room, agents only have room tools: they can't run commands, read your files, use your database
connections or browse the web, whatever someone writes in a document or the thread.

## Members

The owner (who made the room) can change someone between **Editor** and **Reads only**, or remove them. A
removed member keeps what they already had, but gets nothing new: the room switches to a new key.

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
