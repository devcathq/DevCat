// Folders and files on SSH hosts are written `ssh://[user@]host[:port]/path` (see
// src-tauri/src/claude/remote.rs); everything else is a local path. A saved SSH session's are
// `ssh://devcat-<connection id>/path`: shown with the session's name.
import { useConnections } from "../store/connections";

export const isRemote = (p: string | null | undefined): p is string => !!p && p.startsWith("ssh://");

export interface RemoteParts { dest: string; port: number | null; path: string }

export function remoteParts(p: string): RemoteParts | null {
  if (!isRemote(p)) return null;
  const rest = p.slice("ssh://".length);
  const i = rest.indexOf("/");
  const authority = i < 0 ? rest : rest.slice(0, i);
  const path = i < 0 ? "/" : rest.slice(i);
  const m = /^(.*):(\d+)$/.exec(authority);
  return m ? { dest: m[1], port: Number(m[2]), path } : { dest: authority, port: null, path };
}

export function remoteUri(dest: string, port: number | null, path: string): string {
  const p = path.startsWith("/") ? path : path.startsWith("~") ? `/${path}` : `/${path}`;
  return `ssh://${dest}${port ? `:${port}` : ""}${p}`;
}

/** The SSH session (connection id) a `ssh://devcat-<id>/…` path belongs to. */
export function sessionIdOf(p: string | null | undefined): string | null {
  const r = p ? remoteParts(p) : null;
  return r?.dest.startsWith("devcat-") ? r.dest.slice("devcat-".length) : null;
}

/** Host label: `devbox` / `me@box:2222`, or the SSH session's name. */
export function remoteHost(p: string): string {
  const r = remoteParts(p);
  if (!r) return "";
  const sid = sessionIdOf(p);
  if (sid) return useConnections.getState().byId(sid)?.name ?? "SSH session";
  return `${r.dest}${r.port ? `:${r.port}` : ""}`;
}

/** For display: `devbox: ~/app` for remote folders, `~/app` for local ones. */
export function displayPath(p: string): string {
  const r = remoteParts(p);
  const tild = (x: string) => x.replace(/^\/(Users|home)\/[^/]+/, "~");
  return r ? `${remoteHost(p)}: ${tild(r.path)}` : tild(p);
}
