// Tab openers with stable keys, so re-opening the same object focuses its tab.
import type { NodeKind, TableRef } from "../../lib/types";
import { useConnections } from "../../store/connections";
import { openTab } from "../../store/tabs";

const connName = (id: string) => useConnections.getState().byId(id)?.name ?? "?";
const refKey = (t: TableRef) => `${t.database ?? ""}.${t.schema ?? ""}.${t.name}`;

export function openQuery(connId: string, opts: { database?: string | null; schema?: string | null; initialSql?: string; savedQueryId?: string; title?: string } = {}) {
  const key = opts.savedQueryId ? `saved:${opts.savedQueryId}` : undefined;
  return openTab(
    { type: "query", connId, database: opts.database ?? null, schema: opts.schema ?? null, initialSql: opts.initialSql, savedQueryId: opts.savedQueryId },
    opts.title ?? `Query – ${connName(connId)}`,
    key,
  );
}

export function openTableData(connId: string, table: TableRef) {
  return openTab({ type: "table-data", connId, table }, table.name, `table:${connId}:${refKey(table)}`);
}

export function openTableDesign(connId: string, table: TableRef | null, database?: string | null, schema?: string | null) {
  if (table) return openTab({ type: "table-design", connId, table }, `Design ${table.name}`, `design:${connId}:${refKey(table)}`);
  return openTab({ type: "table-design", connId, table: null, database, schema }, "New table");
}

export function openDdl(connId: string, table: TableRef, kind: NodeKind) {
  return openTab({ type: "ddl", connId, table, kind }, `DDL ${table.name}`, `ddl:${connId}:${kind}:${refKey(table)}`);
}

export function openMongoCollection(connId: string, table: TableRef) {
  return openTab({ type: "mongo-collection", connId, table }, table.name, `mongo:${connId}:${refKey(table)}`);
}

export function openRedis(connId: string, db: string | null, key?: string | null) {
  // Tree paths use "db3", key refs use "3"; the backend accepts both. Normalize for stable tab keys.
  const database = db ? db.replace(/^db(?=\d+$)/, "") : db;
  const title = key ? key : `${connName(connId)} db${database ?? 0}`;
  return openTab({ type: "redis", connId, database, key: key ?? null }, title, `redis:${connId}:${database ?? ""}:${key ?? ""}`);
}

export function openErDiagram(connId: string, database?: string | null, schema?: string | null) {
  return openTab({ type: "er-diagram", connId, database, schema }, `ER ${schema ?? database ?? connName(connId)}`, `er:${connId}:${database ?? ""}:${schema ?? ""}`);
}

export function openDataTransfer(connId?: string, database?: string | null, schema?: string | null) {
  return openTab({ type: "data-transfer", connId, database, schema }, "Data Transfer");
}

export function openSchemaCompare(sourceConnId?: string) {
  return openTab({ type: "schema-compare", sourceConnId }, "Schema Compare");
}

export function openHistoryTab() {
  return openTab({ type: "history" }, "History", "history");
}

export function openInfra(connId: string, kind: "k8s" | "rabbitmq" | "docker") {
  return openTab({ type: kind, connId }, connName(connId), `${kind}:${connId}`);
}
