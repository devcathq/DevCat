// SSH session variants: a session's child in the sidebar. Every setting a variant leaves empty is
// its parent's (shown greyed in its dialog), read each time it connects; what it fills in is its
// own (another host, user, tmux session…). Port forwards are never inherited (their ports would
// clash). Desktop only, like every SSH session feature (AGENTS.md). The backend merges them the
// same way: remote.rs `effective_config`.
import type { ConnectionConfig } from "../../lib/types";
import { useConnections } from "../../store/connections";

/** The session this one is a variant of (its id), if any. */
export const parentIdOf = (c: ConnectionConfig): string | null => (c.kind === "ssh" && c.options?.parent) || null;

export const isVariant = (c: ConnectionConfig) => !!parentIdOf(c);

export function parentOf(c: ConnectionConfig): ConnectionConfig | undefined {
  const id = parentIdOf(c);
  return id ? useConnections.getState().byId(id) : undefined;
}

export const variantsOf = (id: string, list = useConnections.getState().connections) => list.filter((c) => parentIdOf(c) === id);

/** The session as it connects: the variant's own settings over its parent's (no secrets: the list has none). */
export function effectiveConfig(c: ConnectionConfig, parent = parentOf(c)): ConnectionConfig {
  if (!parent || !isVariant(c)) return c;
  const options = { ...parent.options };
  delete options.forwards;
  for (const [k, v] of Object.entries(c.options ?? {})) if (v.trim()) options[k] = v;
  return {
    ...parent,
    id: c.id,
    name: c.name,
    host: c.host.trim() || parent.host,
    port: c.port || parent.port,
    user: c.user.trim() || parent.user,
    color: c.color ?? parent.color,
    options,
  };
}

/** "me@host" for tooltips. */
export function sessionWhere(c: ConnectionConfig): string {
  return c.user ? `${c.user}@${c.host}` : c.host;
}
