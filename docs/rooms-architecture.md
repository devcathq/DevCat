# Pairing and Rooms on iroh (proposal)

Status: **draft for discussion**. Nothing below is built yet except where it says "exists".

DevCat already uses iroh to let a phone reach its own computer (`src-tauri/src/remote`, `mobile/`).
This proposal keeps that ("pairing") and adds **rooms**: people on their own networks share
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
│ Data       Loro CRDT docs · iroh-blobs (files, images)                         │
├────────────────────────────────────────────────────────────────────────────────┤
│ Transport  ONE iroh Endpoint per DevCat, routed by ALPN:                       │
│            devcat/remote/1 (exists) · devcat/room/1 · iroh-gossip · iroh-blobs │
├────────────────────────────────────────────────────────────────────────────────┤
│ Connectivity  direct (hole-punch + port mapping) → DevCat relays → n0 (last)   │
│ Finding peers invite link · relay server URL · DNS lookup (n0 or our own)      │
└────────────────────────────────────────────────────────────────────────────────┘
```

`iroh-gossip` (0.101) and `iroh-blobs` (0.103) depend on iroh `^1`, the same major version DevCat
uses (1.3).

## 2. Connection layer (shared by pairing and rooms)

**One endpoint per DevCat.** Its key is already the computer's identity. Incoming connections are
routed by ALPN, so pairing and rooms share one UDP port and one hole-punching session; any other
ALPN is refused.

**Order of paths:**
1. Same network, direct.
2. Internet, direct: hole-punching, plus the router opening the port (UPnP / NAT-PMP / PCP; the
   `portmapper` feature).
3. **DevCat relays:** `iroh-relay` on 2–3 small servers (Singapore/Jakarta first, then EU, US).
   They forward encrypted packets only; iroh keeps trying to upgrade to direct.
4. n0's public relays, last resort. A setting turns them off for self-hosters.

**Finding peers:** every pairing link and invite carries the peer's current relay server URL and
addresses (`home` in `crates/remote-proto`); iroh's DNS lookup is the fallback. Our own
`iroh-dns-server` later, if we want no n0 dependency.

**Visibility:** each connection shows `Direct · 35 ms` or `Relay (sgp) · 140 ms`, for paired phones
and room members.

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
| Membership log (signed changes) | Members | Who's in, roles (owner / editor / viewer), current epoch |

### 4.3 Invite and join (approved like pairing)

```
Owner: Invite ─► link/QR: room_id · invite token (single-use, 24 h) · owner id + relay URL + addrs
                 (no room key: a leaked link alone isn't enough)
Guest opens it ─► connects to the owner over devcat/room/1, shows the token
Owner's DevCat ─► "Akbar wants to join Q4 spec as editor"  [Allow] [Deny]
Allowed        ─► room_key + membership + current docs, over that encrypted connection
               ─► signed "add Akbar" in the membership log; every member learns it
```

- Members accept room connections only from endpoints in the membership log.
- Removing a member starts a new epoch with a new `room_key`, sent only to remaining members. The
  removed member keeps what they already had, and gets nothing new.

### 4.4 Data

Loro (Rust CRDT) per room; Automerge is the alternative.

```
RoomDoc
├─ files:   tree  path → { kind: text | blob, doc_id | blob_hash }
├─ thread:  list  { id, author (person or "Arif's agent"), text, refs (file + range), time }
├─ claims:  map   range → { holder, agent, expires }      (leases, see §5.3)
├─ suggestions: map id → { author, file, range anchors, base version, change, state }
└─ meta:    title, settings
FileDoc (one per text file): LoroText; every change tagged with its author
```

- **Binary files and images:** iroh-blobs; the CRDT holds the hash. Fetched in pieces from any
  member that has them, like a torrent.
- **On disk:** `<app data>/rooms/<room_id>/`, encrypted snapshot plus update log.
- **As files:** a read-only copy at `~/DevCat Rooms/<room>/` for normal tools. Edits go through
  DevCat's editor and room tools; picking up external editors' changes is a later step.

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

- **When an agent acts:** only when @mentioned or assigned a task in the thread, never on every
  message. Agent-to-agent mentions are depth-limited and rate-limited per room, so agents can't
  loop. Each owner pays only for their own agent.
- **Room tools** (new in `crates/harness`):

  | Tool | Effect |
  |---|---|
  | `room_read`, `room_search` | Read |
  | `room_comment` | Post to the thread |
  | `room_claim` / `room_release` | Lease a range |
  | `room_suggest` | Propose a change (**default**) |
  | `room_edit` | Edit directly, only inside a held claim, if the room allows it |

- **No shell, no local files, no connections, no phone access.** Web fetch asks. A new `Room`
  policy is checked before approvals and "Always allow", the same way plan mode is.
- **Prompt injection:** documents and messages from others are untrusted input. With only room
  tools, the worst an injected instruction can do is bad text, which history can undo.

## 7. Availability: the blind keeper (optional)

A headless `devcat keeper` (server, or anyone's always-on machine) joins rooms as a member **without
the room key**: it stores and forwards encrypted changes so members can sync when nobody else is
online. It can't read anything.

## 8. Security summary

| Threat | Defence |
|---|---|
| A non-member finds the room | Random 32-byte id; gossip topic derived from the secret; no room list |
| An invite link leaks | Single-use, expiring token; owner approves each join; no key in the link |
| A relay or keeper reads data | Room-key encryption on top of iroh's end-to-end encryption |
| A removed member keeps reading | New epoch and key for remaining members only |
| An injected instruction in a shared doc | Room tools only; suggestions by default; undo |
| A room member reaches someone's computer | Separate ALPNs; room code never reaches `dispatch.rs` or the PTY |

## 9. Phases

| # | Delivers | Proves |
|---|---|---|
| 0 | Port mapping, relay server URL in links, longer relay timeouts (branch `feat/remote-direct-paths`); direct/relay indicator; one DevCat relay (Singapore) | How often connections go direct; pairing speed |
| 1 | One shared endpoint routed by ALPN; `devcat-rooms`; create, invite, approve, membership log | Two DevCats on different networks meet in a room |
| 2 | RoomDoc, one shared Markdown file, gossip + catch-up sync | Two people edit live; offline edits merge |
| 3 | Thread; agents read and comment on @mention | Agents discussing a document |
| 4 | Minimal edits, claims, suggestions, out-of-date check, attribution, undo | Agents editing safely together |
| 5 | Blobs, presence, removal with a new key, blind keeper | Everyday use |

## 10. Open decisions

1. CRDT: **Loro** (recommended) or Automerge.
2. Who approves joins: owner only, or any admin.
3. Agent edits by default: **suggestions** (recommended), or direct edits inside claims.
4. Relays: who hosts them, and in which regions.
