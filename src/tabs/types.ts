import type { NodeKind, TableRef } from "../lib/types";

/** Every tab kind the workspace can show. Add new kinds here + in registry.tsx. */
export type TabSpec =
  | { type: "welcome" }
  | { type: "query"; connId: string; database?: string | null; schema?: string | null; initialSql?: string; savedQueryId?: string }
  | { type: "table-data"; connId: string; table: TableRef }
  | { type: "table-design"; connId: string; table?: TableRef | null; database?: string | null; schema?: string | null }
  | { type: "ddl"; connId: string; table: TableRef; kind: NodeKind }
  | { type: "redis"; connId: string; database?: string | null; key?: string | null }
  | { type: "mongo-collection"; connId: string; table: TableRef }
  | { type: "er-diagram"; connId: string; database?: string | null; schema?: string | null }
  | { type: "schema-compare"; sourceConnId?: string; targetConnId?: string }
  | { type: "data-transfer"; connId?: string; database?: string | null; schema?: string | null }
  | { type: "history" }
  | { type: "k8s"; connId: string }
  | { type: "rabbitmq"; connId: string }
  | { type: "docker"; connId: string }
  /** Network profile (VPN / SSH tunnel) details; profileId "" = new (kind preselected). */
  | { type: "network"; profileId: string; kind?: "ssh" | "openvpn" | "wireguard" | "k8s"; importPath?: string }
  /** AI workspace for a folder: Chat / Diff / Code Review. providerId = AI connection the chat runs
   *  (absent = Claude Code); sessionId resumes a past Claude session. */
  | { type: "claude"; cwd: string; sessionId?: string | null; view?: "chat" | "diff" | "review"; providerId?: string | null }
  /** Preview of a file (code, markdown, pdf, image, spreadsheet, …). blame = show git blame instead. */
  | { type: "file"; path: string; root?: string | null; blame?: boolean }
  /** Git repository: history graph / working-tree changes / branches / stashes.
   *  path = file-history filter, sha = commit to select in History (consumed once shown). */
  /** A shell in a folder (local or ssh://…), on its own tab. connId = its SSH session. */
  /** `program` / `args`: run that instead of a shell (a container's shell: `docker exec -it`). */
  | { type: "terminal"; cwd: string; connId?: string; program?: string; args?: string[] }
  /** Files on an SSH host next to this computer's, copied both ways. connId = its SSH session. */
  | { type: "sftp"; cwd: string; connId?: string }
  /** Markdown note (see features/notes). */
  | { type: "note"; noteId: string }
  /** A room: documents, thread and agents shared with other people (see features/rooms). */
  | { type: "room"; roomId: string }
  /** Jira / Trello / Linear boards whose tickets the agent works on (see features/tickets). */
  | { type: "tickets" }
  | { type: "git"; repo: string; view?: GitView; path?: string | null; sha?: string | null };

export type GitView = "history" | "changes" | "branches" | "stashes";

export type Tab = TabSpec & {
  id: string;
  title: string;
  /** dirty indicator (unsaved edits) */
  dirty?: boolean;
};

/** The folder (or file) a tab works in, for project focus mode; null = none. */
export function tabFolder(t: TabSpec): string | null {
  switch (t.type) {
    case "claude":
    case "terminal":
    case "sftp":
      return t.cwd;
    case "file":
      return t.root ?? t.path;
    case "git":
      return t.repo;
    default:
      return null;
  }
}

/** Connection a tab belongs to (used by project focus mode); null = not tied to a connection. */
export function tabConnId(t: TabSpec): string | null {
  if ("connId" in t && t.connId) return t.connId;
  if (t.type === "schema-compare") return t.sourceConnId ?? t.targetConnId ?? null;
  return null;
}
