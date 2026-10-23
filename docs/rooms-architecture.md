# Pairing and Rooms on iroh

Status: **built** (`crates/rooms`, `src-tauri/src/rooms.rs`, `rooms_agent.rs`, `src/features/rooms`).
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
files        map   path → { kind: text | blob, created by/at, blob: { hash, size, mime } }
file:<path>  text  the content of each text file (a root container: two people creating the
                   same file at once share it instead of overwriting each other)
thread       list  { id, author (person or "Arif's agent"), text, at, depth, reply_to }
claims       map   id → { path, first/last-character anchors, holder, expires }   (see §5.3)
suggestions  map   id → { path, anchors, base text, replacement, author, state }
history      list  applied changes { before, after, anchors, by, approved by } (for revert)
peers        map   Loro peer → member (who typed what)
```

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
  | `room_files`, `room_read`, `room_search`, `room_thread` | Read |
  | `room_claim` / `room_release` | Lease a range |
  | `room_suggest` | Propose a change (**default**) |
  | `room_edit` | Edit directly, only inside a held claim, only if the room allows it |

  Its final answer is posted to the thread as a reply; its claims are released when it finishes.
- **No shell, no local files, no connections, no web, no phone access.** **Built:** rather than a new
  approval policy, the room agent's tool registry simply contains nothing else: the other tools don't
  exist for it (a test drives it with a scripted model asking for `shell`).
- **Prompt injection:** documents and messages from others are untrusted input. With only room
  tools, the worst an injected instruction can do is bad text, which history can undo.

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

## 9. Phases (all built)

| # | Delivers | Proves |
|---|---|---|
| 0 | Port mapping, relay server URL in links, longer relay timeouts, direct/relay indicator, `docs/relay.md` (running a relay: hosting one is up to you) | How often connections go direct; pairing speed |
| 1 | Rooms endpoint routed by ALPN; `devcat-rooms`; create, invite, approve, signed member list | Two DevCats on different networks meet in a room |
| 2 | RoomDoc, shared text files, gossip + catch-up sync | Two people edit live; offline edits merge |
| 3 | Thread; agents read and comment on @mention | Agents discussing a document |
| 4 | Minimal edits, claims, suggestions, out-of-date check, attribution, undo | Agents editing safely together |
| 5 | Blobs, presence, removal with a new key, blind keeper | Everyday use |

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
