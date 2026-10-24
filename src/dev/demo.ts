// Demo data for `vite --mode demo` (README screenshots / video, see promo/). Answers the commands
// that need a database, VPN, cluster or agent with made-up data; everything else (git, files)
// still goes to the Rust devbridge. Nothing here is real: names, hosts and rows are invented.
import type {
  Bookmark, ColumnInfo, ConnectionConfig, NetProfile, NetStatus, Note, Project, QueryResult, TableDataPage, TableStructure, TreeNode,
} from "../lib/types";

type Emit = (event: string, payload: unknown) => void;

const NOW = Math.floor(Date.now() / 60_000) * 60_000;
const iso = (minsAgo: number) => new Date(NOW - minsAgo * 60_000).toISOString();
/** How Postgres prints a timestamptz. */
const pgTime = (minsAgo: number) => iso(minsAgo).replace("T", " ").replace(/\.\d+Z$/, "+00");

// ---------------------------------------------------------------- projects & connections

const projects: Project[] = [
  { id: "p-shop", name: "Acme Shop", color: "#22c55e", order: 0 },
  { id: "p-pay", name: "Payments", color: "#3b82f6", order: 1 },
];

const conn = (id: string, name: string, kind: ConnectionConfig["kind"], host: string, port: number, projectId: string, extra: Partial<ConnectionConfig> = {}): ConnectionConfig => ({
  id, name, kind, host, port, user: "app", database: null, projectId, options: {}, readOnly: false, ...extra,
});

const connections: ConnectionConfig[] = [
  conn("c-pg", "shop-db", "postgres", "db.acme.internal", 5432, "p-shop", { database: "shop", color: "#22c55e" }),
  conn("c-redis", "shop-cache", "redis", "cache.acme.internal", 6379, "p-shop"),
  conn("c-k8s", "prod-cluster", "kubernetes", "", 0, "p-shop", { options: { context: "acme-prod" } }),
  conn("c-mysql", "ledger", "mysql", "ledger.acme.internal", 3306, "p-pay", { database: "ledger" }),
  conn("c-mongo", "events-store", "mongodb", "mongo.acme.internal", 27017, "p-pay"),
  conn("c-mq", "payments-mq", "rabbitmq", "mq.acme.internal", 5672, "p-pay"),
];

let bookmarks: Bookmark[] = [
  { id: "b-orders", connectionId: "c-pg", path: ["shop", "public", "tables", "orders"], label: "orders", kind: "table", table: { database: "shop", schema: "public", name: "orders" }, order: 0 },
  { id: "b-analytics", connectionId: "c-pg", path: ["analytics"], label: "analytics", kind: "database", table: { database: "analytics", schema: null, name: "analytics" }, order: 1 },
];

// ---------------------------------------------------------------- shop database

const ORDER_COLS: ColumnInfo[] = [
  { name: "id", typeName: "int8" }, { name: "customer", typeName: "text" }, { name: "email", typeName: "text" },
  { name: "status", typeName: "text" }, { name: "total", typeName: "numeric" }, { name: "items", typeName: "int4" },
  { name: "country", typeName: "text" }, { name: "paid", typeName: "bool" }, { name: "created_at", typeName: "timestamptz" },
];
const FIRST = ["Ava", "Noah", "Mia", "Liam", "Zoe", "Ethan", "Iris", "Leo", "Nora", "Omar", "Ines", "Kai", "Sara", "Theo", "Lina", "Max"];
const LAST = ["Tan", "Silva", "Novak", "Haddad", "Kim", "Moreau", "Rossi", "Okafor", "Lindqvist", "Santos", "Wright", "Ito"];
const STATUS = ["delivered", "shipped", "paid", "pending", "delivered", "refunded", "delivered", "shipped"];
const COUNTRY = ["ID", "SG", "DE", "US", "NL", "JP", "BR", "AU", "GB", "FR"];
let seed = 7;
const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
const ORDERS: unknown[][] = Array.from({ length: 240 }, (_, i) => {
  const f = FIRST[Math.floor(rnd() * FIRST.length)], l = LAST[Math.floor(rnd() * LAST.length)];
  const status = STATUS[Math.floor(rnd() * STATUS.length)];
  const items = 1 + Math.floor(rnd() * 6);
  return [
    10480 - i, `${f} ${l}`, `${f}.${l}@example.com`.toLowerCase(), status, (items * (9 + rnd() * 70)).toFixed(2), items,
    COUNTRY[Math.floor(rnd() * COUNTRY.length)], status !== "pending", pgTime(i * 37 + Math.floor(rnd() * 30)),
  ];
});

const TABLES: Record<string, { rows: number; cols: ColumnInfo[] }> = {
  customers: { rows: 18_204, cols: [] }, order_items: { rows: 96_311, cols: [] }, orders: { rows: ORDERS.length, cols: ORDER_COLS },
  products: { rows: 1_204, cols: [] }, refunds: { rows: 312, cols: [] }, shipments: { rows: 40_982, cols: [] },
};

function children(connId: string, path: string[]): TreeNode[] {
  if (connId !== "c-pg") return [];
  const node = (label: string, kind: TreeNode["kind"], hasChildren: boolean, detail?: string): TreeNode => ({
    path: [...path, label === "Tables" ? "tables" : label === "Views" ? "views" : label === "Functions" ? "functions" : label],
    label, kind, hasChildren, detail: detail ?? null,
    table: kind === "table" || kind === "view" ? { database: "shop", schema: "public", name: label } : null,
  });
  switch (path.length) {
    case 0: return [node("shop", "database", true), node("analytics", "database", true)];
    case 1: return [node("public", "schema", true), node("billing", "schema", true)];
    case 2: return [node("Tables", "folder", true), node("Views", "folder", true), node("Functions", "folder", true)];
    case 3:
      if (path[2] === "tables") return Object.entries(TABLES).map(([n, t]) => node(n, "table", true, t.rows >= 1000 ? `${Math.round(t.rows / 1000)}K` : String(t.rows)));
      if (path[2] === "views") return [node("daily_revenue", "view", false), node("top_customers", "view", false)];
      return [node("refresh_stats()", "function", false)];
    default:
      return path[3] === "orders"
        ? ORDER_COLS.map((c) => ({ path: [...path, c.name], label: c.name, kind: "column", hasChildren: false, detail: c.typeName, table: null }))
        : [];
  }
}

/** Understands the WHERE parts DevCat itself generates for the demo table (contains / equals). */
function applyFilter(rows: unknown[][], filter: string | null | undefined): unknown[][] {
  if (!filter) return rows;
  const parts = [...filter.matchAll(/"(\w+)"(?: AS \w+\))? (ILIKE|=) '([^']*)'/g)];
  return rows.filter((r) => parts.every(([, col, op, v]) => {
    const i = ORDER_COLS.findIndex((c) => c.name === col);
    if (i < 0) return true;
    const cell = String(r[i]).toLowerCase();
    return op === "=" ? cell === v.toLowerCase() : cell.includes(v.replace(/^%|%$/g, "").toLowerCase());
  }));
}

function tableData(args: { req: { table: { name: string }; offset: number; limit: number; filter?: string | null; sort: { column: string; desc: boolean }[] } }): TableDataPage {
  const { req } = args;
  if (req.table.name !== "orders") return { result: result("", [{ name: "id", typeName: "int8" }], []), total: 0, primaryKey: ["id"] };
  let rows = applyFilter(ORDERS, req.filter);
  const s = req.sort[0];
  if (s) {
    const i = ORDER_COLS.findIndex((c) => c.name === s.column);
    rows = [...rows].sort((a, b) => (a[i]! < b[i]! ? -1 : a[i]! > b[i]! ? 1 : 0) * (s.desc ? -1 : 1));
  }
  return { result: result("", ORDER_COLS, rows.slice(req.offset, req.offset + req.limit), 4), total: rows.length, primaryKey: ["id"] };
}

const result = (statement: string, columns: ColumnInfo[], rows: unknown[][], elapsedMs = 6): QueryResult =>
  ({ statement, columns, rows: rows as QueryResult["rows"], elapsedMs, truncated: false, rowsAffected: null, message: null });

function execute(script: string): QueryResult[] {
  const by = new Map<string, { n: number; sum: number }>();
  for (const r of ORDERS) {
    const k = String(r[6]);
    const e = by.get(k) ?? { n: 0, sum: 0 };
    e.n++; e.sum += Number(r[4]);
    by.set(k, e);
  }
  const rows = [...by.entries()].sort((a, b) => b[1].sum - a[1].sum)
    .map(([c, e]) => [c, e.n, e.sum.toFixed(2), (e.sum / e.n).toFixed(2)]);
  return [result(script.trim(), [
    { name: "country", typeName: "text" }, { name: "orders", typeName: "int8" },
    { name: "revenue", typeName: "numeric" }, { name: "avg_order", typeName: "numeric" },
  ], rows, 12)];
}

const structure: TableStructure = {
  table: { database: "shop", schema: "public", name: "orders" },
  columns: ORDER_COLS.map((c, i) => ({
    name: c.name, dataType: c.typeName, nullable: !["id", "customer", "status", "total"].includes(c.name),
    default: c.name === "id" ? "nextval('orders_id_seq')" : c.name === "created_at" ? "now()" : null,
    isPrimaryKey: c.name === "id", autoIncrement: c.name === "id", comment: null, ordinal: i + 1,
  })),
  indexes: [
    { name: "orders_pkey", columns: ["id"], unique: true, primary: true, method: "btree" },
    { name: "orders_status_idx", columns: ["status", "created_at"], unique: false, primary: false, method: "btree" },
  ],
  foreignKeys: [],
  comment: null,
};

// ---------------------------------------------------------------- VPN & tunnels

const netProfiles: NetProfile[] = [
  { id: "n-ovpn", name: "office-vpn", kind: "openvpn", projectId: null, autoConnect: false, forwards: [], summary: "udp vpn.acme.example:1194", options: {} },
  { id: "n-wg", name: "staging-wg", kind: "wireguard", projectId: "p-pay", autoConnect: false, forwards: [], summary: "10.40.0.2/32 → wg.acme.example:51820", options: {} },
  { id: "n-ssh", name: "bastion", kind: "ssh", projectId: "p-pay", autoConnect: false, ssh: { enabled: true, host: "bastion.acme.example", port: 22, user: "deploy", auth: "key", keyPath: "~/.ssh/id_ed25519" },
    forwards: [{ kind: "local", bindPort: 13306, remoteHost: "ledger.acme.internal", remotePort: 3306 }], summary: "deploy@bastion.acme.example", options: {} },
  { id: "n-k8s", name: "shop-db (k8s)", kind: "k8s", projectId: "p-shop", autoConnect: false,
    forwards: [{ kind: "service", bindPort: 6543, remoteHost: "shop-db-rw", remotePort: 5432, namespace: "database" }], summary: "svc/shop-db-rw → :6543", options: { k8sConnId: "c-k8s" } },
];
const netStatus: NetStatus[] = [
  { id: "n-ovpn", state: "connected", since: iso(42), detail: "10.8.0.14 via utun6", error: null, localPorts: [] },
  { id: "n-ssh", state: "connected", since: iso(18), detail: "localhost:13306 → ledger.acme.internal:3306", error: null, localPorts: [13306] },
  { id: "n-k8s", state: "connected", since: iso(7), detail: "svc/shop-db-rw → pod shop-db-1", error: null, localPorts: [6543] },
];
const traffic = (id: string) => Array.from({ length: 120 }, (_, i) => {
  const wave = id === "n-ovpn" ? 1 : id === "n-k8s" ? 0.4 : 0.2;
  const rx = Math.round(wave * (40_000 + 30_000 * Math.sin(i / 6) + 25_000 * Math.sin(i / 2.3) ** 2 + (i % 17 === 0 ? 120_000 : 0)));
  const tx = Math.round(rx * 0.35);
  return { id, at: Date.now() - (120 - i) * 1000, rxRate: Math.max(rx, 800), txRate: Math.max(tx, 300), rxTotal: 48_000_000 + i * rx, txTotal: 9_000_000 + i * tx };
});

// ---------------------------------------------------------------- Kubernetes

const uid = (s: string) => `uid-${s}`;
const pod = (name: string, ns: string, app: string, phase: string, restarts: number, age: number, node: string) => ({
  apiVersion: "v1", kind: "Pod",
  metadata: { name, namespace: ns, uid: uid(name), creationTimestamp: iso(age), labels: { app } },
  spec: { nodeName: node, containers: [{ name: app, image: `ghcr.io/acme/${app}:1.${(restarts + 7) % 10}.2` }] },
  status: {
    phase, podIP: `10.42.${age % 7}.${(age * 3) % 250}`,
    containerStatuses: [{ name: app, ready: phase === "Running", restartCount: restarts, state: phase === "Running" ? { running: {} } : { waiting: { reason: "ContainerCreating" } } }],
    conditions: [{ type: "Ready", status: phase === "Running" ? "True" : "False" }],
  },
});
const PODS = [
  pod("storefront-7f9c6d8b5-2xkqp", "shop", "storefront", "Running", 0, 3 * 1440, "node-a"),
  pod("storefront-7f9c6d8b5-h7wzn", "shop", "storefront", "Running", 0, 3 * 1440, "node-b"),
  pod("storefront-7f9c6d8b5-q4mfr", "shop", "storefront", "Running", 1, 3 * 1440, "node-c"),
  pod("checkout-api-5b6d9f7c4-8lmzv", "shop", "checkout-api", "Running", 0, 26 * 60, "node-a"),
  pod("checkout-api-5b6d9f7c4-vt2nb", "shop", "checkout-api", "Running", 0, 26 * 60, "node-b"),
  pod("search-indexer-6c8b7d9f-kq9xw", "shop", "search-indexer", "Running", 2, 9 * 1440, "node-c"),
  pod("image-resizer-54f7b9c8d-zr5tp", "shop", "image-resizer", "Pending", 0, 1, "node-a"),
  pod("shop-db-1", "database", "shop-db", "Running", 0, 21 * 1440, "node-b"),
  pod("shop-db-2", "database", "shop-db", "Running", 0, 21 * 1440, "node-c"),
  pod("redis-0", "database", "redis", "Running", 0, 40 * 1440, "node-a"),
  pod("payments-worker-6d5c4b7f8-9jxlk", "payments", "payments-worker", "Running", 0, 5 * 60, "node-b"),
  pod("payments-worker-6d5c4b7f8-mw3rs", "payments", "payments-worker", "Running", 0, 5 * 60, "node-c"),
  pod("ingress-nginx-controller-7c9d8-fx2lq", "ingress-nginx", "ingress-nginx", "Running", 0, 60 * 1440, "node-a"),
];
const overview = {
  version: "v1.33.4", platform: "linux/amd64", context: "acme-prod", server: "https://k8s.acme.example:6443",
  nodesTotal: 3, nodesReady: 3, podsTotal: PODS.length, podsByPhase: { Running: PODS.length - 1, Pending: 1 }, podsCapacity: 330,
  containerRestarts: 3, deploymentsTotal: 7, deploymentsReady: 6, namespaces: 6,
  cpu: { capacity: 24, allocatable: 23.4, requests: 9.8, limits: 18, usage: 6.1 },
  memory: { capacity: 96 * 2 ** 30, allocatable: 92 * 2 ** 30, requests: 41 * 2 ** 30, limits: 70 * 2 ** 30, usage: 37 * 2 ** 30 },
  metricsAvailable: true, warnings: [], errors: [],
};

// ---------------------------------------------------------------- notes

const notes: Note[] = [
  { id: "note-1", projectId: "p-shop", pinned: true, createdAt: NOW - 86_400_000 * 3, updatedAt: NOW - 3_600_000,
    body: "# Black Friday checklist\n\n- [x] Scale `storefront` to 6 replicas\n- [x] Warm the Redis cache\n- [ ] Raise `checkout-api` rate limit to 400 rps\n- [ ] Freeze deploys Thu 18:00 → Mon 09:00\n\n## Useful queries\n\n```sql\nSELECT status, count(*) FROM orders\nWHERE created_at > now() - interval '1 hour'\nGROUP BY 1;\n```\n" },
  { id: "note-2", projectId: "p-pay", pinned: false, createdAt: NOW - 86_400_000 * 9, updatedAt: NOW - 86_400_000,
    body: "# Ledger migration\n\nRun on the bastion tunnel (`localhost:13306`). Dry-run first, then `--apply`." },
  { id: "note-3", projectId: null, pinned: false, createdAt: NOW - 86_400_000 * 20, updatedAt: NOW - 86_400_000 * 2,
    body: "# On-call\n\nPager rotation, runbooks and the VPN profile to use for each environment." },
];

// ---------------------------------------------------------------- terminals (AI workspace)

const ESC = "\x1b[";
const c = (code: string, s: string) => `${ESC}${code}m${s}${ESC}0m`;
const CLAUDE_SESSION = [
  "",
  `${c("38;5;209", "✻")} ${c("1", "Claude Code")}  ${c("2", "~/acme/storefront")}`,
  "",
  `${c("2", ">")} The checkout page shows the wrong total when a coupon is applied. Find the bug and fix it.`,
  "",
  `${c("38;5;114", "⏺")} I'll look at how the cart total is computed.`,
  "",
  `${c("38;5;114", "⏺")} ${c("1", "Search")}(pattern: "applyCoupon", path: "src")`,
  `  ⎿  Found 3 files`,
  `${c("38;5;114", "⏺")} ${c("1", "Read")}(src/cart/total.ts)`,
  `  ⎿  Read 48 lines`,
  "",
  `${c("38;5;114", "⏺")} The coupon is subtracted ${c("1", "before")} tax is added, but the percentage is`,
  `  taken from the taxed subtotal, so percentage coupons are applied twice.`,
  "",
  `${c("38;5;114", "⏺")} ${c("1", "Update")}(src/cart/total.ts)`,
  `  ⎿  Updated src/cart/total.ts with 3 additions and 2 removals`,
  `      ${c("2", "31")}    const subtotal = sumLines(cart.lines);`,
  `      ${c("2", "32")} ${c("48;5;52", "-  const discount = applyCoupon(withTax(subtotal), cart.coupon);")}`,
  `      ${c("2", "32")} ${c("48;5;22", "+  const discount = applyCoupon(subtotal, cart.coupon);")}`,
  `      ${c("2", "33")} ${c("48;5;22", "+  // tax is charged on the discounted amount")}`,
  `      ${c("2", "34")}    return withTax(subtotal - discount);`,
  "",
  `${c("38;5;114", "⏺")} ${c("1", "Bash")}(pnpm test cart)`,
  `  ⎿  ${c("32", "✓")} cart/total.test.ts (14 tests) 38ms`,
  `     ${c("32", "Test Files  1 passed")} (1)`,
  "",
  `${c("38;5;114", "⏺")} Fixed: coupons now apply to the pre-tax subtotal. Added a test for a 20% coupon`,
  `  on a taxed cart; all 14 cart tests pass.`,
  "",
  `${c("2", "╭" + "─".repeat(74) + "╮")}`,
  `${c("2", "│")} >${" ".repeat(72)}${c("2", "│")}`,
  `${c("2", "╰" + "─".repeat(74) + "╯")}`,
  `  ${c("2", "? for shortcuts")}`,
].join("\r\n");
const SHELL_SESSION = [
  `${c("36", "~/acme/storefront")} ${c("35", "main")} ${c("2", "❯")} pnpm dev`,
  "",
  `  ${c("1;32", "VITE")} v8.3.4  ${c("2", "ready in")} 412 ms`,
  "",
  `  ${c("32", "➜")}  ${c("1", "Local")}:   ${c("36", "http://localhost:5173/")}`,
  `  ${c("2", "➜  Network: use --host to expose")}`,
  "",
].join("\r\n");

// ---------------------------------------------------------------- mobile (remote access + notifications)

const PHONE_ID = "8f3c1d9e" + "a".repeat(48) + "77e21a0b";
const remoteStatus = {
  settings: { enabled: true, hostName: null, port: 47100, publicAddr: null, web: true, relay: "public", relayUrl: null },
  running: true, error: null, id: "4be1" + "c".repeat(56) + "93d0", hostName: "Acme MacBook Pro", addrs: ["192.168.1.24:47100"],
  clients: [{ deviceId: PHONE_ID, name: "Ava's iPhone", platform: "ios", access: "full", since: NOW - 600_000, addr: "192.168.1.31:53120" }],
  pairingExpires: null, pending: [], webRunning: true, webError: null, webAvailable: true, relayConnected: "https://euc1-1.relay.n0.iroh.link./",
};
const remoteDevices = [
  { id: PHONE_ID, name: "Ava's iPhone", platform: "ios", access: "full", pairedAt: NOW - 86_400_000 * 2, lastSeen: NOW, kind: "app" },
  { id: "web-3f9a1c2b7d4e8f60", name: "Android browser", platform: "android", access: "readonly", pairedAt: NOW - 86_400_000 * 12, lastSeen: NOW - 86_400_000 * 3, kind: "web" },
];
const notifySettings = { enabled: true, server: "https://ntfy.sh", topic: "devcat-k3v9q2x7mf4hx2ab", token: null, onDone: true, onInput: true, minSeconds: 30, onlyWhenAway: true };
const audit = [
  { at: NOW - 120_000, device: PHONE_ID, deviceName: "Ava's iPhone", action: "db_apply_changes", detail: "[c-pg] orders: 1 change(s)", ok: true },
  { at: NOW - 540_000, device: PHONE_ID, deviceName: "Ava's iPhone", action: "db_execute", detail: "[c-pg] SELECT country, count(*) FROM orders GROUP BY 1", ok: true },
  { at: NOW - 900_000, device: PHONE_ID, deviceName: "Ava's iPhone", action: "pty_open", detail: "/Users/ava/acme/api-gateway (claude)", ok: true },
  { at: NOW - 1_800_000, device: PHONE_ID, deviceName: "Ava's iPhone", action: "k8s_restart", detail: "[c-k8s] deployments search-indexer", ok: true },
  { at: NOW - 2_400_000, device: PHONE_ID, deviceName: "Ava's iPhone", action: "net_connect", detail: "n-wg — handshake timed out", ok: false },
];
/** The QR code comes from the Rust devbridge (remote_qr), so it's a real, scannable one. */
const bridgeQr = (text: string) =>
  fetch("http://127.0.0.1:1430/invoke", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ cmd: "remote_qr", args: { text } }) })
    .then((r) => r.json()).then((b) => b.ok ?? "");

// ---------------------------------------------------------------- dispatcher

let streamSeq = 0;

/** Returns the demo answer, or `undefined` to let the devbridge handle the command. */
const demoMemories = [
  { place: { kind: "global" }, label: "Every project", handoffs: 0, memories: [
    { name: "prefers-pnpm", description: "Uses pnpm, never npm or yarn", kind: "user", body: "pnpm everywhere, including CI.", origin: "codex", updated: "2026-10-23T09:12:00Z", place: { kind: "global" } },
  ] },
  { place: { kind: "project", id: "p-shop" }, label: "Shop", handoffs: 1, memories: [
    { name: "staging-first", description: "Deploy to staging before prod", kind: "feedback", body: "Always deploy to staging first.\n**Why:** prod broke once.", origin: "claude-code", updated: "2026-10-22T16:40:00Z", place: { kind: "project", id: "p-shop" } },
    { name: "orders-schema-owner", description: "The billing team owns the orders schema", kind: "project", body: "Ask #billing before changing orders.*", origin: "gemini", updated: "2026-10-20T11:05:00Z", place: { kind: "project", id: "p-shop" } },
  ] },
];

const demoClaudeImport = {
  synced: false, auto: true, rules: true, rulesFiles: ["~/.codex/AGENTS.md", "~/.gemini/GEMINI.md"], notes: [],
  items: [
    { id: "/Users/demo/.claude/projects/-src-shop/memory/real-db-tests.md", folder: "~/src/shop", name: "real-db-tests", description: "Integration tests hit a real database, never mocks", kind: "feedback", global: false, target: "Shop", status: "new" },
    { id: "/Users/demo/.claude/projects/-src-shop/memory/role.md", folder: "~/src/shop", name: "user-role", description: "Backend engineer, new to the React side", kind: "user", global: true, target: "Shop", status: "new" },
    { id: "/Users/demo/.claude/projects/-src-shop/memory/staging.md", folder: "~/src/shop", name: "staging-first", description: "Deploy to staging before prod", kind: "feedback", global: false, target: "Shop", status: "same" },
  ],
};

export function demoInvoke(cmd: string, args: Record<string, unknown>, emit: Emit): unknown {
  switch (cmd) {
    case "list_projects": return projects;
    case "list_connections": return connections;
    case "list_bookmarks": return bookmarks;
    case "add_bookmark": {
      const b = args.bookmark as Bookmark;
      const existing = bookmarks.find((x) => x.connectionId === b.connectionId && x.path.join("/") === b.path.join("/"));
      if (existing) return existing;
      const saved = { ...b, id: `b-${Date.now()}`, order: bookmarks.length };
      bookmarks = [...bookmarks, saved];
      return saved;
    }
    case "remove_bookmark": bookmarks = bookmarks.filter((x) => x.id !== args.id); return null;
    case "reorder_bookmarks": {
      const ids = args.ids as string[];
      const pos = (id: string) => (ids.includes(id) ? ids.indexOf(id) : ids.length);
      bookmarks = [...bookmarks].sort((a, b) => pos(a.id) - pos(b.id)).map((b, order) => ({ ...b, order }));
      return bookmarks;
    }
    case "get_connection": return connections.find((x) => x.id === args.id);
    case "open_connection": return "connected";
    case "open_connection_ids": return ["c-pg", "c-k8s"];
    case "db_list_databases": return ["shop", "analytics"];
    case "db_children": return children(String(args.connId), (args.path as string[]) ?? []);
    case "db_table_data": return tableData(args as never);
    case "db_table_structure": return structure;
    case "db_execute": return execute(String(args.script ?? ""));
    case "db_schema_snapshot": return { tables: [structure], views: [] };
    case "notes_list": return notes;
    case "note_save": return args.note;
    case "net_list": return netProfiles;
    case "net_status": return netStatus;
    case "net_traffic": return traffic(String(args.id));
    case "net_logs": return [];
    case "net_tools": return { openvpn: "/Applications/DevCat.app/Contents/Resources/vpn/openvpn", wireguard: "/opt/local/bin/wg-quick", elevation: "osascript", installHints: {} };
    case "net_helper_status": return { supported: true, installed: true, upToDate: true, appVersion: "0.3.6", helperPath: "", sudoersPath: "", openvpnUpdate: false };
    case "net_get": {
      const profile = netProfiles.find((p) => p.id === args.id);
      return { profile, configText: profile?.kind === "openvpn" ? "client\ndev tun\nproto udp\nremote vpn.acme.example 1194\n" : null };
    }
    case "k8s_connect": return { version: overview.version, platform: overview.platform, context: overview.context, server: overview.server, namespace: "shop" };
    case "k8s_namespaces": return ["database", "default", "ingress-nginx", "kube-system", "payments", "shop"];
    case "k8s_cluster_overview": return overview;
    case "k8s_metrics":
      if (args.kind === "nodes") {
        return { available: true, items: ["node-a", "node-b", "node-c"].map((name, i) => ({ name, namespace: null, cpu: [2.4, 1.9, 1.8][i], memory: [13, 12, 12][i] * 2 ** 30, containers: [] })) };
      }
      return { available: true, items: PODS.map((p, i) => ({ name: p.metadata.name, namespace: p.metadata.namespace, cpu: 0.02 + ((i * 37) % 40) / 100, memory: (90 + ((i * 53) % 400)) * 2 ** 20, containers: [] })) };
    case "k8s_list":
      return args.kind === "nodes" ? ["node-a", "node-b", "node-c"].map((name) => ({
        apiVersion: "v1", kind: "Node", metadata: { name, uid: uid(name), creationTimestamp: iso(90 * 1440), labels: { "node-role.kubernetes.io/worker": "" } },
        status: { allocatable: { cpu: "7800m", memory: "30Gi", pods: "110" }, capacity: { cpu: "8", memory: "32Gi" },
          conditions: [{ type: "Ready", status: "True" }], nodeInfo: { kubeletVersion: "v1.33.4" } },
      })) : args.kind === "pods" ? PODS : [];
    case "k8s_port_forward_list": return [];
    case "k8s_helm_releases": return [];
    case "k8s_watch_start": {
      const id = `w${++streamSeq}`;
      const ns = args.namespace as string | null;
      const objs = args.kind === "pods" || args.kind === "Pod" ? PODS.filter((p) => !ns || p.metadata.namespace === ns) : [];
      setTimeout(() => emit("k8s-watch", { streamId: id, type: "Restarted", object: objs }), 30);
      return id;
    }
    case "k8s_stream_stop": return null;
    case "claude_info": return { path: "/usr/local/bin/claude", version: "2.1.0", models: [] };
    case "ai_provider_detect":
      return (["claude", "codex", "gemini"] as const).map((kind) => ({ kind, program: kind, path: `/usr/local/bin/${kind}`, version: "1.0.0" }));
    case "pty_open": {
      const id = `pty${++streamSeq}`;
      const text = args.program === "claude" ? CLAUDE_SESSION : SHELL_SESSION;
      const data = btoa(String.fromCharCode(...new TextEncoder().encode(text)));
      setTimeout(() => emit("pty-output", { id, data }), 50);
      return id;
    }
    case "pty_write": case "pty_resize": case "pty_close": return null;
    case "update_check": return null;
    case "remote_status": return remoteStatus;
    case "remote_save": return { ...remoteStatus, settings: args.settings };
    case "remote_devices": return remoteDevices;
    case "remote_audit": return audit;
    case "remote_qr": return bridgeQr(String(args.text));
    case "remote_pair_start": {
      const appLink = `devcat://pair?v=1&id=${remoteStatus.id}&code=K7Q2X9MPLA&name=Acme%20MacBook%20Pro&addr=192.168.1.24%3A47100`;
      const webLink = `http://192.168.1.24:47100/#v=1&id=${remoteStatus.id}&code=K7Q2X9MPLA&s=demo-secret-not-real&name=Acme%20MacBook%20Pro&relay=public`;
      const link = `https://devcathq.github.io/DevCat/#v=1&id=${remoteStatus.id}&code=K7Q2X9MPLA&name=Acme%20MacBook%20Pro&relay=public&addr=192.168.1.24%3A47100`;
      return bridgeQr(link).then((svg: string) => ({ link, appLink: `${appLink}&relay=public`, webLink, hostedLink: link, code: "K7Q2X9MPLA", svg, expiresAt: Date.now() + 272_000 }));
    }
    case "notify_settings": case "notify_save": return args.settings ?? notifySettings;
    case "memory_list": return demoMemories;
    case "memory_claude_preview": return demoClaudeImport;
    case "memory_sync_status": return { running: true, name: "Demo MacBook", peers: [{ id: "ab".repeat(32), name: "Office desktop", os: "linux", pairedAt: Date.now() - 86400000, lastSync: Date.now() - 120000, lastResult: "2 received, 1 sent", syncing: false }], pending: [], ticket: null };
    case "memory_sync_activity": return [{ at: Date.now() - 120000, device: "ab".repeat(32), deviceName: "Office desktop", action: "synced", detail: "2 received, 1 sent", ok: true }];
    case "memory_sync_all": return { imported: 2, updated: 0, removed: 0, agents: ["Claude Code", "Codex", "Gemini CLI"], rulesFiles: ["~/.codex/AGENTS.md"], notes: [] };
    case "memory_save": return args.memory;
    case "memory_delete": return null;
    default: return undefined;
  }
}
