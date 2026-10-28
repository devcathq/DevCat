# Pairing and Rooms on iroh

Status: **built** (`crates/rooms`, `src-tauri/src/rooms.rs`, `rooms_agent.rs`, `rooms_tools.rs`, `src/features/rooms`); §12 lists designs not built yet.
User guide: `site/docs/pages/rooms.md`. Where the build differs from the first proposal, it says so
(**Built:**); known limits are listed at the end.

DevCat already uses iroh to let a phone reach its own computer (`src-tauri/src/remote`, `mobile/`).
This keeps that ("pairing") and adds **rooms**: people on their own networks share
documents, discuss them in a thread, and each person's DevCat Agent can join the discussion and
edit, without a central server holding the data.

## 0. Principles

1. **Peer-to-peer first.** Devices connect directly; a relay is only the fallback, and it is
   DevCat's own relay, not the public one.
2. **Two trusts, never mixed.**
   - **Pairing** = *my phone controls my computer*. Powerful, own devices only.
   - **Rooms** = *people share documents*. A room never reaches your terminal, files, connections
     or `remote/dispatch.rs`.
3. **No server holds readable data.** What's always online (relays, the optional keeper) only sees
   ciphertext.
4. **Agents run on their owner's computer** with the owner's model and API key, and get only room
   tools inside a room.

## 1. Layers

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ UI         Mobile tab (pairing)  │  Rooms sidebar · shared doc editor · thread │
├──────────────────────────────────┼───────────────────────────────────────────┤
│ Features   remote/ (exists)      │  devcat-rooms (new crate)                  │
│            pair · dispatch       │  membership · invites · sync · claims      │
│                                  │  agent bridge → harness (room tools)       │
├──────────────────────────────────┴───────────────────────────────────────────┤
│ Data       one Loro CRDT per room · sealed blobs (files, images)                │
├────────────────────────────────────────────────────────────────────────────────┤
│ Transport  pairing endpoint (exists): devcat/remote/1                          │
│            rooms endpoint (own key), routed by ALPN: devcat/room/1 · gossip    │
├────────────────────────────────────────────────────────────────────────────────┤
│ Connectivity  direct (hole-punch + port mapping) → DevCat relays → n0 (last)   │
│ Finding peers invite link · relay server URL · DNS lookup (n0 or our own)      │
└────────────────────────────────────────────────────────────────────────────────┘
```

`iroh-gossip` (0.101) depends on iroh `^1`, the same major version DevCat uses (1.3).

**Built:** rooms have **their own endpoint and key**, not the pairing one. People you share a room with
never learn the identity your phones pair with, and room code can't reach the pairing protocol; the cost
is a second UDP port. Behind it, one iroh `Router` serves `devcat/room/1` and gossip.

## 2. Connection layer (the same rules for pairing and rooms)

Pairing and rooms each have an endpoint (see above), both built the same way; each only accepts its
own ALPNs.

**Order of paths:**
1. Same network, direct.
2. Internet, direct: hole-punching, plus the router opening the port (UPnP / NAT-PMP / PCP; the
   `portmapper` feature).
3. **A relay**: n0's public relays by default, or your own `iroh-relay` (`docs/relay.md`), ideally
   close to your users. They forward encrypted packets only; iroh keeps trying to upgrade to direct.

**Finding peers:** every pairing link and invite carries the peer's current relay server URL and
addresses (`home` in `crates/remote-proto`); iroh's DNS lookup is the fallback. Our own
`iroh-dns-server` later, if we want no n0 dependency.

**Visibility:** each connection shows `direct · 35 ms` or `through relay … · 140 ms`, for paired
phones and room members.

## 3. Pairing (phone ↔ own computer)

Today's design, plus: relay server URL in the link, port mapping, longer relay timeouts, the
direct/relay indicator. A paired phone controls *your* computer; a room member never can. Rooms on
the phone come later and go through the user's own computer, within the limits in `CLAUDE.md`.

## 4. Rooms

### 4.1 Identity

- **v1:** a member is one DevCat install (endpoint key) plus a display name.
- **v2:** a person key signs that person's devices, so one person = one member.

### 4.2 What a room is

| Part | Held by | Purpose |
|---|---|---|
| `room_id` (32 random bytes) | Members | Names the room; can't be guessed |
| `room_key` (32-byte secret, per *epoch*) | Members only | Encrypts all room traffic and storage; the gossip topic is derived from it |
| Member list, signed by the owner (newest version wins) | Members | Who's in, roles (owner / editor / viewer / keeper), current epoch, room settings |

### 4.3 Invite and join (approved like pairing)

```
Owner: Invite ─► link/QR: room_id · invite token (single-use, 24 h) · owner id + relay URL + addrs
                 (no room key: a leaked link alone isn't enough)
Guest opens it ─► connects to the owner over devcat/room/1, shows the token
Owner's DevCat ─► "Akbar wants to join Q4 spec as editor"  [Allow] [Deny]
Allowed        ─► room keys + member list + the document, over that encrypted connection
               ─► the owner signs a new member list with Akbar in it; every member learns it
```

- Members only sync with, and hand keys to, endpoints in the newest signed member list.
- Removing a member starts a new epoch with a new `room_key`, sent only to remaining members. The
  removed member keeps what they already had, and gets nothing new.

### 4.4 Data

One Loro document per room (`crates/rooms/src/doc.rs`):

```
files        map   path → { kind: text | blob | board, created by/at, blob: { hash, size, mime } }
file:<path>  text  the content of each text file (a root container: two people creating the
                   same file at once share it instead of overwriting each other)
board:<path> map   a whiteboard's shapes: shape id → its Excalidraw JSON
thread       list  { id, author (person, "Arif's agent", "Arif's agent (Claude Code)"), text, at,
                   depth, reply_to }
claims       map   id → { path, first/last-character anchors, holder, expires }   (see §5.3)
suggestions  map   id → { path, anchors, base text, replacement, author, state }
history      list  applied changes { before, after, anchors, by, approved by } (for revert)
peers        map   Loro peer → member (who typed what)
tasks        map   id → { title, notes, assignee (member, or their agent), state, by, at }
pins         map   message id → { by, at }: the thread's decisions
reactions    map   "message|member|emoji" → { by, at }
```

- **Folders** are the `/` in a path. **Built:** Loro refuses `/` in a root container's *name* when it imports a
  change (a panic, in Rust and in the window), so a path inside a root name is written with `/` → `%2F` and
  `%` → `%25` (`doc.rs` `root_part`, `replica.ts` `rootPart`). Names without either kept their old root, so
  rooms from before folders are unchanged; text the first versions put under a raw `file:a/b` root is copied to
  its new name once, by the owner only (`move_old_roots`), so two members can't both copy it.
- **Rename and move** copy a file's text (or a board's shapes) to the new root and empty the old one; a file
  with open suggestions or a claim can't move (their anchors live in the old root).
- **Whiteboards** merge per shape: a shape changed in the window is written when its Excalidraw `version`
  goes up, and shapes from others are merged with Excalidraw's `reconcileElements` (the newer version wins, and a
  shape being drawn here isn't replaced mid-stroke). The backend reads a board's text for agents
  (`board_text`).
- **Tasks** are replaced whole on each change (the later change wins when two are made at once); pins and
  reactions are one entry each, so they never conflict.

- **Binary files and images:** sealed with the room key and kept by whoever shared them; the CRDT holds
  the hash. **Built:** fetched over `devcat/room/1` (`Blob`) from any member who has it, members only,
  instead of iroh-blobs: iroh-blobs serves any blob to whoever knows its hash, so it would have needed the
  same sealing plus its own access control.
- **On disk:** `<app data>/rooms/<room_id>/`: `room.json` (member list, keys, invites, your
  preferences), `doc.loro` (the document) and `blobs/`. Like the rest of DevCat's data, it is protected
  by your account, not encrypted again on disk.
- **Editing:** in DevCat's room tab and through the agent's room tools. A copy as plain files on disk,
  for other editors, is a later step.

### 4.5 Sync

| Situation | How |
|---|---|
| Live | Each change encrypted with the room key and broadcast with iroh-gossip (a mesh: each member connects to a few others) |
| Catching up | On connect, two peers exchange version vectors over `devcat/room/1` and send only what's missing |
| Presence, cursors | Short-lived gossip messages, never stored |
| Nobody else online | Optional blind keeper (§7) |

## 5. Concurrent edits: what can conflict, and what stops it

"Conflict" means two different things here.

### 5.1 Lost changes: prevented by the CRDT

Every insert and delete is its own operation with a unique ID. Changes made at the same time are
all kept, and every member ends with the same result whatever order they arrive in.

```
Original:        "The deadline is Friday."
Arif (offline):  "The deadline is Friday at 5pm."
Akbar (offline): "The final deadline is Friday."
Merged:          "The final deadline is Friday at 5pm."   (both kept, on every device)
```

The thread is an append-only list: messages sent at the same moment both appear, in the same order
everywhere.

### 5.2 Edits that clash in meaning: the CRDT can't stop these

A CRDT merges characters, not meaning. Two agents rewriting the same paragraph at once lose
nothing, but their rewrites interleave into nonsense:

```
Original:       "Ship v2 in March."
Arif's agent:   delete it, write "Ship v2 in April after QA."
Akbar's agent:  delete it, write "Delay v2; scope is unclear."
Merged:         "Ship v2 in April after QA.Delay v2; scope is unclear."
```

Worse: an agent that reads a file, rewrites it whole and writes it back ("delete everything, insert
the new text") **wipes out every edit others made in between**. That is the real overwrite risk, and
it comes from how agents write, not from the network.

### 5.3 The layers

| # | Layer | Stops |
|---|---|---|
| 1 | **CRDT** | Lost keystrokes |
| 2 | **Minimal edits only.** An agent's change is diffed against *the version it read*; only the changed spans are applied, located with Loro cursors (anchors that follow the text as others type). Room tools have no "write whole file" | An agent wiping out others' recent edits |
| 3 | **Claims (leases).** Before editing a range the agent claims it (about 5 min, renewable, expires if the agent dies). Others see "Akbar's agent is editing §3" and wait or suggest instead | Two agents rewriting the same paragraph |
| 4 | **Suggestions by default.** Agents propose; a person accepts (like Google Docs' suggesting mode) | Bad or conflicting changes landing unseen |
| 5 | **Out-of-date check.** A suggestion records its base version. If its range changed since, it's marked *out of date*; the agent re-reads and proposes again instead of applying | Applying a change to text that has moved |
| 6 | **History and undo per author.** Every change is tagged ("Arif's agent, approved by Arif") and can be undone, even later | Recovering when something slips through |
| 7 | **Thread first.** Agents agree in the thread ("I'll take §3") before claiming; the room's rules say claim before edit | Agents working at cross purposes |

**Claims that clash.** Two agents can claim the same range while briefly out of sync. Once the
claims sync, the earlier claim wins (ties broken by member id); the agent that lost turns its edit
into a suggestion. Nothing is lost.

**People typing.** People edit live without claims (normal collaborative editing). An agent's claim
shows a marker over the range; a person typing inside it is allowed, and makes the agent's pending
change out of date (layer 5).

## 6. Agents in a room

```
Arif's DevCat                          Akbar's DevCat
 └ Arif's agent (Arif's model, key)     └ Akbar's agent (Akbar's model, key)
        │ room tools only                        │
        └───────────── RoomDoc (CRDT) ◄──────────┘
```

- **When an agent acts:** only when mentioned in the thread (`@<first name>-agent`, or `@agents`),
  never on every message, and only if its owner allows it (anyone / only me / nobody). Replies chain
  at most 3 deep between agents, an agent runs at most 20 times an hour per room, and mentions older
  than 10 minutes (arriving after being offline) don't wake it. Each owner pays only for their own agent.
- **Room tools** (`src-tauri/src/rooms_agent.rs`, on the harness's `Tool` trait):

  | Tool | Effect |
  |---|---|
  | `room_files`, `room_read`, `room_search`, `room_thread`, `room_tasks` | Read (a board reads as its text; the thread marks decisions) |
  | `room_claim` / `room_release` | Lease a range |
  | `room_suggest` | Propose a change (**default**) |
  | `room_task` | Create a task, or update one (take it, mark it done, give it to someone) |
  | `room_remember` | Propose a team memory entry (`memory/<name>.md`): a suggestion a person accepts |
  | `room_edit` | Edit directly, only inside a held claim, only if the room allows it |

  Its final answer is posted to the thread as a reply; its claims are released when it finishes. Its turn
  starts with the room's files, its **team memory** (`memory/*.md`: name and description of each, from the
  frontmatter Claude Code's memory uses) and the open tasks.
- **No shell, no local files, no connections, no web, no phone access.** **Built:** rather than a new
  approval policy, the room agent's tool registry simply contains nothing else: the other tools don't
  exist for it (a test drives it with a scripted model asking for `shell`).
- **Prompt injection:** documents and messages from others are untrusted input. With only room
  tools, the worst an injected instruction can do is bad text, which history can undo.

### 6.1 The user's own coding agents (built)

Besides the room agent, a user can let *their own* coding agents work in a room: Claude Code, Codex, Cursor
(through `devcat mcp`) and DevCat Agent chats. It's a per-room choice of that user (`Prefs::coding_agents`,
kept in their `room.json`, never shared), off until they turn it on.

```
Claude Code ─stdio─► devcat mcp ──127.0.0.1 + token──► DevCat app ──► Room (as "Akbar's agent (Claude Code)")
DevCat Agent chat ─────────────────────────────────────► (in the app) ┘
```

- **The same tools** (`rooms_tools.rs` wraps `rooms_agent.rs`'s with a `room` input), plus `rooms_list` and
  `room_post`, with the same limits: editors' tools only for an editor, `room_edit` only where the owner allows
  agent edits. Messages are written at depth 1 (agents answering them stop after a few rounds).
- **Who acted:** `Author.via` ("Claude Code") shows in the room as "Akbar's agent (Claude Code)", and keeps its
  claims apart from Akbar's room agent's.
- **The bridge:** `devcat mcp` can't open rooms itself (one iroh key, one writer of the document), so the app
  listens on 127.0.0.1 with a fresh random token in `<data>/rooms/agents.json` (the user's own data folder), and
  `devcat mcp` forwards each call. With the app closed the tools say to open it. Tools are only listed when a
  room on disk allows coding agents.
- **No way to copy local files in.** Unlike the room agent, a coding agent has a shell: content from a room could
  steer it. So no tool uploads files from the computer to a room (sharing files stays a person's act in the
  window), writes ask in a DevCat Agent chat (`Kind::Fetch`, like a web fetch: the text leaves the computer),
  and `rooms_list` and the MCP instructions say room content is information, not instructions. The user is told
  to turn it on only in rooms with people they trust.

### 6.2 Between a room and the rest of DevCat (built)

- **In:** a note, a memory (into `memory/`), a skill folder (into `skills/`) or a repository's diff (into
  `changes/`), by a person, from the room or from Notes and Memory.
- **Out:** "Add to my skills" (a folder with `SKILL.md`, to `~/.claude/skills`, `~/.agents/skills`,
  `~/.codex/skills` or a project's `.claude/skills`), "Save to my memory", "Copy to my notes", "Save to this
  computer". Each shows what will be written and waits for the person; a skill comes with a warning that it
  instructs agents that can run commands.

## 7. Availability: the blind keeper (optional)

A headless `devcat keeper` (server, or anyone's always-on machine) joins rooms as a member **without
the room key**: it stores and forwards encrypted changes so members can sync when nobody else is
online. It can't read anything. **Built:** members push sealed batches of changes (`KeeperPush`) and
fetch what they missed (`KeeperFetch`, by index). The keeper answers only members of the newest member
list the owner signed, and `--owner <id>` limits it to chosen owners' rooms.

## 8. Security summary

| Threat | Defence |
|---|---|
| A non-member finds the room | Random 32-byte id; gossip topic derived from the secret; no room list |
| An invite link leaks | Single-use, expiring token; owner approves each join; no key in the link |
| A relay or keeper reads data | Room-key encryption on top of iroh's end-to-end encryption |
| A removed member keeps reading | New epoch and key for remaining members only |
| An injected instruction in a shared doc | Room tools only; suggestions by default; undo |
| A room member reaches someone's computer | Separate ALPNs; room code never reaches `dispatch.rs` or the PTY |
| Room content steers someone's coding agent (it has a shell) | Off per room until its user turns it on; no tool copies local files into a room; writes ask in a chat; agents told room content is information |
| A skill or memory from a room changes what someone's agents do | Nothing leaves a room on its own: the person reads it and confirms (a warning for skills); team memory goes through suggestions |
| Another program calls the app's room bridge | Loopback only; a random token per start, in the user's data folder |

## 9. Phases (all built)

| # | Delivers | Proves |
|---|---|---|
| 0 | Port mapping, relay server URL in links, longer relay timeouts, direct/relay indicator, `docs/relay.md` (running a relay: hosting one is up to you) | How often connections go direct; pairing speed |
| 1 | Rooms endpoint routed by ALPN; `devcat-rooms`; create, invite, approve, signed member list | Two DevCats on different networks meet in a room |
| 2 | RoomDoc, shared text files, gossip + catch-up sync | Two people edit live; offline edits merge |
| 3 | Thread; agents read and comment on @mention | Agents discussing a document |
| 4 | Minimal edits, claims, suggestions, out-of-date check, attribution, undo | Agents editing safely together |
| 5 | Blobs, presence, removal with a new key, blind keeper | Everyday use |
| 6 | Folders, rename/move, Markdown preview, whiteboards, tasks, decisions, reactions, person mentions, team memory, share in and out, coding agents | A group's daily work in one place |

## 10. Decisions taken

1. CRDT: **Loro** 1.16.2, the same release in the backend (Rust) and the window (`loro-crdt`, WASM).
   The editor types into the window's own copy; copies exchange updates, so concurrent edits merge
   whatever arrives first.
2. Joins: the **owner** approves each one.
3. Agents **suggest** by default; the owner can let them edit inside their claims.
4. Relays: the DevCat → Mobile setting (public by default; your own: `docs/relay.md`).

## 11. Known limits

- **Roles are enforced by honest clients.** A viewer's DevCat doesn't send document edits, and every
  DevCat refuses to send editor changes for a viewer, but the CRDT accepts any member's changes: a
  modified client could still edit. Likewise the author written on a message or suggestion isn't signed.
  Members are people the owner let in; signing changes is future work.
- **Removing someone hides what comes next, not what they had.** They keep their copy up to the
  removal.
- **The keeper's log only grows** (no compaction yet).
- **Presence and typing** are best-effort (gossip, every 15 s); catch-up sync is what guarantees
  copies converge.
- **Not on the phone** (see `CLAUDE.md`): rooms are desktop-only, like the SSH workbench.
- **One version for everyone.** A DevCat from before folders can't open a file in a folder (its window uses the
  raw root name), and ignores boards, tasks, decisions and reactions.
- **Tasks are last-change-wins** as a whole record: two changes to one task at the same moment keep only the later.
- **Whiteboards** keep deleted shapes (marked deleted) in the document, and don't share images yet (the image
  tool is off).
- **Mentions are in-app** (a toast, the taskbar, badges): there are no system notifications on the desktop yet.

## 12. Designs for later (not built)

These came up with the features above, but each needs its own design and review before code: they reach
further into someone's computer or identity.

### 12.1 Watching a teammate's agent session

Following a teammate's Claude Code or DevCat Agent session live, like Zed's follow mode.

- Opt-in per session by its owner ("Share this session with <room>"), read-only for the others, ending when the
  owner stops it or the room tab closes.
- Share the *chat* (messages, tool calls and their results), not the terminal: terminals show secrets (tokens in
  `env`, `.env` files). Tool results still can: show a warning and let the owner hide a result before it's sent.
- Carried as presence-like gossip (not stored in the document), with a size cap; nothing to replay later unless
  the owner posts a summary to the thread.
- No steering from others in the first version: suggestions go in the thread.

### 12.2 Signed changes and one identity per person

Today roles are enforced by honest clients and authors aren't signed (§11).

- A person key (in the keychain) signs that person's devices; the member list lists persons with their devices.
- Each change carries the device's signature over its Loro change hash; peers drop changes from non-members, and
  editors' changes from viewers, on import. Loro has no hook to reject one change inside an update, so this needs
  per-change export on the sending side and checking before `import`.
- Messages, suggestions, tasks and decisions show a verified author.

### 12.3 Rooms on the phone

Through the user's own computer, as the rest of DevCat Mobile, within the limits in `AGENTS.md`: reading the
thread, writing in it, reacting, tasks and decisions first; files read-only; no sharing of the phone's files
(they would land on the computer, see "Files land on the wrong machine" in `AGENTS.md`), no coding agents.

### 12.4 A room folder as a Git repository

Export a folder to a local repository and keep it updated (commit per accepted suggestion, author = who changed
it), or import a repository's files. The open questions are conflicts with edits made outside the room and large
repositories (the room's size limits).
