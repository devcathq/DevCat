// Dialect helpers shared by the sync-script and data-compare generators:
// identifier quoting, literal rendering and cross-dialect type mapping.
import type { CellValue, Dialect } from "../../lib/types";

export type SqlDialect = Exclude<Dialect, "none">;

export function quoteIdent(d: SqlDialect, ident: string): string {
  switch (d) {
    case "mysql": return "`" + ident.replace(/`/g, "``") + "`";
    case "mssql": return "[" + ident.replace(/]/g, "]]") + "]";
    default: return '"' + ident.replace(/"/g, '""') + '"';
  }
}

/** Qualified table name. Schema is ignored for MySQL/SQLite (database scoped by connection). */
export function qualify(d: SqlDialect, name: string, schema?: string | null): string {
  if (schema && (d === "postgres" || d === "mssql")) return `${quoteIdent(d, schema)}.${quoteIdent(d, name)}`;
  return quoteIdent(d, name);
}

export function literal(d: SqlDialect, v: CellValue | undefined): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "boolean") return d === "postgres" ? (v ? "TRUE" : "FALSE") : v ? "1" : "0";
  if (typeof v === "number") return Number.isFinite(v) ? String(v) : "NULL";
  const s = typeof v === "string" ? v : JSON.stringify(v);
  const esc = s.replace(/'/g, "''");
  if (d === "mysql") return `'${esc.replace(/\\/g, "\\\\")}'`;
  if (d === "mssql") return `N'${esc}'`;
  return `'${esc}'`;
}

/** Statement terminator / batch separator. */
export function terminator(d: SqlDialect): string {
  return d === "mssql" ? ";\nGO" : ";";
}

// ---------------------------------------------------------------------------
// Type normalization & mapping

interface ParsedType { base: string; args: string[]; array: boolean; unsigned: boolean }

export function parseType(raw: string): ParsedType {
  let t = raw.trim().toLowerCase().replace(/\s+/g, " ");
  const array = /\[\]$/.test(t);
  t = t.replace(/\[\]$/, "");
  const unsigned = /\bunsigned\b/.test(t);
  t = t.replace(/\bunsigned\b|\bzerofill\b/g, "").trim();
  const m = t.match(/^([^(]+?)\s*\(([^)]*)\)\s*(.*)$/);
  if (m) {
    const base = (m[1] + (m[3] ? " " + m[3] : "")).trim();
    return { base, args: m[2].split(",").map((a) => a.trim()).filter(Boolean), array, unsigned };
  }
  return { base: t, args: [], array, unsigned };
}

/** Canonical type families used to compare types across dialects. */
type Family =
  | "smallint" | "int" | "bigint" | "tinyint" | "decimal" | "real" | "double" | "bool"
  | "varchar" | "char" | "text" | "nvarchar" | "date" | "time" | "timestamp" | "timestamptz"
  | "json" | "uuid" | "binary" | "other";

const FAMILY: Record<string, Family> = {
  smallint: "smallint", int2: "smallint", smallserial: "smallint",
  int: "int", integer: "int", int4: "int", serial: "int", mediumint: "int",
  bigint: "bigint", int8: "bigint", bigserial: "bigint",
  tinyint: "tinyint",
  decimal: "decimal", numeric: "decimal", money: "decimal", smallmoney: "decimal",
  real: "real", float4: "real",
  double: "double", "double precision": "double", float8: "double", float: "double",
  bool: "bool", boolean: "bool", bit: "bool",
  varchar: "varchar", "character varying": "varchar", varchar2: "varchar",
  nvarchar: "nvarchar", "national character varying": "nvarchar",
  char: "char", character: "char", nchar: "char", bpchar: "char",
  text: "text", tinytext: "text", mediumtext: "text", longtext: "text", ntext: "text", clob: "text", citext: "text",
  date: "date",
  time: "time", "time without time zone": "time",
  timestamp: "timestamp", "timestamp without time zone": "timestamp", datetime: "timestamp", datetime2: "timestamp", smalldatetime: "timestamp",
  timestamptz: "timestamptz", "timestamp with time zone": "timestamptz", datetimeoffset: "timestamptz",
  json: "json", jsonb: "json",
  uuid: "uuid", uniqueidentifier: "uuid",
  bytea: "binary", blob: "binary", tinyblob: "binary", mediumblob: "binary", longblob: "binary",
  binary: "binary", varbinary: "binary", image: "binary",
};

function familyOf(p: ParsedType): Family {
  return FAMILY[p.base] ?? "other";
}

/** Normalize a type for same-dialect comparison (aliases + whitespace + case). */
export function normalizeType(raw: string, opts: { ignoreSize?: boolean } = {}): string {
  const p = parseType(raw);
  const fam = familyOf(p);
  const base = fam === "other" ? p.base : fam;
  // Integer display widths (int(11)) are noise.
  const args = opts.ignoreSize || ["int", "bigint", "smallint", "tinyint"].includes(fam) ? [] : p.args;
  return `${base}${args.length ? `(${args.join(",")})` : ""}${p.array ? "[]" : ""}${p.unsigned ? " unsigned" : ""}`;
}

export function typesEqual(a: string, b: string, crossDialect: boolean, ignoreSize = false): boolean {
  if (!crossDialect) return normalizeType(a, { ignoreSize }) === normalizeType(b, { ignoreSize });
  const pa = parseType(a), pb = parseType(b);
  const fa = familyOf(pa), fb = familyOf(pb);
  if (fa === "other" || fb === "other") return normalizeType(a, { ignoreSize }) === normalizeType(b, { ignoreSize });
  const loose = (f: Family) => (f === "nvarchar" ? "varchar" : f);
  if (loose(fa) !== loose(fb)) return false;
  if (ignoreSize || fa === "text" || fa === "json" || fa === "bool") return true;
  return pa.args.join(",") === pb.args.join(",");
}

/** Map a column type from one dialect into another. Same-dialect: returned unchanged. */
export function mapType(raw: string, from: SqlDialect, to: SqlDialect): string {
  if (from === to) return raw;
  const p = parseType(raw);
  const fam = familyOf(p);
  const n = p.args[0];
  const sized = (base: string, def?: string) => (n ? `${base}(${p.args.join(",")})` : def ?? base);
  switch (to) {
    case "generic":
      switch (fam) {
        case "tinyint": case "smallint": return "SMALLINT";
        case "int": return "INTEGER";
        case "bigint": return "BIGINT";
        case "decimal": return sized("DECIMAL");
        case "real": return "REAL";
        case "double": return "DOUBLE PRECISION";
        case "bool": return "BOOLEAN";
        case "varchar": case "nvarchar": return n && n !== "max" ? `VARCHAR(${n})` : "CLOB";
        case "char": return sized("CHAR");
        case "date": return "DATE";
        case "time": return "TIME";
        case "timestamp": return "TIMESTAMP";
        case "timestamptz": return "TIMESTAMP WITH TIME ZONE";
        case "uuid": return "CHAR(36)";
        case "binary": return "BLOB";
        default: return "CLOB";
      }
    case "postgres":
      switch (fam) {
        case "tinyint": case "smallint": return "smallint";
        case "int": return "integer";
        case "bigint": return "bigint";
        case "decimal": return sized("numeric");
        case "real": return "real";
        case "double": return "double precision";
        case "bool": return "boolean";
        case "varchar": case "nvarchar": return n && n !== "max" ? `varchar(${n})` : "text";
        case "char": return sized("char");
        case "text": return "text";
        case "date": return "date";
        case "time": return "time";
        case "timestamp": return "timestamp";
        case "timestamptz": return "timestamptz";
        case "json": return "jsonb";
        case "uuid": return "uuid";
        case "binary": return "bytea";
        default: return "text";
      }
    case "mysql":
      switch (fam) {
        case "tinyint": return "tinyint";
        case "smallint": return "smallint";
        case "int": return "int";
        case "bigint": return "bigint";
        case "decimal": return sized("decimal", "decimal(18,4)");
        case "real": return "float";
        case "double": return "double";
        case "bool": return "tinyint(1)";
        case "varchar": case "nvarchar": return n && n !== "max" ? `varchar(${n})` : "longtext";
        case "char": return sized("char");
        case "text": return "longtext";
        case "date": return "date";
        case "time": return "time";
        case "timestamp": case "timestamptz": return "datetime(6)";
        case "json": return "json";
        case "uuid": return "char(36)";
        case "binary": return "longblob";
        default: return "longtext";
      }
    case "mssql":
      switch (fam) {
        case "tinyint": return "tinyint";
        case "smallint": return "smallint";
        case "int": return "int";
        case "bigint": return "bigint";
        case "decimal": return sized("decimal", "decimal(18,4)");
        case "real": return "real";
        case "double": return "float";
        case "bool": return "bit";
        case "varchar": return n ? `varchar(${n})` : "varchar(max)";
        case "nvarchar": return n ? `nvarchar(${n})` : "nvarchar(max)";
        case "char": return sized("nchar");
        case "text": return "nvarchar(max)";
        case "date": return "date";
        case "time": return "time";
        case "timestamp": return "datetime2";
        case "timestamptz": return "datetimeoffset";
        case "json": return "nvarchar(max)";
        case "uuid": return "uniqueidentifier";
        case "binary": return "varbinary(max)";
        default: return "nvarchar(max)";
      }
    case "sqlite":
      switch (fam) {
        case "tinyint": case "smallint": case "int": case "bigint": case "bool": return "INTEGER";
        case "decimal": return "NUMERIC";
        case "real": case "double": return "REAL";
        case "binary": return "BLOB";
        default: return "TEXT";
      }
  }
}

/** Default expression mapping — conservative: drops casts and translates well known functions. */
export function mapDefault(raw: string | null | undefined, from: SqlDialect, to: SqlDialect): string | null {
  if (raw === null || raw === undefined || raw.trim() === "") return null;
  let d = raw.trim();
  // Postgres "nextval(...)" defaults belong to serial/identity — never port them.
  if (/^nextval\(/i.test(d)) return null;
  if (from === to) return d;
  d = d.replace(/::[\w\s"]+(\[\])?/g, "");
  // MSSQL wraps defaults in parentheses: ((0)), (getdate())
  while (/^\((.*)\)$/.test(d) && from === "mssql") d = d.slice(1, -1);
  const lower = d.toLowerCase();
  const nowish = ["now()", "current_timestamp", "current_timestamp()", "getdate()", "sysdatetime()", "localtimestamp"];
  if (nowish.includes(lower)) return to === "mssql" ? "GETDATE()" : "CURRENT_TIMESTAMP";
  if (lower === "true" || lower === "false") {
    const b = lower === "true";
    return to === "postgres" ? (b ? "TRUE" : "FALSE") : b ? "1" : "0";
  }
  return d;
}

/** Normalize a default expression for comparison. */
export function normalizeDefault(raw: string | null | undefined): string {
  if (raw === null || raw === undefined) return "";
  let d = raw.trim();
  while (/^\((.*)\)$/.test(d)) d = d.slice(1, -1).trim();
  d = d.replace(/::[\w\s"]+(\[\])?/g, "");
  return d.toLowerCase();
}
