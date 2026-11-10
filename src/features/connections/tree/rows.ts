// Flattens connections + cached tree nodes into the visible row list rendered by the
// virtualized tree. Also derives database/schema context for every node, and adds the
// Bookmarks section and the "show only bookmarked" filter.
// Only dependency-free .ts imports, so rows.check.ts (`pnpm check:tree`) runs under plain Node.
import type { Bookmark, ConnectionConfig, NodeKind, Project, TableRef, TreeNode } from "../../../lib/types.ts";
import { INFRA_KINDS } from "../../../lib/types.ts";
import { nodeKey } from "./keys.ts";
import type { NodeState } from "./store";

/** Database / schema / folder a node lives in (inclusive of the node itself). */
export interface NodeCtx {
  database: string | null;
  schema: string | null;
  /** lowercase name of the nearest folder ("tables", "views", ...) */
  folder: string | null;
}

export const BOOKMARKS_ROW = "b:";

export type Row =
  | { type: "bookmarks"; id: typeof BOOKMARKS_ROW; count: number; collapsed: boolean; depth: 0; parentId: null }
  | { type: "bookmark"; id: string; bookmark: Bookmark; conn: ConnectionConfig; depth: 1; parentId: typeof BOOKMARKS_ROW }
  | { type: "project"; id: string; project: Project; count: number; collapsed: boolean; depth: 0; parentId: null }
  | { type: "conn"; id: string; conn: ConnectionConfig; depth: number; expanded: boolean; parentId: string | null }
  | { type: "node"; id: string; conn: ConnectionConfig; node: TreeNode; ctx: NodeCtx; parentPath: string[]; depth: number; expanded: boolean; loading: boolean; bookmarked: boolean; parentId: string }
  | { type: "error"; id: string; conn: ConnectionConfig; path: string[]; message: string; depth: number; parentId: string }
  | { type: "empty"; id: string; depth: number; parentId: string; text?: string };

export type SelectableRow = Extract<Row, { type: "bookmarks" | "bookmark" | "project" | "conn" | "node" }>;

export const isInfra = (c: ConnectionConfig) => INFRA_KINDS.includes(c.kind);
export const connRowId = (id: string) => `c:${id}`;
export const projectRowId = (id: string) => `p:${id}`;
export const nodeRowId = (connId: string, path: string[]) => `n:${nodeKey(connId, path)}`;
export const bookmarkRowId = (id: string) => `${BOOKMARKS_ROW}${id}`;

const lastSeg = (n: TreeNode) => n.path[n.path.length - 1] ?? n.label;

export function childCtx(ctx: NodeCtx, n: TreeNode): NodeCtx {
  switch (n.kind) {
    case "database": return { database: n.table?.database ?? lastSeg(n), schema: null, folder: null };
    case "schema": return { ...ctx, schema: n.table?.schema ?? lastSeg(n), folder: null };
    case "folder": return { ...ctx, folder: lastSeg(n).toLowerCase() };
    default: return ctx;
  }
}

/** TableRef for an openable node, synthesized from context when the backend omits it. */
export function refOf(n: TreeNode, ctx: NodeCtx): TableRef {
  return n.table ?? { database: ctx.database, schema: ctx.schema, name: n.label };
}

/** Redis key name for a key node. */
export const redisKeyOf = (n: TreeNode) => n.table?.name ?? lastSeg(n);

/** True if the node is a "Tables" folder (or a folder of a SQL kind that holds tables). */
export function isTablesFolder(n: TreeNode): boolean {
  return n.kind === "folder" && (lastSeg(n).toLowerCase() === "tables" || n.label.toLowerCase() === "tables");
}

const BOOKMARK_KINDS: NodeKind[] = [
  "database", "schema", "table", "view", "materializedView", "collection", "key",
  "function", "procedure", "trigger", "sequence", "event",
];

/** Databases, schemas and objects that open in a tab can be bookmarked. */
export const isBookmarkable = (n: TreeNode) => BOOKMARK_KINDS.includes(n.kind);

/** Secondary text of a bookmark row: "<connection> · <database>" (just the connection for databases). */
export function bookmarkHint(b: Bookmark, conn: ConnectionConfig): string {
  const db = b.kind === "database" ? null : b.table?.database;
  return db ? `${conn.name} · ${db}` : conn.name;
}

/** Rows that have a chevron. */
export function isExpandable(r: Row): boolean {
  switch (r.type) {
    case "project": case "bookmarks": return true;
    case "conn": return !isInfra(r.conn);
    case "node": return r.node.hasChildren;
    default: return false;
  }
}

export function isExpanded(r: Row): boolean {
  switch (r.type) {
    case "project": case "bookmarks": return !r.collapsed;
    case "conn": case "node": return r.expanded;
    default: return false;
  }
}

interface BuildArgs {
  connections: ConnectionConfig[];
  projects: Project[];
  nodes: Record<string, NodeState>;
  /** collapsed state keyed by project id */
  collapsedProjects: Record<string, boolean>;
  query: string;
  bookmarks: Bookmark[];
  bookmarksCollapsed: boolean;
  /** "Show only bookmarked": in connections that have bookmarks, hide everything else */
  bookmarksOnly: boolean;
}

export function buildRows({ connections, projects, nodes, collapsedProjects, query, bookmarks, bookmarksCollapsed, bookmarksOnly }: BuildArgs): Row[] {
  const q = query.trim().toLowerCase();
  const rows: Row[] = [];
  const matchMemo = new Map<string, boolean>();

  const marked = new Set(bookmarks.map((b) => nodeKey(b.connectionId, b.path)));
  // Filter: per connection with bookmarks, keep bookmarks and their ancestors; bookmarked
  // databases / schemas ("containers") keep their whole subtree.
  const filtered = new Set<string>();
  const keep = new Set<string>();
  const containers = new Set<string>();
  if (bookmarksOnly) {
    for (const b of bookmarks) {
      filtered.add(b.connectionId);
      for (let i = 1; i <= b.path.length; i++) keep.add(nodeKey(b.connectionId, b.path.slice(0, i)));
      if (b.kind === "database" || b.kind === "schema") containers.add(nodeKey(b.connectionId, b.path));
    }
  }
  /** `open`: the node's parent is (inside) a bookmarked container, so nothing is filtered. */
  const visible = (connId: string, n: TreeNode, open: boolean) => open || !filtered.has(connId) || keep.has(nodeKey(connId, n.path));
  const opens = (connId: string, n: TreeNode, open: boolean) => open || containers.has(nodeKey(connId, n.path));

  // Does any visible loaded descendant of (connId, path) match the query?
  const descMatches = (connId: string, path: string[], open: boolean): boolean => {
    const key = nodeKey(connId, path);
    const memo = matchMemo.get(key);
    if (memo !== undefined) return memo;
    matchMemo.set(key, false); // cycle guard
    const kids = nodes[key]?.children ?? [];
    const res = kids.some((k) => visible(connId, k, open) && (k.label.toLowerCase().includes(q) || descMatches(connId, k.path, opens(connId, k, open))));
    matchMemo.set(key, res);
    return res;
  };

  const emitChildren = (conn: ConnectionConfig, path: string[], ctx: NodeCtx, depth: number, parentId: string, filter: boolean, open: boolean) => {
    const st = nodes[nodeKey(conn.id, path)];
    if (st?.error) {
      rows.push({ type: "error", id: `e:${nodeKey(conn.id, path)}`, conn, path, message: st.error, depth, parentId });
      return;
    }
    const kids = st?.children;
    if (!kids) return;
    if (kids.length === 0 && !filter) {
      rows.push({ type: "empty", id: `x:${nodeKey(conn.id, path)}`, depth, parentId });
      return;
    }
    for (const n of kids) {
      if (!visible(conn.id, n, open)) continue;
      const kidOpen = opens(conn.id, n, open);
      const selfMatch = filter && n.label.toLowerCase().includes(q);
      const below = filter && !selfMatch && descMatches(conn.id, n.path, kidOpen);
      if (filter && !selfMatch && !below) continue;
      const key = nodeKey(conn.id, n.path);
      const ns = nodes[key];
      const nctx = childCtx(ctx, n);
      // While searching, ancestors of matches are shown expanded.
      const expanded = n.hasChildren && (below || !!ns?.expanded);
      const id = nodeRowId(conn.id, n.path);
      rows.push({ type: "node", id, conn, node: n, ctx: nctx, parentPath: path, depth, expanded, loading: !!ns?.loading, bookmarked: marked.has(key), parentId });
      if (expanded || (ns?.error && ns.expanded)) emitChildren(conn, n.path, nctx, depth + 1, id, below, kidOpen);
    }
  };

  const rootCtx: NodeCtx = { database: null, schema: null, folder: null };
  const byId = new Map(connections.map((c) => [c.id, c]));
  const byName = (a: ConnectionConfig, b: ConnectionConfig) => a.name.localeCompare(b.name);

  // SSH session variants sit under their session (features/ssh/variants.ts), wherever it is.
  const parentOf = (c: ConnectionConfig) => {
    const p = c.kind === "ssh" ? c.options?.parent : undefined;
    return p && byId.get(p)?.kind === "ssh" ? p : null;
  };
  const variants = new Map<string, ConnectionConfig[]>();
  for (const c of connections) {
    const p = parentOf(c);
    if (p) variants.set(p, [...(variants.get(p) ?? []), c]);
  }

  /** `all`: show it and its variants whatever the search (its project's name matched). */
  const emitConn = (conn: ConnectionConfig, depth: number, parentId: string | null, all = false) => {
    const nameMatch = all || !q || conn.name.toLowerCase().includes(q);
    const subs = (variants.get(conn.id) ?? []).sort(byName);
    const shownSubs = nameMatch ? subs : subs.filter((v) => v.name.toLowerCase().includes(q));
    const below = !!q && !nameMatch && !isInfra(conn) && descMatches(conn.id, [], false);
    if (!nameMatch && !below && !shownSubs.length) return;
    const root = nodes[nodeKey(conn.id, [])];
    const expanded = !isInfra(conn) && (below || !!root?.expanded);
    const id = connRowId(conn.id);
    rows.push({ type: "conn", id, conn, depth, expanded, parentId });
    if (expanded) emitChildren(conn, [], rootCtx, depth + 1, id, below, false);
    for (const v of shownSubs) rows.push({ type: "conn", id: connRowId(v.id), conn: v, depth: depth + 1, expanded: false, parentId: id });
  };

  // Bookmarks first, for connections that are visible (focus mode) and still exist.
  const shown: { bookmark: Bookmark; conn: ConnectionConfig }[] = [];
  for (const bookmark of bookmarks) {
    const conn = byId.get(bookmark.connectionId);
    if (conn && (!q || bookmark.label.toLowerCase().includes(q) || conn.name.toLowerCase().includes(q))) shown.push({ bookmark, conn });
  }
  if (shown.length) {
    const collapsed = !q && bookmarksCollapsed;
    rows.push({ type: "bookmarks", id: BOOKMARKS_ROW, count: shown.length, collapsed, depth: 0, parentId: null });
    if (!collapsed) {
      for (const { bookmark, conn } of shown) {
        rows.push({ type: "bookmark", id: bookmarkRowId(bookmark.id), bookmark, conn, depth: 1, parentId: BOOKMARKS_ROW });
      }
    }
  }

  const known = new Set(projects.map((p) => p.id));
  const members = new Map<string, ConnectionConfig[]>();
  const top = connections.filter((c) => !parentOf(c));
  for (const c of top) {
    if (c.projectId && known.has(c.projectId)) members.set(c.projectId, [...(members.get(c.projectId) ?? []), c]);
  }

  // Projects first (in user order, including empty ones), then connections without a project.
  for (const project of projects) {
    const list = (members.get(project.id) ?? []).sort(byName);
    const pid = projectRowId(project.id);
    const projectMatch = !q || project.name.toLowerCase().includes(q);
    const at = rows.length;
    // When searching, projects are always expanded so matches are visible.
    const collapsed = !q && !!collapsedProjects[project.id];
    rows.push({ type: "project", id: pid, project, count: list.length, collapsed, depth: 0, parentId: null });
    if (!collapsed) {
      if (list.length === 0) {
        if (!q) rows.push({ type: "empty", id: `x:${pid}`, depth: 1, parentId: pid, text: "Empty — drag a connection here or right-click to add one" });
      } else if (projectMatch && q) {
        // project name matched: show all members unfiltered by the search
        for (const c of list) emitConn(c, 1, pid, true);
      } else {
        for (const c of list) emitConn(c, 1, pid);
      }
    }
    // Drop the project header if nothing in it matched the search.
    if (q && rows.length === at + 1 && !projectMatch) rows.pop();
  }
  for (const c of top.filter((c) => !c.projectId || !known.has(c.projectId)).sort(byName)) emitConn(c, 0, null);
  return rows;
}
