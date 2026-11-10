// `pnpm check:tree`: the Bookmarks section and the "show only bookmarked" filter in buildRows.
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import type { Bookmark, ConnectionConfig, TreeNode } from "../../../lib/types.ts";
import { nodeKey } from "./keys.ts";
import { bookmarkHint, buildRows, type Row } from "./rows.ts";
import type { NodeState } from "./store";

function eq(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}\n  expected ${e}\n  actual   ${a}`);
}

const conn = (id: string): ConnectionConfig => ({ id, name: id, kind: "postgres", host: "", user: "", options: {}, readOnly: false, projectId: null });
const node = (path: string[], kind: TreeNode["kind"], hasChildren = true): TreeNode => ({ path, label: path[path.length - 1], kind, hasChildren });
const bm = (connectionId: string, path: string[], kind: Bookmark["kind"], database: string | null = null): Bookmark => ({
  id: `${connectionId}:${path.join("/")}`, connectionId, path, label: path[path.length - 1], kind, order: 0,
  table: { database, schema: null, name: path[path.length - 1] },
});

// pg: app/public/tables/{users,orders} and analytics/public/tables/{events,clicks}; other: db1, db2.
const nodes: Record<string, NodeState> = {};
const put = (connId: string, path: string[], children: TreeNode[]) => { nodes[nodeKey(connId, path)] = { expanded: true, children }; };
put("pg", [], [node(["app"], "database"), node(["analytics"], "database")]);
for (const db of ["app", "analytics"]) {
  put("pg", [db], [node([db, "public"], "schema")]);
  put("pg", [db, "public"], [node([db, "public", "tables"], "folder")]);
}
put("pg", ["app", "public", "tables"], [node(["app", "public", "tables", "users"], "table", false), node(["app", "public", "tables", "orders"], "table", false)]);
put("pg", ["analytics", "public", "tables"], [node(["analytics", "public", "tables", "events"], "table", false), node(["analytics", "public", "tables", "clicks"], "table", false)]);
put("other", [], [node(["db1"], "database", false), node(["db2"], "database", false)]);

const label = (r: Row) =>
  r.type === "node" ? r.node.path.join("/")
  : r.type === "conn" ? `conn:${r.conn.id}`
  : r.type === "bookmark" ? `bm:${r.bookmark.label}`
  : r.type;

const users = bm("pg", ["app", "public", "tables", "users"], "table", "app");
const analytics = bm("pg", ["analytics"], "database", "analytics");
const base = {
  connections: [conn("pg"), conn("other")], projects: [], nodes, collapsedProjects: {}, query: "",
  bookmarks: [] as Bookmark[], bookmarksCollapsed: false, bookmarksOnly: false,
};
const run = (over: Partial<typeof base>) => buildRows({ ...base, ...over });
const labels = (over: Partial<typeof base>) => run(over).map(label);

const OTHER = ["conn:other", "db1", "db2"];
const APP = ["app", "app/public", "app/public/tables", "app/public/tables/users", "app/public/tables/orders"];
const ANALYTICS = ["analytics", "analytics/public", "analytics/public/tables", "analytics/public/tables/events", "analytics/public/tables/clicks"];
const FULL = [...OTHER, "conn:pg", ...APP, ...ANALYTICS];

eq(labels({}), FULL, "no bookmarks: no section, full tree");

const both = [users, analytics];
eq(labels({ bookmarks: both }), ["bookmarks", "bm:users", "bm:analytics", ...FULL], "section listed first, in bookmark order");
eq(labels({ bookmarks: both, bookmarksCollapsed: true }), ["bookmarks", ...FULL], "collapsed section shows only its header");
const flags = run({ bookmarks: both }).filter((r) => r.type === "node").map((r) => (r.type === "node" && r.bookmarked ? r.node.label : null)).filter(Boolean);
eq(flags, ["users", "analytics"], "bookmarked nodes are flagged");

eq(
  labels({ bookmarks: both, bookmarksOnly: true }),
  ["bookmarks", "bm:users", "bm:analytics", ...OTHER, "conn:pg", "app", "app/public", "app/public/tables", "app/public/tables/users", ...ANALYTICS],
  "filter: unbookmarked siblings hidden, ancestors kept, bookmarked database fully visible, other connection untouched",
);

eq(
  labels({ bookmarks: both, bookmarksOnly: true, query: "click" }),
  ["conn:pg", "analytics", "analytics/public", "analytics/public/tables", "analytics/public/tables/clicks"],
  "filter + search: both must match",
);
eq(labels({ bookmarks: both, bookmarksOnly: true, query: "orders" }), [], "search can't reach nodes the filter hides");
eq(labels({ bookmarks: both, query: "orders" }), ["conn:pg", ...APP.slice(0, 3), "app/public/tables/orders"], "without the filter search finds them");
eq(labels({ bookmarks: both, query: "users" }), ["bookmarks", "bm:users", "conn:pg", ...APP.slice(0, 4)], "search filters the section too");

eq(labels({ bookmarks: both, bookmarksOnly: true, connections: [conn("other")] }), OTHER, "bookmarks of hidden or deleted connections are not shown");

eq([bookmarkHint(users, conn("pg")), bookmarkHint(analytics, conn("pg"))], ["pg · app", "pg"], "bookmark hints");

// SSH session variants: under their session, wherever it is; on their own if it's gone.
const ssh = (id: string, parent?: string, projectId: string | null = null): ConnectionConfig =>
  ({ ...conn(id), kind: "ssh", projectId, options: parent ? { parent } : {} });
const sshConns = [ssh("mini", undefined, "home"), ssh("mini-build", "mini", "home"), ssh("mini-agent", "mini", "home"), ssh("lost", "gone")];
const sshRows = (query = "") => run({ connections: sshConns, query, projects: [{ id: "home", name: "home" } as never] });
eq(sshRows().map((r) => `${label(r)}@${r.depth}`), ["project@0", "conn:mini@1", "conn:mini-agent@2", "conn:mini-build@2", "conn:lost@0"], "variants nested under their session");
eq(sshRows().find((r) => r.id === "c:mini-build")?.parentId, "c:mini", "a variant's tree parent is its session");
eq(sshRows("build").map(label), ["project", "conn:mini", "conn:mini-build"], "search finds a variant and shows its session");
eq(sshRows("mini-a").map(label), ["project", "conn:mini", "conn:mini-agent"], "only matching variants");
eq(sshRows("home").map(label), ["project", "conn:mini", "conn:mini-agent", "conn:mini-build"], "project match: all of it");
eq((sshRows()[0] as Extract<Row, { type: "project" }>).count, 1, "a project counts sessions, not their variants");

console.log("tree rows: all checks passed");
