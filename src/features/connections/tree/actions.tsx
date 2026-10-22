// Tree actions: activation (double-click / Enter), connection management and
// destructive object operations.
import { toast } from "sonner";
import { api, errMsg } from "../../../lib/api";
import { dialectOf, type ConnectionConfig, type TableRef, type TreeNode } from "../../../lib/types";
import { useConnections } from "../../../store/connections";
import { useUi } from "../../../store/ui";
import { confirmDialog } from "../dialogs";
import { openDdl, openInfra, openMongoCollection, openRedis, openTableData } from "../openers";
import { qualifiedName } from "../../table/sqlDialect";
import { isInfra, redisKeyOf, refOf, type NodeCtx } from "./rows";
import { useTree } from "./store";
import { openSshTerminal } from "../../ssh/open";

const tree = () => useTree.getState();

export function toggleConnection(conn: ConnectionConfig) {
  if (isInfra(conn)) return;
  const n = tree().nodes[`${conn.id}|`];
  if (n?.expanded) tree().setExpanded(conn.id, [], false);
  else void tree().expandConnection(conn.id);
}

/** Double-click / Enter on a connection row. */
export function activateConnection(conn: ConnectionConfig) {
  if (conn.kind === "kubernetes") openInfra(conn.id, "k8s");
  else if (conn.kind === "rabbitmq") openInfra(conn.id, "rabbitmq");
  else if (conn.kind === "ssh") openSshTerminal(conn);
  else toggleConnection(conn);
}

export function toggleNode(conn: ConnectionConfig, node: TreeNode, expanded: boolean) {
  if (!node.hasChildren) return;
  tree().setExpanded(conn.id, node.path, !expanded);
}

/** Double-click / Enter on an object node. Returns true if it opened something. */
export function activateNode(conn: ConnectionConfig, node: TreeNode, ctx: NodeCtx, expanded: boolean): boolean {
  const ref = refOf(node, ctx);
  switch (node.kind) {
    case "table":
    case "view":
    case "materializedView":
      if (conn.kind === "mongodb") openMongoCollection(conn.id, ref);
      else openTableData(conn.id, ref);
      return true;
    case "collection":
      openMongoCollection(conn.id, ref);
      return true;
    case "function":
    case "procedure":
    case "trigger":
    case "sequence":
    case "event":
      openDdl(conn.id, ref, node.kind);
      return true;
    case "index":
      if (conn.kind === "mongodb" || !node.table) break;
      openDdl(conn.id, ref, node.kind);
      return true;
    case "key":
      openRedis(conn.id, node.table?.database ?? ctx.database, redisKeyOf(node));
      return true;
    case "database":
      if (conn.kind === "redis") {
        openRedis(conn.id, ctx.database);
        return true;
      }
      break;
  }
  toggleNode(conn, node, expanded);
  return false;
}

export async function disconnect(conn: ConnectionConfig) {
  try {
    await api.closeConnection(conn.id);
  } catch (e) {
    toast.error(`Disconnect failed: ${errMsg(e)}`);
  }
  useConnections.getState().markOpen(conn.id, false);
  tree().clearConn(conn.id);
}

export async function duplicateConnection(conn: ConnectionConfig) {
  try {
    const full = await api.getConnection(conn.id);
    const saved = await useConnections.getState().save({ ...full, id: "", name: `${full.name} copy` });
    toast.success(`Created ${saved.name}`);
    useUi.getState().openConnectionDialog(saved.id);
  } catch (e) {
    toast.error(`Duplicate failed: ${errMsg(e)}`);
  }
}

export async function deleteConnection(conn: ConnectionConfig) {
  const ok = await confirmDialog({
    title: "Delete connection",
    message: <>Delete the connection <b>{conn.name}</b>? Saved credentials for it are removed too. This cannot be undone.</>,
    confirmLabel: "Delete",
    danger: true,
  });
  if (!ok) return;
  try {
    await useConnections.getState().remove(conn.id);
    tree().clearConn(conn.id);
    toast.success(`Deleted ${conn.name}`);
  } catch (e) {
    toast.error(`Delete failed: ${errMsg(e)}`);
  }
}

async function runDestructive(conn: ConnectionConfig, title: string, sql: string, opts: { database?: string | null; schema?: string | null }, refreshPath: string[], done: string) {
  const ok = await confirmDialog({
    title,
    message: conn.readOnly
      ? <span className="text-warn">This connection is marked read-only.</span>
      : "The following statement will be executed:",
    code: sql,
    confirmLabel: "Execute",
    danger: true,
  });
  if (!ok) return;
  try {
    await api.execute(conn.id, sql, { ...opts, skipHistory: true });
    toast.success(done);
    tree().refresh(conn.id, refreshPath);
  } catch (e) {
    toast.error(errMsg(e));
  }
}

export function truncateTable(conn: ConnectionConfig, ref: TableRef, parentPath: string[]) {
  const d = dialectOf(conn.kind);
  const name = qualifiedName(d, ref);
  const sql = d === "sqlite" ? `DELETE FROM ${name};` : `TRUNCATE TABLE ${name};`;
  return runDestructive(conn, `Truncate ${ref.name}`, sql, { database: ref.database, schema: ref.schema }, parentPath, `Truncated ${ref.name}`);
}

export function dropObject(conn: ConnectionConfig, node: TreeNode, ref: TableRef, parentPath: string[]) {
  const d = dialectOf(conn.kind);
  const name = qualifiedName(d, ref);
  const what = node.kind === "view" ? "VIEW" : node.kind === "materializedView" ? "MATERIALIZED VIEW" : "TABLE";
  const sql = `DROP ${what} ${name};`;
  return runDestructive(conn, `Drop ${ref.name}`, sql, { database: ref.database, schema: ref.schema }, parentPath, `Dropped ${ref.name}`);
}

export function dropCollection(conn: ConnectionConfig, ref: TableRef, parentPath: string[]) {
  const sql = `db.getCollection(${JSON.stringify(ref.name)}).drop()`;
  return runDestructive(conn, `Drop collection ${ref.name}`, sql, { database: ref.database }, parentPath, `Dropped ${ref.name}`);
}

export async function deleteRedisKey(conn: ConnectionConfig, database: string | null, key: string, parentPath: string[]) {
  const ok = await confirmDialog({
    title: "Delete key",
    message: <>Delete the key <code className="font-mono text-accent">{key}</code>?</>,
    confirmLabel: "Delete",
    danger: true,
  });
  if (!ok) return;
  try {
    await api.deleteKey(conn.id, database, key);
    toast.success(`Deleted ${key}`);
    tree().refresh(conn.id, parentPath);
  } catch (e) {
    toast.error(errMsg(e));
  }
}

export function copyText(text: string) {
  navigator.clipboard.writeText(text).then(
    () => toast.success("Copied to clipboard"),
    (e) => toast.error(errMsg(e)),
  );
}

export async function refreshAll() {
  try {
    await useConnections.getState().load();
  } catch (e) {
    toast.error(errMsg(e));
  }
  const { nodes, refresh } = tree();
  for (const c of useConnections.getState().connections) {
    if (nodes[`${c.id}|`]?.expanded && useConnections.getState().open.has(c.id)) refresh(c.id, []);
  }
}
