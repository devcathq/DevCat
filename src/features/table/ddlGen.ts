// DDL generation for the table designer: CREATE TABLE and ALTER TABLE (by diffing an original
// structure against an edited one) for postgres / mysql / sqlite / mssql.
// Pure functions — no React or api imports — so they can be unit tested in isolation.
//
// Matching rules for the edited structure: every column / index / foreign key may carry
// `origName`:  string => the original object it was edited from (renames are detected),
//              null   => brand new object,
//              undefined => matched by `name` against the original.
// Original objects not matched by any edited object are dropped.
import type { ColumnDef, Dialect, ForeignKeyDef, IndexDef, TableRef, TableStructure } from "../../lib/types";
import { qualifiedName, quoteIdent, sqlLiteral } from "./sqlDialect";

export type DesignColumn = ColumnDef & { origName?: string | null };
export type DesignIndex = IndexDef & { origName?: string | null };
export type DesignForeignKey = ForeignKeyDef & { origName?: string | null };
export interface DesignStructure extends TableStructure {
  columns: DesignColumn[];
  indexes: DesignIndex[];
  foreignKeys: DesignForeignKey[];
}

// ---------------------------------------------------------------------------------------------
// Type suggestions

const TYPES: Record<Exclude<Dialect, "none">, string[]> = {
  postgres: [
    "smallint", "integer", "bigint", "serial", "bigserial", "numeric", "real", "double precision", "money",
    "boolean", "text", "varchar", "char", "uuid", "date", "time", "timetz", "timestamp", "timestamptz", "interval",
    "json", "jsonb", "bytea", "inet", "cidr", "macaddr", "tsvector", "xml", "integer[]", "text[]",
  ],
  mysql: [
    "tinyint", "smallint", "mediumint", "int", "bigint", "decimal", "float", "double", "bit", "boolean",
    "char", "varchar", "tinytext", "text", "mediumtext", "longtext", "binary", "varbinary", "blob", "mediumblob",
    "longblob", "date", "time", "datetime", "timestamp", "year", "json", "enum", "set",
  ],
  sqlite: ["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC", "BOOLEAN", "DATE", "DATETIME", "VARCHAR"],
  generic: ["SMALLINT", "INTEGER", "BIGINT", "DECIMAL", "NUMERIC", "REAL", "DOUBLE PRECISION", "BOOLEAN", "CHAR", "VARCHAR", "CLOB", "BLOB", "DATE", "TIME", "TIMESTAMP"],
  mssql: [
    "bit", "tinyint", "smallint", "int", "bigint", "decimal", "numeric", "money", "smallmoney", "float", "real",
    "char", "varchar", "nchar", "nvarchar", "text", "ntext", "date", "time", "datetime", "datetime2",
    "datetimeoffset", "smalldatetime", "uniqueidentifier", "binary", "varbinary", "image", "xml",
  ],
};

export function typeSuggestions(d: Dialect): string[] {
  return d === "none" ? [] : TYPES[d];
}

export function defaultColumnType(d: Dialect): string {
  switch (d) {
    case "postgres": return "integer";
    case "mysql": return "int";
    case "sqlite": return "INTEGER";
    case "mssql": return "int";
    default: return "";
  }
}

/** Index methods offered per dialect. */
export function indexMethods(d: Dialect): string[] {
  switch (d) {
    case "postgres": return ["btree", "hash", "gin", "gist", "brin", "spgist"];
    case "mysql": return ["BTREE", "HASH"];
    case "mssql": return ["NONCLUSTERED", "CLUSTERED"];
    default: return [];
  }
}

export const FK_ACTIONS = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

// ---------------------------------------------------------------------------------------------
// Small helpers

const norm = (s: string | null | undefined) => (s ?? "").trim().toLowerCase().replace(/\s+/g, " ").replace(/\s*,\s*/g, ",");
const nz = (s: string | null | undefined) => {
  const t = (s ?? "").trim();
  return t === "" ? null : t;
};
const sameList = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);
const sameSet = (a: string[], b: string[]) => a.length === b.length && [...a].sort().join("\u0000") === [...b].sort().join("\u0000");
const fkAction = (s: string | null | undefined) => norm(s) || "no action";

function isSerialDefault(d: Dialect, c: ColumnDef) {
  return d === "postgres" && /^nextval\(/i.test(c.default ?? "");
}

/** Statements that are pure comments (informational, not executable). */
export function isCommentOnly(stmt: string) {
  return stmt.split("\n").every((l) => l.trim() === "" || l.trim().startsWith("--"));
}

/** Join statements into a preview script. */
export function toScript(stmts: string[]): string {
  return stmts.map((s) => (isCommentOnly(s) ? s : s + ";")).join("\n\n");
}

/** Script to execute: comment-only statements removed. */
export function toExecutableScript(stmts: string[]): string {
  return stmts.filter((s) => !isCommentOnly(s)).map((s) => s + ";").join("\n");
}

function q(d: Dialect, s: string) {
  return quoteIdent(d, s);
}

function colList(d: Dialect, cols: string[]) {
  return cols.map((c) => q(d, c)).join(", ");
}

/** Qualified reference to a table referenced by a foreign key. */
function refTableName(d: Dialect, fk: ForeignKeyDef): string {
  if (d === "mysql") return qualifiedName(d, { database: fk.refSchema, name: fk.refTable });
  if (d === "sqlite") return q(d, fk.refTable);
  return qualifiedName(d, { schema: fk.refSchema, name: fk.refTable });
}

function pkColumns(s: TableStructure): string[] {
  const idx = s.indexes.find((i) => i.primary);
  const fromCols = s.columns.filter((c) => c.isPrimaryKey).map((c) => c.name);
  return idx && idx.columns.length ? idx.columns : fromCols;
}

// ---------------------------------------------------------------------------------------------
// Column definitions

function columnDef(d: Dialect, c: ColumnDef, opts: { sqliteInlinePk?: boolean } = {}): string {
  const parts = [q(d, c.name)];
  const type = c.dataType.trim() || defaultColumnType(d);
  const def = nz(c.default);
  switch (d) {
    case "postgres": {
      parts.push(type);
      const identity = c.autoIncrement && !isSerialDefault(d, c) && !/serial$/i.test(type);
      if (identity) parts.push("GENERATED BY DEFAULT AS IDENTITY");
      if (!c.nullable) parts.push("NOT NULL");
      if (def && !identity) parts.push(`DEFAULT ${def}`);
      break;
    }
    case "mysql":
      parts.push(type);
      parts.push(c.nullable ? "NULL" : "NOT NULL");
      if (def) parts.push(`DEFAULT ${def}`);
      if (c.autoIncrement) parts.push("AUTO_INCREMENT");
      if (nz(c.comment)) parts.push(`COMMENT ${sqlLiteral(d, c.comment!)}`);
      break;
    case "sqlite":
      if (opts.sqliteInlinePk) {
        parts.push("INTEGER PRIMARY KEY AUTOINCREMENT");
      } else {
        parts.push(type);
        if (!c.nullable) parts.push("NOT NULL");
      }
      if (def) parts.push(`DEFAULT ${def}`);
      break;
    case "mssql":
      parts.push(type);
      if (c.autoIncrement) parts.push("IDENTITY(1,1)");
      parts.push(c.nullable ? "NULL" : "NOT NULL");
      if (def) parts.push(`DEFAULT ${def}`);
      break;
  }
  return parts.join(" ");
}

function indexCreate(d: Dialect, t: TableRef, idx: IndexDef): string {
  const method = nz(idx.method);
  const unique = idx.unique ? "UNIQUE " : "";
  const cols = colList(d, idx.columns);
  switch (d) {
    case "postgres":
      return `CREATE ${unique}INDEX ${q(d, idx.name)} ON ${qualifiedName(d, t)}${method ? ` USING ${method}` : ""} (${cols})`;
    case "mysql":
      return `CREATE ${unique}INDEX ${q(d, idx.name)} ON ${qualifiedName(d, t)} (${cols})${method && /^(btree|hash)$/i.test(method) ? ` USING ${method.toUpperCase()}` : ""}`;
    case "mssql": {
      const cl = method && /^(non)?clustered$/i.test(method) ? method.toUpperCase() + " " : "";
      return `CREATE ${unique}${cl}INDEX ${q(d, idx.name)} ON ${qualifiedName(d, t)} (${cols})`;
    }
    default:
      return `CREATE ${unique}INDEX ${q(d, idx.name)} ON ${qualifiedName(d, t)} (${cols})`;
  }
}

function indexDrop(d: Dialect, t: TableRef, name: string): string {
  switch (d) {
    case "postgres": return `DROP INDEX ${t.schema ? q(d, t.schema) + "." : ""}${q(d, name)}`;
    case "mysql": return `DROP INDEX ${q(d, name)} ON ${qualifiedName(d, t)}`;
    case "mssql": return `DROP INDEX ${q(d, name)} ON ${qualifiedName(d, t)}`;
    default: return `DROP INDEX ${q(d, name)}`;
  }
}

function fkClause(d: Dialect, fk: ForeignKeyDef): string {
  let s = `CONSTRAINT ${q(d, fk.name)} FOREIGN KEY (${colList(d, fk.columns)}) REFERENCES ${refTableName(d, fk)} (${colList(d, fk.refColumns)})`;
  if (nz(fk.onDelete) && fkAction(fk.onDelete) !== "no action") s += ` ON DELETE ${fk.onDelete!.toUpperCase()}`;
  if (nz(fk.onUpdate) && fkAction(fk.onUpdate) !== "no action") s += ` ON UPDATE ${fk.onUpdate!.toUpperCase()}`;
  return s;
}

function mssqlSchema(t: TableRef) {
  return t.schema ?? "dbo";
}

/** sp_addextendedproperty / sp_updateextendedproperty / sp_dropextendedproperty for MS_Description. */
function mssqlComment(t: TableRef, column: string | null, value: string | null, existed: boolean): string | null {
  const lit = (s: string) => `N'${s.replace(/'/g, "''")}'`;
  const target = `@level0type = N'SCHEMA', @level0name = ${lit(mssqlSchema(t))}, @level1type = N'TABLE', @level1name = ${lit(t.name)}` +
    (column ? `, @level2type = N'COLUMN', @level2name = ${lit(column)}` : "");
  if (value === null) return existed ? `EXEC sp_dropextendedproperty @name = N'MS_Description', ${target}` : null;
  const proc = existed ? "sp_updateextendedproperty" : "sp_addextendedproperty";
  return `EXEC ${proc} @name = N'MS_Description', @value = ${lit(value)}, ${target}`;
}

/** Drop the (unknown-named) default constraint of an MSSQL column. Single batch, no semicolons. */
function mssqlDropDefault(t: TableRef, column: string, n: number): string {
  const obj = `${mssqlSchema(t)}.${t.name}`.replace(/'/g, "''");
  const col = column.replace(/'/g, "''");
  const tbl = qualifiedName("mssql", t).replace(/'/g, "''");
  return [
    `DECLARE @df${n} sysname`,
    `SELECT @df${n} = dc.name FROM sys.default_constraints dc JOIN sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id WHERE dc.parent_object_id = OBJECT_ID(N'${obj}') AND c.name = N'${col}'`,
    `IF @df${n} IS NOT NULL EXEC(N'ALTER TABLE ${tbl} DROP CONSTRAINT [' + @df${n} + N']')`,
  ].join("\n");
}

/** Drop the primary key constraint of an MSSQL table when its name is unknown. */
function mssqlDropPk(t: TableRef): string {
  const obj = `${mssqlSchema(t)}.${t.name}`.replace(/'/g, "''");
  const tbl = qualifiedName("mssql", t).replace(/'/g, "''");
  return [
    "DECLARE @pk sysname",
    `SELECT @pk = name FROM sys.key_constraints WHERE type = 'PK' AND parent_object_id = OBJECT_ID(N'${obj}')`,
    `IF @pk IS NOT NULL EXEC(N'ALTER TABLE ${tbl} DROP CONSTRAINT [' + @pk + N']')`,
  ].join("\n");
}

// ---------------------------------------------------------------------------------------------
// CREATE TABLE

function createTableStatement(d: Dialect, t: TableRef, s: TableStructure): string {
  const pk = s.columns.filter((c) => c.isPrimaryKey).map((c) => c.name);
  const sqliteInline = d === "sqlite" && pk.length === 1 && s.columns.some((c) => c.isPrimaryKey && c.autoIncrement);
  const lines = s.columns.map((c) => "  " + columnDef(d, c, { sqliteInlinePk: sqliteInline && c.isPrimaryKey }));
  if (pk.length && !sqliteInline) {
    lines.push(`  ${d === "mssql" ? `CONSTRAINT ${q(d, `PK_${t.name}`)} ` : ""}PRIMARY KEY (${colList(d, pk)})`);
  }
  for (const fk of s.foreignKeys) lines.push("  " + fkClause(d, fk));
  let stmt = `CREATE TABLE ${qualifiedName(d, t)} (\n${lines.join(",\n")}\n)`;
  if (d === "mysql" && nz(s.comment)) stmt += ` COMMENT = ${sqlLiteral(d, s.comment!)}`;
  return stmt;
}

/** CREATE TABLE plus indexes and comments. `table.name` is the new table's name. */
export function generateCreateTable(d: Dialect, table: TableRef, s: TableStructure): string[] {
  if (d === "none") return [];
  if (!s.columns.length) return ["-- A table needs at least one column"];
  const out = [createTableStatement(d, table, s)];
  for (const idx of s.indexes) if (!idx.primary && idx.columns.length) out.push(indexCreate(d, table, idx));
  if (d === "postgres") {
    if (nz(s.comment)) out.push(`COMMENT ON TABLE ${qualifiedName(d, table)} IS ${sqlLiteral(d, s.comment!)}`);
    for (const c of s.columns) if (nz(c.comment)) out.push(`COMMENT ON COLUMN ${qualifiedName(d, table)}.${q(d, c.name)} IS ${sqlLiteral(d, c.comment!)}`);
  } else if (d === "mssql") {
    const tc = mssqlComment(table, null, nz(s.comment), false);
    if (tc) out.push(tc);
    for (const c of s.columns) {
      const cc = mssqlComment(table, c.name, nz(c.comment), false);
      if (cc) out.push(cc);
    }
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// ALTER TABLE

interface ColPair { col: DesignColumn; orig: ColumnDef | null }
interface ColDiff { type: boolean; nullable: boolean; default: boolean; autoIncrement: boolean; comment: boolean }

function matchByOrig<O extends { name: string }, E extends { name: string; origName?: string | null }>(orig: O[], edited: E[]) {
  const byName = new Map(orig.map((o) => [o.name, o]));
  const used = new Set<string>();
  const pairs = edited.map((e) => {
    const key = e.origName === null ? null : e.origName ?? e.name;
    const o = key != null && !used.has(key) ? byName.get(key) ?? null : null;
    if (o) used.add(o.name);
    return { e, o };
  });
  const removed = orig.filter((o) => !used.has(o.name));
  return { pairs, removed };
}

function diffColumn(d: Dialect, o: ColumnDef, c: ColumnDef): ColDiff {
  return {
    type: norm(o.dataType) !== norm(c.dataType),
    nullable: o.nullable !== c.nullable,
    default: (nz(o.default) ?? "") !== (nz(c.default) ?? ""),
    autoIncrement: o.autoIncrement !== c.autoIncrement,
    comment: d !== "sqlite" && (nz(o.comment) ?? "") !== (nz(c.comment) ?? ""),
  };
}
const anyDiff = (x: ColDiff) => x.type || x.nullable || x.default || x.autoIncrement || x.comment;

/** True when the edited column differs from its original (ignoring renames). */
export function columnChanged(d: Dialect, o: ColumnDef, c: ColumnDef): boolean {
  return o.name !== c.name || o.isPrimaryKey !== c.isPrimaryKey || anyDiff(diffColumn(d, o, c));
}

export function indexChanged(o: IndexDef, e: IndexDef, renames: Map<string, string> = new Map()): boolean {
  const mapped = o.columns.map((c) => renames.get(c) ?? c);
  const methodDiff = !!nz(o.method) && !!nz(e.method) && norm(o.method) !== norm(e.method);
  return !sameList(mapped, e.columns) || o.unique !== e.unique || methodDiff;
}

export function foreignKeyChanged(o: ForeignKeyDef, e: ForeignKeyDef, renames: Map<string, string> = new Map()): boolean {
  const mapped = o.columns.map((c) => renames.get(c) ?? c);
  const schemaDiff = !!nz(o.refSchema) && !!nz(e.refSchema) && o.refSchema !== e.refSchema;
  return !sameList(mapped, e.columns) || o.refTable !== e.refTable || !sameList(o.refColumns, e.refColumns) ||
    schemaDiff || fkAction(o.onDelete) !== fkAction(e.onDelete) || fkAction(o.onUpdate) !== fkAction(e.onUpdate);
}

/**
 * ALTER statements turning `original` into `edited`. `table` is the original table reference;
 * a different `edited.table.name` renames the table.
 */
export function generateAlterTable(d: Dialect, original: TableStructure, edited: DesignStructure, table: TableRef): string[] {
  // JDBC: every database alters tables its own way; the designer doesn't guess
  if (d === "none" || d === "generic") return [];
  const newName = nz(edited.table?.name) ?? table.name;
  const ref: TableRef = { ...table, name: newName };

  const { pairs: cp, removed: droppedCols } = matchByOrig(original.columns, edited.columns);
  const cols: ColPair[] = cp.map(({ e, o }) => ({ col: e, orig: o }));
  const renames = new Map<string, string>();
  for (const p of cols) if (p.orig) renames.set(p.orig.name, p.col.name);

  const origPk = pkColumns(original).map((c) => renames.get(c) ?? c);
  const newPk = edited.columns.filter((c) => c.isPrimaryKey).map((c) => c.name);
  const pkChanged = !sameSet(origPk, newPk);

  const origIdx = original.indexes.filter((i) => !i.primary);
  const editIdx = edited.indexes.filter((i) => !i.primary);
  const ix = matchByOrig(origIdx, editIdx);
  const fx = matchByOrig(original.foreignKeys, edited.foreignKeys);

  const ctx: AlterCtx = { d, original, edited, table, ref, newName, cols, droppedCols, renames, origPk, newPk, pkChanged, ix, fx };
  switch (d) {
    case "postgres": return alterPostgres(ctx);
    case "mysql": return alterMysql(ctx);
    case "mssql": return alterMssql(ctx);
    case "sqlite": return alterSqlite(ctx);
  }
}

interface AlterCtx {
  d: Dialect;
  original: TableStructure;
  edited: DesignStructure;
  table: TableRef;
  ref: TableRef;
  newName: string;
  cols: ColPair[];
  droppedCols: ColumnDef[];
  renames: Map<string, string>;
  origPk: string[];
  newPk: string[];
  pkChanged: boolean;
  ix: { pairs: { e: DesignIndex; o: IndexDef | null }[]; removed: IndexDef[] };
  fx: { pairs: { e: DesignForeignKey; o: ForeignKeyDef | null }[]; removed: ForeignKeyDef[] };
}

/** Index pairs split into: renamed only / dropped+recreated / created. */
function indexPlan(c: AlterCtx) {
  const rename: { from: string; to: string }[] = [];
  const drop: string[] = c.ix.removed.map((i) => i.name);
  const create: IndexDef[] = [];
  for (const { e, o } of c.ix.pairs) {
    if (!e.columns.length) continue;
    if (!o) create.push(e);
    else if (indexChanged(o, e, c.renames)) { drop.push(o.name); create.push(e); }
    else if (o.name !== e.name) rename.push({ from: o.name, to: e.name });
  }
  return { rename, drop, create };
}

function fkPlan(c: AlterCtx) {
  const rename: { from: string; to: string }[] = [];
  const drop: string[] = c.fx.removed.map((f) => f.name);
  const create: ForeignKeyDef[] = [];
  for (const { e, o } of c.fx.pairs) {
    if (!e.columns.length || !e.refTable) continue;
    if (!o) create.push(e);
    else if (foreignKeyChanged(o, e, c.renames)) { drop.push(o.name); create.push(e); }
    else if (o.name !== e.name) rename.push({ from: o.name, to: e.name });
  }
  return { rename, drop, create };
}

function alterPostgres(c: AlterCtx): string[] {
  const { d, ref } = c;
  const out: string[] = [];
  const T = () => qualifiedName(d, ref);
  if (c.newName !== c.table.name) out.push(`ALTER TABLE ${qualifiedName(d, c.table)} RENAME TO ${q(d, c.newName)}`);

  const fk = fkPlan(c);
  const ix = indexPlan(c);
  for (const n of fk.drop) out.push(`ALTER TABLE ${T()} DROP CONSTRAINT ${q(d, n)}`);
  for (const n of ix.drop) out.push(indexDrop(d, ref, n));
  if (c.pkChanged && c.origPk.length) {
    const name = c.original.indexes.find((i) => i.primary)?.name ?? `${c.table.name}_pkey`;
    out.push(`ALTER TABLE ${T()} DROP CONSTRAINT ${q(d, name)}`);
  }
  for (const p of c.cols) if (p.orig && p.orig.name !== p.col.name) out.push(`ALTER TABLE ${T()} RENAME COLUMN ${q(d, p.orig.name)} TO ${q(d, p.col.name)}`);
  for (const col of c.droppedCols) out.push(`ALTER TABLE ${T()} DROP COLUMN ${q(d, col.name)}`);
  for (const { col, orig } of c.cols) {
    if (!orig) continue;
    const x = diffColumn(d, orig, col);
    const A = `ALTER TABLE ${T()} ALTER COLUMN ${q(d, col.name)}`;
    if (x.type) out.push(`${A} TYPE ${col.dataType} USING ${q(d, col.name)}::${col.dataType}`);
    if (x.autoIncrement && !col.autoIncrement) out.push(`${A} DROP IDENTITY IF EXISTS`);
    if (x.nullable) out.push(`${A} ${col.nullable ? "DROP" : "SET"} NOT NULL`);
    if (x.default) out.push(nz(col.default) ? `${A} SET DEFAULT ${col.default}` : `${A} DROP DEFAULT`);
    if (x.autoIncrement && col.autoIncrement) out.push(`${A} ADD GENERATED BY DEFAULT AS IDENTITY`);
  }
  for (const { col, orig } of c.cols) if (!orig) out.push(`ALTER TABLE ${T()} ADD COLUMN ${columnDef(d, col)}`);
  if (c.pkChanged && c.newPk.length) out.push(`ALTER TABLE ${T()} ADD PRIMARY KEY (${colList(d, c.newPk)})`);
  for (const r of ix.rename) out.push(`ALTER INDEX ${ref.schema ? q(d, ref.schema) + "." : ""}${q(d, r.from)} RENAME TO ${q(d, r.to)}`);
  for (const i of ix.create) out.push(indexCreate(d, ref, i));
  for (const r of fk.rename) out.push(`ALTER TABLE ${T()} RENAME CONSTRAINT ${q(d, r.from)} TO ${q(d, r.to)}`);
  for (const f of fk.create) out.push(`ALTER TABLE ${T()} ADD ${fkClause(d, f)}`);

  if ((nz(c.original.comment) ?? "") !== (nz(c.edited.comment) ?? "")) {
    out.push(`COMMENT ON TABLE ${T()} IS ${nz(c.edited.comment) ? sqlLiteral(d, c.edited.comment!) : "NULL"}`);
  }
  for (const { col, orig } of c.cols) {
    const before = nz(orig?.comment) ?? "";
    const after = nz(col.comment) ?? "";
    if (before !== after) out.push(`COMMENT ON COLUMN ${T()}.${q(d, col.name)} IS ${after ? sqlLiteral(d, after) : "NULL"}`);
  }
  return out;
}

function alterMysql(c: AlterCtx): string[] {
  const { d, ref } = c;
  const out: string[] = [];
  if (c.newName !== c.table.name) out.push(`ALTER TABLE ${qualifiedName(d, c.table)} RENAME TO ${qualifiedName(d, ref)}`);
  const T = qualifiedName(d, ref);
  const fk = fkPlan(c);
  const ix = indexPlan(c);
  // FK drops go first in their own statement: MySQL refuses to drop an index still used by a FK.
  if (fk.drop.length || fk.rename.length) {
    out.push(`ALTER TABLE ${T}\n  ${[...fk.drop, ...fk.rename.map((r) => r.from)].map((n) => `DROP FOREIGN KEY ${q(d, n)}`).join(",\n  ")}`);
  }
  // Everything else in one ALTER so AUTO_INCREMENT + PRIMARY KEY changes validate together.
  const clauses: string[] = [];
  for (const n of ix.drop) clauses.push(`DROP INDEX ${q(d, n)}`);
  if (c.pkChanged && c.origPk.length) clauses.push("DROP PRIMARY KEY");
  for (const col of c.droppedCols) clauses.push(`DROP COLUMN ${q(d, col.name)}`);
  c.cols.forEach(({ col, orig }, i) => {
    if (orig && orig.name !== col.name) clauses.push(`CHANGE COLUMN ${q(d, orig.name)} ${columnDef(d, col)}`);
    else if (orig && anyDiff(diffColumn(d, orig, col))) clauses.push(`MODIFY COLUMN ${columnDef(d, col)}`);
    else if (!orig) {
      const prev = c.cols[i - 1]?.col.name;
      clauses.push(`ADD COLUMN ${columnDef(d, col)}${prev ? ` AFTER ${q(d, prev)}` : " FIRST"}`);
    }
  });
  if (c.pkChanged && c.newPk.length) clauses.push(`ADD PRIMARY KEY (${colList(d, c.newPk)})`);
  for (const r of ix.rename) clauses.push(`RENAME INDEX ${q(d, r.from)} TO ${q(d, r.to)}`);
  for (const i of ix.create) {
    const method = nz(i.method) && /^(btree|hash)$/i.test(i.method!) ? ` USING ${i.method!.toUpperCase()}` : "";
    clauses.push(`ADD ${i.unique ? "UNIQUE " : ""}INDEX ${q(d, i.name)} (${colList(d, i.columns)})${method}`);
  }
  if ((nz(c.original.comment) ?? "") !== (nz(c.edited.comment) ?? "")) clauses.push(`COMMENT = ${sqlLiteral(d, nz(c.edited.comment) ?? "")}`);
  if (clauses.length) out.push(`ALTER TABLE ${T}\n  ${clauses.join(",\n  ")}`);
  const fkAdds = [...fk.create, ...fk.rename.map((r) => c.fx.pairs.find((p) => p.e.name === r.to)!.e)];
  if (fkAdds.length) out.push(`ALTER TABLE ${T}\n  ${fkAdds.map((f) => `ADD ${fkClause(d, f)}`).join(",\n  ")}`);
  return out;
}

function alterMssql(c: AlterCtx): string[] {
  const { d, ref } = c;
  const out: string[] = [];
  const lit = (s: string) => `N'${s.replace(/'/g, "''")}'`;
  const schema = mssqlSchema(ref);
  let dfVar = 0; // DECLAREd variables must be unique within the batch
  const dropDefault = (col: string) => mssqlDropDefault(ref, col, ++dfVar);
  if (c.newName !== c.table.name) out.push(`EXEC sp_rename ${lit(`${schema}.${c.table.name}`)}, ${lit(c.newName)}`);
  const T = qualifiedName(d, ref);
  const fk = fkPlan(c);
  const ix = indexPlan(c);
  for (const n of fk.drop) out.push(`ALTER TABLE ${T} DROP CONSTRAINT ${q(d, n)}`);
  for (const n of ix.drop) out.push(indexDrop(d, ref, n));
  if (c.pkChanged && c.origPk.length) {
    const name = c.original.indexes.find((i) => i.primary)?.name;
    out.push(name ? `ALTER TABLE ${T} DROP CONSTRAINT ${q(d, name)}` : mssqlDropPk(ref));
  }
  for (const p of c.cols) {
    if (p.orig && p.orig.name !== p.col.name) out.push(`EXEC sp_rename ${lit(`${schema}.${c.newName}.${p.orig.name}`)}, ${lit(p.col.name)}, N'COLUMN'`);
  }
  for (const col of c.droppedCols) {
    if (nz(col.default)) out.push(dropDefault(col.name));
    out.push(`ALTER TABLE ${T} DROP COLUMN ${q(d, col.name)}`);
  }
  for (const { col, orig } of c.cols) {
    if (!orig) continue;
    const x = diffColumn(d, orig, col);
    if (x.type || x.nullable) out.push(`ALTER TABLE ${T} ALTER COLUMN ${q(d, col.name)} ${col.dataType} ${col.nullable ? "NULL" : "NOT NULL"}`);
    if (x.default) {
      if (nz(orig.default)) out.push(dropDefault(col.name));
      if (nz(col.default)) out.push(`ALTER TABLE ${T} ADD CONSTRAINT ${q(d, `DF_${c.newName}_${col.name}`)} DEFAULT ${col.default} FOR ${q(d, col.name)}`);
    }
    if (x.autoIncrement) out.push(`-- IDENTITY cannot be added to or removed from existing column ${q(d, col.name)}; recreate the column instead`);
  }
  for (const { col, orig } of c.cols) if (!orig) out.push(`ALTER TABLE ${T} ADD ${columnDef(d, col)}`);
  if (c.pkChanged && c.newPk.length) out.push(`ALTER TABLE ${T} ADD CONSTRAINT ${q(d, `PK_${c.newName}`)} PRIMARY KEY (${colList(d, c.newPk)})`);
  for (const r of ix.rename) out.push(`EXEC sp_rename ${lit(`${schema}.${c.newName}.${r.from}`)}, ${lit(r.to)}, N'INDEX'`);
  for (const i of ix.create) out.push(indexCreate(d, ref, i));
  for (const r of fk.rename) out.push(`EXEC sp_rename ${lit(`${schema}.${r.from}`)}, ${lit(r.to)}, N'OBJECT'`);
  for (const f of fk.create) out.push(`ALTER TABLE ${T} ADD ${fkClause(d, f)}`);

  const tc = (nz(c.original.comment) ?? "") !== (nz(c.edited.comment) ?? "")
    ? mssqlComment(ref, null, nz(c.edited.comment), !!nz(c.original.comment)) : null;
  if (tc) out.push(tc);
  for (const { col, orig } of c.cols) {
    const before = nz(orig?.comment);
    const after = nz(col.comment);
    if ((before ?? "") === (after ?? "")) continue;
    const s = mssqlComment(ref, col.name, after, !!before);
    if (s) out.push(s);
  }
  return out;
}

function alterSqlite(c: AlterCtx): string[] {
  const { d, ref } = c;
  const fkChanged = c.fx.removed.length > 0 || c.fx.pairs.some(({ e, o }) => !o || foreignKeyChanged(o, e, c.renames) || o.name !== e.name);
  const needsRebuild =
    c.pkChanged || fkChanged ||
    c.cols.some(({ col, orig }) => (orig ? anyDiff(diffColumn(d, orig, col)) : col.isPrimaryKey || col.autoIncrement || (!col.nullable && !nz(col.default)))) ||
    c.droppedCols.some((col) => col.isPrimaryKey);

  if (needsRebuild) return sqliteRebuild(c);

  const out: string[] = [];
  if (c.newName !== c.table.name) out.push(`ALTER TABLE ${qualifiedName(d, c.table)} RENAME TO ${q(d, c.newName)}`);
  const T = qualifiedName(d, ref);
  const ix = indexPlan(c);
  for (const n of ix.drop) out.push(indexDrop(d, ref, n));
  for (const r of ix.rename) out.push(indexDrop(d, ref, r.from));
  for (const p of c.cols) if (p.orig && p.orig.name !== p.col.name) out.push(`ALTER TABLE ${T} RENAME COLUMN ${q(d, p.orig.name)} TO ${q(d, p.col.name)}`);
  for (const col of c.droppedCols) out.push(`ALTER TABLE ${T} DROP COLUMN ${q(d, col.name)}`);
  for (const { col, orig } of c.cols) if (!orig) out.push(`ALTER TABLE ${T} ADD COLUMN ${columnDef(d, col)}`);
  for (const i of ix.create) out.push(indexCreate(d, ref, i));
  for (const r of ix.rename) {
    const e = c.ix.pairs.find((p) => p.e.name === r.to)!.e;
    out.push(indexCreate(d, ref, e));
  }
  return out;
}

/** SQLite can't alter column definitions/constraints: recreate the table and copy the data. */
function sqliteRebuild(c: AlterCtx): string[] {
  const { d, ref } = c;
  const tmp: TableRef = { ...ref, name: `_${c.newName}_new` };
  const out = [
    "-- SQLite cannot alter column definitions or constraints in place: the table is rebuilt and data copied",
    "PRAGMA foreign_keys = OFF",
    createTableStatement(d, tmp, c.edited),
  ];
  const kept = c.cols.filter((p) => p.orig);
  if (kept.length) {
    out.push(
      `INSERT INTO ${qualifiedName(d, tmp)} (${colList(d, kept.map((p) => p.col.name))})\n` +
      `  SELECT ${colList(d, kept.map((p) => p.orig!.name))} FROM ${qualifiedName(d, c.table)}`,
    );
  }
  out.push(`DROP TABLE ${qualifiedName(d, c.table)}`);
  out.push(`ALTER TABLE ${qualifiedName(d, tmp)} RENAME TO ${q(d, c.newName)}`);
  for (const i of c.edited.indexes) if (!i.primary && i.columns.length) out.push(indexCreate(d, ref, i));
  out.push("PRAGMA foreign_keys = ON");
  return out;
}
