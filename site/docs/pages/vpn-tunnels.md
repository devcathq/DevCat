# VPN and tunnels

Connect OpenVPN and WireGuard VPNs, SSH tunnels and Kubernetes port-forwards, and watch their traffic live.

![The network view with VPN profiles, a tunnel and its traffic graph](../img/network.png)

## Add a VPN or tunnel

Click **VPN & SSH tunnels** in the left bar, then **+** (**Add VPN or SSH tunnel**):

| Choice | Use it for |
| --- | --- |
| **Import file…** | An OpenVPN `.ovpn` or WireGuard `.conf` file you got from your company or provider |
| **Import from ~/.ssh/config…** | SSH hosts you already have, turned into tunnels |
| **New OpenVPN** / **New WireGuard** | Setting one up in a form instead of a file |
| **New SSH tunnel** | Local port forwards and a **SOCKS proxy** through an SSH server |
| **New Kubernetes port-forward** | A service or pod in a cluster, on a local port |

You can also double-click an `.ovpn` file in Finder or Explorer (or use *Open With → DevCat*) to import it.

## Connect

Click a profile and **Connect**. Its status, log (**Show logs**) and a live traffic graph appear on the right.

> **Note:** VPNs need administrator rights, so your system asks for your password (UAC on Windows) when one
> connects. See [Passwordless VPN](#passwordless-vpn-macos-and-linux) to skip that on macOS and Linux.

### Connect databases through a VPN or tunnel

In a database connection's form, set **Network** to a VPN, tunnel or port-forward. When you open that connection,
DevCat connects the network first. See [Database connections](connections.md).

## AI / MCP access

Agents (Claude Code, Codex… through [Sync MCP](sync-mcp.md), and the DevCat Agent) can see and connect your VPNs
and tunnels, and set up new ones from a file. Each profile's **AI / MCP access** (edit the profile) decides how far
they go:

| Access | What agents can do |
| --- | --- |
| **Off** | Nothing: the profile is hidden from them. A connection through it works for them only once you've connected it |
| **Read & connect** | See its status and log, and connect it (DevCat also does when a connection needs it). The default |
| **Full** | Also disconnect it and edit it |

Profiles an agent adds start at **Read & connect**, and agents never change a profile's access. Deleting a profile
can't be undone, so only the DevCat Agent can, after you type the profile's name. A config an agent
writes may not run scripts or programs (OpenVPN `up`, `down`, `plugin`, `script-security`…; WireGuard `PostUp`,
`PreDown`…). See [What agents can do](agent-tools.md).

## What you need installed

- **Windows:** nothing up front. DevCat includes the official OpenVPN and WireGuard installers and offers to install
  the missing one the first time you connect, with one UAC prompt.
- **macOS:** OpenVPN is built in. For WireGuard, install `wireguard-tools` (for example `brew install
  wireguard-tools`).
- **Linux:** install `openvpn` and / or `wireguard-tools` from your distribution.

## SSH tunnels

An SSH tunnel has an **SSH server** (host, user, **Password** or **Private key**) and one or more forwards:

- **Local port → remote host and port**, for example `localhost:5433 → db.internal:5432`. **Add forward** adds more.
- **SOCKS proxy**: a local port your browser or tools can use as a proxy into that network.

## Kubernetes port-forwards

Pick one of your Kubernetes connections, the **Namespace** and the **Service** (or pod), and the **Local port**.
It's switched on and off like a VPN.

Every client connection gets its own forward, so one closing can't drop the others. Services are looked up again for
new connections, so a database failover to a new pod is followed.

## Passwordless VPN (macOS and Linux)

To stop typing your password on every connect:

1. In the VPN list, click **Set up passwordless VPN…**.
2. Enter your password once.

DevCat installs a small root-owned helper and a sudoers rule for your account only. After that, VPNs connect
without asking. The helper keeps working across DevCat updates and only asks to be updated (**Update helper**) when
it changes. **Remove** takes it out again.

> **Warning:** For safety the helper refuses configs that run scripts or load plugins (OpenVPN `up`, `down`,
> `plugin`; WireGuard `PostUp`, `PreDown`…) and files that don't belong to you. Those still connect with the
> password prompt.
