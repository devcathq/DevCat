// Dialect helpers shared by the query editor, table data, tree actions and DDL generator.
import type { CellValue, ConnectionKind, Dialect, TableRef } from "../../lib/types";

export function quoteIdent(d: Dialect, ident: string): string {
  switch (d) {
    case "mysql": return "`" + ident.replace(/`/g, "``") + "`";
    case "mssql": return "[" + ident.replace(/]/g, "]]") + "]";
    default: return '"' + ident.replace(/"/g, '""') + '"';
  }
}

/**
 * Qualified table name for statements executed with ExecOptions { database, schema }
 * already set: PG/MSSQL use schema.table, MySQL db.table, SQLite bare name.
 */
export function qualifiedName(d: Dialect, t: TableRef): string {
  const q = (s: string) => quoteIdent(d, s);
  switch (d) {
    case "postgres":
    case "mssql":
    case "generic":
      return t.schema ? `${q(t.schema)}.${q(t.name)}` : q(t.name);
    case "mysql":
      return t.database ? `${q(t.database)}.${q(t.name)}` : q(t.name);
    case "sqlite":
      return t.schema && t.schema !== "main" ? `${q(t.schema)}.${q(t.name)}` : q(t.name);
    default:
      return t.name;
  }
}

/** Render a JS value as a SQL literal (mirrors sql_common::literal in Rust). */
export function sqlLiteral(d: Dialect, v: CellValue | undefined): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "boolean") return d === "postgres" ? (v ? "TRUE" : "FALSE") : v ? "1" : "0";
  if (typeof v === "number") return String(v);
  const s = typeof v === "string" ? v : JSON.stringify(v);
  const esc = s.replace(/'/g, "''");
  if (d === "mysql") return `'${esc.replace(/\\/g, "\\\\")}'`;
  if (d === "mssql") return `N'${esc}'`;
  return `'${esc}'`;
}

/** LIMIT clause / TOP prefix for a quick select. */
export function selectTop(d: Dialect, t: TableRef, n = 100): string {
  const name = qualifiedName(d, t);
  if (d === "mssql") return `SELECT TOP ${n} * FROM ${name};`;
  // JDBC: no row limit every database knows; the query tab caps the rows it fetches
  if (d === "generic") return `SELECT * FROM ${name};`;
  return `SELECT * FROM ${name} LIMIT ${n};`;
}

/** sql-formatter language for a dialect. */
export function formatterLanguage(d: Dialect): "postgresql" | "mysql" | "sqlite" | "transactsql" | "sql" {
  switch (d) {
    case "postgres": return "postgresql";
    case "mysql": return "mysql";
    case "sqlite": return "sqlite";
    case "mssql": return "transactsql";
    default: return "sql";
  }
}

export function isSqlKind(k: ConnectionKind): boolean {
  return k === "postgres" || k === "mysql" || k === "sqlite" || k === "mssql" || k === "jdbc";
}

/** Kinds that have a schema level beneath databases. */
export function hasSchemas(k: ConnectionKind): boolean {
  return k === "postgres" || k === "mssql" || k === "jdbc";
}

export function defaultSchema(k: ConnectionKind): string | null {
  if (k === "postgres") return "public";
  if (k === "mssql") return "dbo";
  return null;
}
