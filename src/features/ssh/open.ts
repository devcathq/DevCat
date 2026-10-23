// Saved SSH sessions (Xshell-style; desktop only, see CLAUDE.md): a session's folders are
// `ssh://devcat-<connection id>/path`, which the backend resolves to the saved host, user, key,
// jump host and password (src-tauri/src/claude/remote.rs).
import type { ConnectionConfig } from "../../lib/types";
import { remoteUri, sessionIdOf } from "../../lib/remotePath";
import { openTab } from "../../store/tabs";
import { useConnections } from "../../store/connections";
import { openClaudeWorkspace } from "../claude/open";

/** A folder of the session (default: its start folder, else home). */
export const sessionUri = (c: ConnectionConfig, dir?: string | null) =>
  remoteUri(`devcat-${c.id}`, null, dir || c.options.startDir || "~");

/** A new terminal tab logged in to the session (each call opens another, like Xshell). */
export function openSshTerminal(c: ConnectionConfig) {
  return openTab({ type: "terminal", cwd: sessionUri(c), connId: c.id }, c.name);
}

/** SFTP tab for a folder on an SSH host (a session's, or any ssh:// folder). */
export function openSftp(cwd: string) {
  const id = sessionIdOf(cwd);
  const c = id ? useConnections.getState().byId(id) : undefined;
  return openTab({ type: "sftp", cwd, connId: c?.id }, `SFTP · ${c?.name ?? cwd.replace(/^ssh:\/\/([^/]+).*/, "$1")}`);
}

/** The AI workspace (Claude Code…) running on the host, in the session's start folder. */
export function openSshAgent(c: ConnectionConfig) {
  return openClaudeWorkspace(sessionUri(c), { title: c.name });
}
