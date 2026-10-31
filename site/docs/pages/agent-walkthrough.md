# Walkthrough: let an agent set up and use DevCat

From a company VPN file to a working database, cluster and queue, done by Claude Code and checked in DevCat.

This walkthrough connects Claude Code to DevCat, then asks it in plain words to set up a VPN, add a database behind
it, look into a cluster, a container and a queue, and schedule a daily check. Codex, Cursor, Gemini CLI and the
DevCat Agent work the same way. The [tool reference](agent-tools.md) lists everything they can do.

You need DevCat open, Claude Code installed, and for the examples: an OpenVPN `.ovpn` file, a database behind that
VPN, and (optionally) a Kubernetes, Docker or RabbitMQ connection.

## 1. Connect Claude Code to DevCat

1. Click the plug button above the connections (**Sync MCP**).
2. Leave **Claude Code** ticked and click **Sync**.
3. Start a new Claude Code session in your project folder (in DevCat's AI workspace, or a terminal).

Check that it worked: ask

> What can you do with DevCat here?

Claude Code lists DevCat's tools for this folder. If it doesn't know DevCat, run `/mcp` in Claude Code: `devcat`
should be **connected**.

> **Tip:** Give your project its code folder first (project menu → **Code folders & MCP…**), so its connections,
> VPNs and notes go to agents working there and not elsewhere.

## 2. Set up the VPN

Put the `.ovpn` file somewhere the agent can see it (the project folder, or `~/Downloads`), then ask:

> Set up the office VPN from ~/Downloads/office.ovpn in DevCat and connect it. My VPN user is akbar.

Claude Code calls `net_profile_save` with the file, your user and `connect: true`:

- DevCat reads the file, inlines its certificate and key files, and saves the profile in this folder's project
  (password and passphrase go to your keychain).
- It connects. Your system may show the usual admin prompt (UAC on Windows); approve it.
- Claude Code reports the VPN address, for example *Office VPN is connected: connected since 09:12 · 10.8.0.6*.

If the config runs a script (`up`, `down`, `plugin`…), DevCat refuses it from an agent. Import that file yourself in
**VPN & SSH tunnels** if you trust it.

If it doesn't connect, ask *"Why didn't the VPN connect?"*: the agent reads `net_logs` and tells you what
OpenVPN said (wrong password, unreachable server, a missing tool).

## 3. Add a database behind the VPN

> Add the staging Postgres from our .env to DevCat, through the office VPN, read-only.

Claude Code reads your `.env`, then calls `devcat_connection_add` with the host, port, user, password and database,
`network: "Office VPN"` and `read_only: true`. DevCat connects first, through the VPN, and saves it only if that
works. The connection appears in DevCat's sidebar right away.

Connections an agent adds start at **Read-only** AI access. To let agents change data, edit the connection →
**Advanced** → **AI / MCP access** → **Full** (a connection marked read-only never can).

## 4. Use it

> Which tables in staging have no primary key? And how many orders came in today?

Claude Code reads the schema (`db_schema`) and runs a `SELECT … LIMIT` (`db_query`). If the VPN isn't connected
when the agent first uses the connection, DevCat connects it first, as it does when you open the connection
yourself.

More things to try:

- *"Export today's orders to orders.csv"*: `db_export` writes the file without the rows going through the model.
- *"Back up the staging schema to backups/staging.sql"*: `db_backup`.
- *"What did I run against staging yesterday?"*: `db_history`.
- *"Save that query as Daily orders"*: `db_saved_query_save`; it's in **Saved queries**.

## 5. Clusters, containers and queues

With a Kubernetes connection:

> Why is the api deployment in prod not ready?

The agent lists the pods (`k8s_get`), reads the failing pod's events (`k8s_events`) and logs (`k8s_logs`), and
tells you, for example, that the image tag doesn't exist. Asking it to *"restart the api deployment"*
(`k8s_action`) needs that connection's access at **Full**; otherwise DevCat refuses and the agent says how to allow
it.

With a Docker connection:

> Which containers are unhealthy, and what does the worker log say?

`docker_list`, `docker_inspect` (secret-looking environment variables hidden) and `docker_logs`.

With a RabbitMQ connection:

> Which queues are backing up? Show me a message from the biggest one.

`rmq_list` sorted by messages, then `rmq_peek`: the messages go back on the queue.

## 6. Schedule a daily check

> Every weekday at 08:30, check staging for failed jobs and summarize them.

Claude Code calls `agent_job_save` with `daily_at: "08:30"`, `days: [1,2,3,4,5]` and the prompt. It's a DevCat Agent
job in this folder, in **Ask first** mode: it asks you (in the window, or on DevCat Mobile) before running a command
or editing a file. It's listed under **Scheduled** in the DevCat Agent's chat list for the folder; ask *"Run the
staging check now"* to run it right away (`agent_job_run`). Jobs run while the DevCat app is open.

## 7. See what the agent did

Open **Sync MCP** → **Activity…** (or **MCP activity…** in the connections menu). Every call is there: the agent,
the connection, what it ran, whether it worked and how long it took, refused ones included.

## Turning things off

| To | Do |
| --- | --- |
| Hide a connection from agents | Edit connection → **Advanced** → **AI / MCP access** → **Off** |
| Keep agents from connecting a VPN | Edit the profile → **AI / MCP access** → **Off** |
| Let agents disconnect, edit or delete a VPN | Edit the profile → **AI / MCP access** → **Full** |
| Stop Claude Code using DevCat | Remove the `devcat` MCP server from Claude Code's config (`~/.claude.json`) |
