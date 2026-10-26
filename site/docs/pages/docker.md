# Docker

See your containers, grouped by Compose project, with their CPU and memory; follow logs, open a shell, start,
stop and remove them, and clean up images, volumes and networks.

## Add a Docker engine

1. In *Connections*, click **New connection** and pick **Docker**.
2. Leave **Context** on *current context* to use the engine `docker` uses on this computer (Docker Desktop,
   OrbStack, Colima, Rancher Desktop…), or pick another context from `docker context ls`.
3. Or fill in **Host** to reach another engine directly: `ssh://user@server` (uses your SSH key, there's no
   password prompt), `tcp://host:2376` or `unix:///var/run/docker.sock`. The host wins over the context.
4. Click **Test connection** to check it, then save. Double-click the connection (or right-click → **Open**)
   to open the engine in a tab.

DevCat runs the `docker` command line, so Docker (or at least its CLI) must be installed, and the engine running.
If it isn't, the tab says so; start Docker and click **Retry**.

## Find your way around

The top bar shows the engine: its Docker version, CPUs, memory and how many containers run. On the left:

- **Containers**: grouped by Compose project, then the ones started on their own. Each row has the service
  (or the container's name), image, status, ports, and CPU and memory for running ones. Click a project to fold
  it, and tick **Running only** to hide stopped containers.
- **Images**, **Volumes** and **Networks**.

**Filter** at the top narrows the list (by name, image, project or service). The lists refresh every 5 seconds;
change it with **Refresh**, or turn it off.

## Act on a container

Hover a row for its buttons, or right-click it (or click **⋯**):

| Action | What it does |
| --- | --- |
| **Logs** | Follow the logs, from the last 500 lines (double-clicking a row does it too). **Find in logs**, **Times** for timestamps, **Follow** to stay at the bottom, and copy. After a restart, they carry on |
| **Shell** | Open a terminal tab with a shell inside the container (`bash`, else `sh`) |
| **Inspect** | Its `docker inspect` JSON, which you can copy |
| **Start**, **Stop**, **Restart** | What they say |
| **Pause** / **Resume** | Freeze the container's processes, and unfreeze them |
| **Kill** | Stop it at once (SIGKILL), after asking |
| **Remove…** | Remove it, after asking. A running one is stopped first. Its volumes stay |

Images, volumes and networks have **Inspect** and **Remove**. Removing an image that has several tags removes
only that row's tag. An image a stopped container still uses can't be removed straight away: DevCat says why and
offers to force it. The `bridge`, `host` and `none` networks can't be removed.

A **read-only** connection only looks: logs and inspect work, everything that changes something is off, and so
is the shell.

Docker is desktop-only: DevCat Mobile and the browser version don't list Docker connections.
