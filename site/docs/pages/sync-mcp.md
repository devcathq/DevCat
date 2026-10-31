# Sync MCP

Let Claude Code, Codex, Cursor and Gemini CLI use your DevCat connections, with the access you choose for each one.

DevCat has its own MCP server, `devcat mcp`. Agents connected to it can list your connections, read schemas, run
queries (SQL, MongoDB, Redis), work with Kubernetes, Docker and RabbitMQ, connect and set up your VPNs and tunnels,
add connections, and schedule DevCat Agent jobs: see [What agents can do](agent-tools.md) for every tool, and the
[walkthrough](agent-walkthrough.md) for an example. Passwords stay in DevCat: the agent never sees a password or a
connection string.

## Set it up

1. Click the plug button above the connections (or **Sync MCP…** in a project's menu).
2. DevCat lists the agents it found and what it will change in each one's config. Only DevCat's own `devcat`
   entry is added or replaced; other servers and settings are left alone, and each file is backed up once
   (`<file>.devcat-bak`).
3. Optionally tick other MCP servers that reach one of your databases, to have DevCat replace them.
4. Click **Sync**, then start a new agent session.

After the first sync, DevCat keeps the agents' configs up to date as your projects change, and repairs the entry
when DevCat moves (after an update, say). Connections are read on every call, so a new connection reaches the
agents right away, without syncing again.

| Agent | Config DevCat edits |
| --- | --- |
| Claude Code | `~/.claude.json` (one entry for every folder, plus one per project code folder) |
| Codex | `~/.codex/config.toml` |
| Cursor | `~/.cursor/mcp.json` |
| Gemini CLI | `~/.gemini/settings.json` |

## Which connections an agent sees

Give a project its **code folders** (project menu → **Code folders & MCP…**). Its connections then go only to
agents working in those folders, the DevCat Agent included. Connections in no project, or in a project without
folders, go to every folder.

## AI / MCP access

Each connection has an **AI / MCP access** setting (Edit connection → **Advanced**):

| Access | What agents can do |
| --- | --- |
| **Off** | Nothing: the connection is hidden from every agent, the DevCat Agent included |
| **Read-only** | Read schemas and run queries that only read. A query that may change data is refused |
| **Full** | Also change data over MCP: INSERT, UPDATE, DELETE, DDL, Redis writes |

- New connections start at **Read-only**.
- A connection marked **Read-only connection** can never be **Full**: DevCat refuses writes over MCP, and its
  driver refuses them as well.
- DevCat enforces the setting itself, whatever the agent asks for.
- Connections saved before this setting existed follow their project's **Allow writes from MCP** (Code folders &
  MCP…) until you pick an access for them.

The DevCat Agent in the AI workspace asks you before each change, so for it only **Off** makes a difference.

VPNs and tunnels have the same setting (edit the profile in **VPN & SSH tunnels**): **Off** hides the profile,
**Read & connect** (the default) lets agents see its status and connect it, **Full** also lets them disconnect, edit
and delete it. See [VPN and tunnels](vpn-tunnels.md#ai-mcp-access).

## DevCat open or closed

With DevCat open (the app, or `devcat serve`), `devcat mcp` hands each call to it: VPNs, tunnels, scheduled jobs
and ticket runs live there, and a VPN DevCat connects works for every program on the computer. With DevCat closed,
`devcat mcp` runs the call itself and never starts a VPN; the tools that need DevCat say so. See
[Where the calls run](agent-tools.md#where-the-calls-run).

## Resources

Besides tools, the server offers MCP *resources* that an agent can attach without a tool call:

- `devcat://project/<project>/connections`: the connections this folder sees, with their access.
- `devcat://conn/<id>/schema`: a database's tables and columns (DevCat connects when the agent reads it).

## Activity

Every tool call agents make through `devcat mcp` is logged: the agent, the connection, what it ran, whether it
worked (refused writes included) and how long it took. Open it with **Activity…** in the Sync MCP dialog, or
**MCP activity…** in the connections menu. The log stays on this computer (`mcp-audit.jsonl` in DevCat's data
folder); **Clear** empties it.

## Run it by hand

`devcat mcp` speaks MCP over stdio. Options:

- `--project auto|none|<id>`: the project whose code folder the agent works in (`auto`, the default), only the
  connections every folder gets (`none`), or one project.
- `--folder <path>`: the folder for `auto`.
- `--data-dir <path>`: DevCat's data folder, if not the app's.

Sync MCP edits files on this computer, so it isn't available from DevCat Mobile.
