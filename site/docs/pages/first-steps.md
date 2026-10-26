# First steps

The parts of the window, projects, tabs and windows: what you need before using any one feature.

## The window

- **Left bar**: one icon per area. From top to bottom: *AI Workspace*, *Connections*, *History & saved queries*,
  *VPN & SSH tunnels*, *Git repositories* and *Notes*.
- **Sidebar**: the list for the selected area (your connections, your repositories, your notes…).
- **Tabs**: everything you open (a query, a table, a chat, a cluster) opens in a tab on the right.
- **Title bar**, top right: **Appearance** (palette), **Mobile** (phone), **Keep awake** (coffee cup) and **New
  window**.
- **Status bar** at the bottom: open connections and the version (click it to check for updates).

To hide the sidebar, click the active icon in the left bar again, use the toggle at the bottom of the left bar, or
press <kbd>⌘B</kbd> / <kbd>Ctrl+B</kbd>.

## Projects

A project groups the connections, chats, repositories and notes of one piece of work.

1. In *Connections*, click **New project** (or right-click empty space → *New project…*).
2. Give it a name, for example *Shop backend*.
3. Add connections to it: create them inside the project, or right-click a connection → *Move to project*.

Right-click a project → **Focus on this project** to show only that project everywhere. **Exit focus mode** shows
everything again. The project menu at the top left switches between projects.

In focus, tabs that belong to another project are hidden (they come back when you leave focus): its connections and
notes, and chats, terminals, files and Git tabs of folders that aren't the project's. A folder becomes the
project's when you use it while the project is focused, as the AI Workspace folder or by opening a chat, terminal,
file or Git tab in it (also one that's already open, hidden). Folders on an SSH session go by that session's
project. Your home folder counts only for itself, not for the folders in it. The AI Workspace's recent folders list the project's own; the
others are under **Other folders**, and **Remove from this project** in the same menu takes the open folder out.

## Tabs

- Drag a tab along the tab bar to reorder it.
- Every **New chat** opens a chat of its own.
- Right-click a tab → **Move to new window** to give it a window of its own.

## Windows

<kbd>⇧⌘N</kbd> / <kbd>Ctrl+Shift+N</kbd> opens another window. Each window has its own tabs and its own project
focus, so you can keep one project per screen. Right-click a project → *Open in new window* opens it straight in
a window of its own.

## Open files with DevCat

- **macOS:** DevCat appears under *Open With* in Finder for code and text files. Choose *Always Open With* (or *Get
  Info → Open with → Change All…*) to make it the default.
- **Windows and Linux:** pick DevCat under *Open with → Choose another app*.

The file opens in an editor tab, shown relative to its Git repository. OpenVPN `.ovpn` files open the VPN import
instead ([VPN and tunnels](vpn-tunnels.md)).
