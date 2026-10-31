# What agents can do

The tools Claude Code, Codex, Cursor, Gemini CLI and the DevCat Agent get in DevCat, and the rules DevCat holds them to.

Agents connected through [Sync MCP](sync-mcp.md), and the DevCat Agent in the AI workspace, can do most of what
you do in DevCat: use your databases, clusters, Docker engines and RabbitMQ brokers, connect your VPNs and tunnels,
set up new connections and VPN profiles, schedule DevCat Agent jobs and read your ticket boards. Passwords stay in
DevCat: an agent never sees one.

For a step-by-step example, see the [walkthrough](agent-walkthrough.md).

## The tools

An agent only gets the tools it can use in its folder: no Docker tools without a Docker connection, no ticket tools
without a ticket board, and so on. When that changes (the agent adds your first Docker connection, say), DevCat tells
the agent, and the new tools appear in the same session.

### Connections and setup

| Tool | What it does |
| --- | --- |
| `devcat_connections` | Lists the connections the folder sees: kind, database, the VPN it goes through, read-only, what agents may do |
| `devcat_connection_add` | Adds a database, Redis, MongoDB, Kubernetes, Docker or RabbitMQ connection (DevCat connects first and saves it only if that works) |
| `devcat_connection_test` | Checks that DevCat can reach a server: an existing connection, or settings you'd add |

### Databases, Redis, MongoDB

| Tool | What it does |
| --- | --- |
| `db_schema` | Tables and columns, or one table's columns, indexes and foreign keys |
| `db_query` | Runs SQL, MongoDB shell or Redis commands and returns the rows |
| `db_ddl` | The CREATE statement of a table, view, function, procedure, trigger or sequence |
| `db_export` | Writes a table or a query's rows to a file (to Downloads unless you say where): CSV, TSV, JSON, JSON lines, SQL inserts or Excel |
| `db_dump` | Dumps a database to a `.sql` file (a folder of JSON lines for MongoDB) |
| `db_import` | Adds the rows of a CSV or JSON file to a table |
| `db_restore` | Runs a dump against a database. DevCat Agent only, after you type the database's name |
| `db_drop_database` | Drops a database. DevCat Agent only, after you type its name |
| `db_history` | The queries you ran in DevCat, or your saved queries |
| `db_saved_query_save` | Saves a query in your **Saved queries** |

### Kubernetes

| Tool | What it does |
| --- | --- |
| `k8s_get` | Lists objects with their status, or one object as JSON (Secret values hidden) |
| `k8s_logs` | A pod's recent logs, or its previous run's |
| `k8s_events` | One object's events (why a pod is Pending or restarting) |
| `k8s_overview` | The cluster at a glance, and pod or node CPU and memory from metrics-server |
| `k8s_helm` | Helm releases, or one release's values (secret-looking keys hidden), notes and history |
| `k8s_action` | Scale, restart, suspend or resume |
| `k8s_apply` | Applies YAML, like `kubectl apply` |
| `k8s_delete` | Deletes an object. DevCat Agent only, after you type its name |

### Docker

| Tool | What it does |
| --- | --- |
| `docker_list` | Containers (with Compose project and service), stats, images, volumes, networks, or the engine's info |
| `docker_inspect` | `docker inspect` of a container, image, volume or network (secret-looking environment variables hidden) |
| `docker_logs` | A container's recent logs |
| `docker_action` | Start, stop, restart, pause, unpause or kill a container |
| `docker_remove` | Removes a container, image, volume or network. DevCat Agent only, after you type its name |

### RabbitMQ

| Tool | What it does |
| --- | --- |
| `rmq_overview` | Version, message totals, rates, and each node's memory, disk and alarms |
| `rmq_list` | Queues, exchanges, bindings, connections, channels, consumers, vhosts or policies (filtered, sorted, paged) |
| `rmq_peek` | Looks at a queue's first messages; they go back on the queue |
| `rmq_publish` | Publishes a message |
| `rmq_manage` | Declares queues and exchanges, binds and unbinds, closes connections |
| `rmq_delete` | Deletes or purges a queue, deletes an exchange. DevCat Agent only, after you type its name |

### VPNs and tunnels

| Tool | What it does |
| --- | --- |
| `net_status` | Your OpenVPN and WireGuard VPNs, SSH tunnels and Kubernetes port-forwards: connected or not, address, local ports, last error, which connections go through each |
| `net_logs` | A profile's recent log, to see why it didn't connect |
| `net_connect` | Connects a profile and waits until it's up |
| `net_disconnect` | Disconnects one |
| `net_profile_save` | Adds a profile (from a `.ovpn` or `.conf` file, or an SSH tunnel or port-forward) or changes one, and can connect it |
| `net_profile_delete` | Deletes one. DevCat Agent only, after you type its name |

An agent rarely needs `net_connect`: when it uses a connection that goes through a VPN or tunnel, DevCat connects
that first, as it does when you open the connection.

### Jobs, tickets, notes and memory

| Tool | What it does |
| --- | --- |
| `agent_jobs` | The folder's [scheduled DevCat Agent jobs](ai-workspace.md), with their last runs |
| `agent_job_save` | Schedules a job (every N minutes, or daily at a time), changes or deletes one |
| `agent_job_run` | Runs a job now |
| `tickets_list` | Your [ticket boards](tickets.md) and the tickets in a column, in full |
| `tickets_fix` | Hands tickets to the board's own agent run, like the **Fix** button |
| `devcat_notes`, `devcat_note_write` | Search, read and write your [notes](notes.md) |
| `memory_*`, `handoff_*` | The [shared memory](memory.md) |

## What agents may do

DevCat enforces these rules itself, whatever the agent asks for.

| What | The rule |
| --- | --- |
| A connection | Its **AI / MCP access** (Edit connection → **Advanced**): **Off** hides it, **Read-only** lets agents read, **Full** also lets them change things over MCP. A **Read-only connection** never changes. See [Sync MCP](sync-mcp.md#ai-mcp-access) |
| Changes through a connection | `db_query` writes, `db_import`, `k8s_action`, `k8s_apply`, `docker_action`, `rmq_publish` and `rmq_manage` need **Full** over MCP |
| What can't be undone | Dropping or restoring a database, emptying a table before an import, deleting a Kubernetes object, a container, image, volume, network, queue, exchange or VPN profile, purging a queue: the DevCat Agent only, and DevCat asks you to type the name first, whatever the permission mode. Never over MCP, never from a phone |
| Exports and dumps | Need only **Read-only** (they read the database and write a file on this computer) |
| A VPN or tunnel | Its own **AI / MCP access** (edit the profile in **VPN & SSH tunnels**): **Off** hides it, **Read & connect** (the default) shows its status and lets agents connect it, **Full** also lets them disconnect and edit it |
| A connection an agent adds | Starts at **Read-only** access. Agents can't add SSH sessions, or edit or delete connections |
| A VPN config an agent writes | May not run scripts or programs (OpenVPN `up`, `down`, `plugin`, `script-security`…; WireGuard `PostUp`, `PreDown`…), whichever way it connects later |
| Scheduled jobs | Only in the agent's own folder (or below). A job an agent makes asks you before commands and edits; only you can make one that doesn't (**Full access** mode) |
| Ticket boards and notes | Follow the folder rule of connections: a project with code folders keeps its boards and notes to agents working there |
| Secrets in what agents read | Kubernetes Secret values, secret-looking Docker environment variables and Helm values are hidden; RabbitMQ users and permissions aren't listed |

The DevCat Agent asks you before each change (in **Ask first** mode), so for it only **Off** makes a difference.
The agent working on a ticket gets the connection tools, but none that reach ticket boards, schedule jobs, add
connections or change VPN profiles: a ticket's text comes from the tracker, and it can't make the agent change your
DevCat.

## Where the calls run

`devcat mcp` is started by each agent, but your VPNs, tunnels, scheduled jobs and ticket runs live in the DevCat
that's open. So:

- **DevCat open** (the app, or [`devcat serve`](devcat-serve.md)): `devcat mcp` hands every call to it. VPNs connect
  there and work for every program on the computer.
- **DevCat closed**: `devcat mcp` runs the call itself. Databases, clusters, Docker and RabbitMQ still work when
  they're reachable, but it never starts a VPN: the tools that need DevCat (VPNs, jobs, tickets) say so.
- Scheduled jobs live in the app, so they need the app open (`devcat serve` doesn't run them).

The two talk over `127.0.0.1` with a random port and token that only your account can read
(`agent-bridge.json` in DevCat's data folder). Every call is logged in **MCP activity** either way.

## When something is refused

The agent gets the reason, and usually what you can change:

- *"DevCat only lets agents read … over MCP"*: set the connection's **AI / MCP access** to **Full**.
- *"Agents over MCP may see and connect … but not disconnect or edit it"*: set the profile's access to **Full**.
- *"… from the DevCat Agent in DevCat only, where the user confirms it"*: it can't be undone. Ask the DevCat Agent in
  DevCat, and type the name when it asks.
- *"DevCat isn't open"*: open DevCat (or run `devcat serve`) and ask again.
- *"This config uses `up`…"*: the config runs a script. Import the file yourself in **VPN & SSH tunnels** if you
  trust it.
