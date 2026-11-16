//! Cross-dialect DDL generation (CREATE TABLE etc.) from `TableStructure`.
//! Used by backup, data transfer and schema compare.
//!
//! Types are handled in two steps: `parse_type` turns a dialect specific type
//! name into a neutral [`Ty`], `render_type` prints it for the target dialect.
//! `convert_structure` applies that (plus default/identity translation) to a
//! whole table so `create_table_sql` can render it for another database.
use super::sql_common::{literal, quote_ident};
use super::{ColumnDef, Dialect, ForeignKeyDef, IndexDef, TableRef, TableStructure};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Dialect neutral column type.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    TinyInt { unsigned: bool },
    SmallInt,
    Int,
    BigInt,
    /// (precision, scale)
    Decimal(Option<(u32, Option<u32>)>),
    Money,
    Real,
    Double,
    Bool,
    Char(Option<u32>),
    /// `None` = unbounded / `max`
    Varchar(Option<u32>),
    Text,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Json,
    Binary,
    Uuid,
    Xml,
    Array,
    Enum,
    Other(String),
}

/// Coarse classification used to coerce JSON values (import, transfer, SQL export).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    Integer,
    Float,
    Decimal,
    Bool,
    Text,
    Temporal,
    Json,
    Binary,
    Uuid,
    Other,
}

/// Parse a type name as reported by a driver of dialect `from`.
/// Returns the neutral type and whether it implies auto-increment (`serial`).
pub fn parse_type(from: Dialect, raw: &str) -> (Ty, bool) {
    let s = raw.trim().to_ascii_lowercase();
    if s.ends_with("[]") || (from == Dialect::Postgres && s.starts_with('_')) || s.starts_with("array") {
        return (Ty::Array, false);
    }
    let (base, args) = match (s.find('('), s.rfind(')')) {
        (Some(a), Some(b)) if b > a => (format!("{} {}", &s[..a], &s[b + 1..]), s[a + 1..b].to_string()),
        _ => (s.clone(), String::new()),
    };
    let unsigned = base.contains("unsigned");
    let base = base
        .split_whitespace()
        .filter(|w| !matches!(*w, "unsigned" | "signed" | "zerofill"))
        .collect::<Vec<_>>()
        .join(" ");
    let nums: Vec<Option<u32>> =
        if args.trim().is_empty() { vec![] } else { args.split(',').map(|a| a.trim().parse().ok()).collect() };
    let n0 = nums.first().copied().flatten();
    let n1 = nums.get(1).copied().flatten();
    let is_max = args.trim() == "max";

    if from == Dialect::None {
        // MongoDB inferred BSON type names
        let ty = match base.as_str() {
            "string" | "symbol" => Ty::Text,
            "int" | "int32" => Ty::Int,
            "long" | "int64" => Ty::BigInt,
            "double" => Ty::Double,
            "decimal" | "decimal128" => Ty::Decimal(None),
            "bool" | "boolean" => Ty::Bool,
            "date" | "timestamp" => Ty::Timestamp,
            "objectid" => Ty::Varchar(Some(24)),
            "object" | "array" | "document" => Ty::Json,
            "bindata" | "binary" => Ty::Binary,
            "uuid" => Ty::Uuid,
            _ => Ty::Text,
        };
        return (ty, false);
    }

    let ty = match base.as_str() {
        "serial" | "serial4" => return (Ty::Int, true),
        "bigserial" | "serial8" => return (Ty::BigInt, true),
        "smallserial" | "serial2" => return (Ty::SmallInt, true),
        "tinyint" | "int1" => {
            if from == Dialect::Mysql && n0 == Some(1) {
                Ty::Bool
            } else {
                Ty::TinyInt { unsigned: unsigned || from == Dialect::Mssql }
            }
        }
        "bool" | "boolean" => Ty::Bool,
        "bit" => {
            if n0.unwrap_or(1) == 1 { Ty::Bool } else { Ty::Binary }
        }
        "smallint" | "int2" | "year" => {
            if unsigned { Ty::Int } else { Ty::SmallInt }
        }
        "mediumint" => Ty::Int,
        "int" | "integer" | "int4" => {
            if unsigned || (from == Dialect::Sqlite && base == "integer") { Ty::BigInt } else { Ty::Int }
        }
        "bigint" | "int8" => {
            if unsigned { Ty::Decimal(Some((20, Some(0)))) } else { Ty::BigInt }
        }
        "decimal" | "numeric" | "dec" | "number" | "fixed" => Ty::Decimal(n0.map(|p| (p, n1))),
        "money" | "smallmoney" => Ty::Money,
        "real" | "float4" => Ty::Real,
        "float" => match n0 {
            Some(p) if p <= 24 => Ty::Real,
            Some(_) => Ty::Double,
            None if from == Dialect::Mysql => Ty::Real,
            None => Ty::Double,
        },
        "double" | "double precision" | "float8" => Ty::Double,
        "char" | "character" | "nchar" | "bpchar" | "national char" | "national character" => Ty::Char(n0),
        "varchar" | "character varying" | "nvarchar" | "varchar2" | "nvarchar2" | "national varchar"
        | "national character varying" | "varying character" => Ty::Varchar(if is_max { None } else { n0 }),
        "sysname" => Ty::Varchar(Some(128)),
        "text" | "tinytext" | "mediumtext" | "longtext" | "ntext" | "clob" | "citext" | "name" | "string" => Ty::Text,
        "date" => Ty::Date,
        "time" | "time without time zone" | "time with time zone" | "timetz" => Ty::Time,
        // MSSQL `timestamp` is a rowversion, not a date
        "timestamp" if from == Dialect::Mssql => Ty::Binary,
        "timestamp" | "datetime" | "datetime2" | "smalldatetime" | "timestamp without time zone" => Ty::Timestamp,
        "timestamptz" | "timestamp with time zone" | "datetimeoffset" => Ty::TimestampTz,
        "interval" => Ty::Interval,
        "json" | "jsonb" => Ty::Json,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" | "image" | "rowversion" => {
            Ty::Binary
        }
        "uuid" | "uniqueidentifier" => Ty::Uuid,
        "xml" => Ty::Xml,
        "enum" | "set" | "user-defined" => Ty::Enum,
        "inet" | "cidr" | "macaddr" | "macaddr8" => Ty::Varchar(Some(64)),
        _ => Ty::Other(raw.trim().to_string()),
    };
    (ty, false)
}

/// Print a neutral type for dialect `to`.
pub fn render_type(to: Dialect, ty: &Ty) -> String {
    match to {
        Dialect::Postgres => match ty {
            Ty::TinyInt { .. } | Ty::SmallInt => "smallint".into(),
            Ty::Int => "integer".into(),
            Ty::BigInt => "bigint".into(),
            Ty::Decimal(Some((p, Some(s)))) => format!("numeric({},{})", p.min(&1000), s),
            Ty::Decimal(Some((p, None))) => format!("numeric({})", p.min(&1000)),
            Ty::Decimal(None) => "numeric".into(),
            Ty::Money => "numeric(19,4)".into(),
            Ty::Real => "real".into(),
            Ty::Double => "double precision".into(),
            Ty::Bool => "boolean".into(),
            Ty::Char(n) => format!("char({})", n.unwrap_or(1)),
            Ty::Varchar(Some(n)) => format!("varchar({n})"),
            Ty::Varchar(None) | Ty::Text | Ty::Enum | Ty::Other(_) => "text".into(),
            Ty::Date => "date".into(),
            Ty::Time => "time".into(),
            Ty::Timestamp => "timestamp".into(),
            Ty::TimestampTz => "timestamptz".into(),
            Ty::Interval => "interval".into(),
            Ty::Json | Ty::Array => "jsonb".into(),
            Ty::Binary => "bytea".into(),
            Ty::Uuid => "uuid".into(),
            Ty::Xml => "xml".into(),
        },
        Dialect::Mysql => match ty {
            Ty::TinyInt { unsigned: true } => "tinyint unsigned".into(),
            Ty::TinyInt { unsigned: false } => "tinyint".into(),
            Ty::SmallInt => "smallint".into(),
            Ty::Int => "int".into(),
            Ty::BigInt => "bigint".into(),
            Ty::Decimal(Some((p, s))) => {
                let p = (*p).min(65);
                format!("decimal({},{})", p, s.unwrap_or(0).min(30).min(p))
            }
            Ty::Decimal(None) => "decimal(65,30)".into(),
            Ty::Money => "decimal(19,4)".into(),
            Ty::Real => "float".into(),
            Ty::Double => "double".into(),
            Ty::Bool => "tinyint(1)".into(),
            Ty::Char(n) => match n.unwrap_or(1) {
                n if n <= 255 => format!("char({n})"),
                n => format!("varchar({n})"),
            },
            Ty::Varchar(Some(n)) if *n <= 16383 => format!("varchar({n})"),
            Ty::Varchar(_) | Ty::Text | Ty::Xml | Ty::Other(_) => "longtext".into(),
            Ty::Date => "date".into(),
            Ty::Time => "time(6)".into(),
            Ty::Timestamp | Ty::TimestampTz => "datetime(6)".into(),
            Ty::Interval => "varchar(64)".into(),
            Ty::Json | Ty::Array => "json".into(),
            Ty::Binary => "longblob".into(),
            Ty::Uuid => "char(36)".into(),
            Ty::Enum => "varchar(255)".into(),
        },
        Dialect::Mssql => match ty {
            Ty::TinyInt { unsigned: true } => "tinyint".into(),
            Ty::TinyInt { unsigned: false } | Ty::SmallInt => "smallint".into(),
            Ty::Int => "int".into(),
            Ty::BigInt => "bigint".into(),
            Ty::Decimal(Some((p, s))) => {
                let p = (*p).min(38);
                format!("decimal({},{})", p, s.unwrap_or(0).min(p))
            }
            Ty::Decimal(None) => "decimal(38,10)".into(),
            Ty::Money => "decimal(19,4)".into(),
            Ty::Real => "real".into(),
            Ty::Double => "float".into(),
            Ty::Bool => "bit".into(),
            Ty::Char(n) => match n.unwrap_or(1) {
                n if n <= 4000 => format!("nchar({n})"),
                _ => "nvarchar(max)".into(),
            },
            Ty::Varchar(Some(n)) if *n <= 4000 => format!("nvarchar({n})"),
            Ty::Varchar(_) | Ty::Text | Ty::Json | Ty::Array | Ty::Other(_) => "nvarchar(max)".into(),
            Ty::Date => "date".into(),
            Ty::Time => "time".into(),
            Ty::Timestamp => "datetime2".into(),
            Ty::TimestampTz => "datetimeoffset".into(),
            Ty::Interval => "nvarchar(64)".into(),
            Ty::Binary => "varbinary(max)".into(),
            Ty::Uuid => "uniqueidentifier".into(),
            Ty::Xml => "xml".into(),
            Ty::Enum => "nvarchar(255)".into(),
        },
        Dialect::Sqlite => match ty {
            Ty::TinyInt { .. } | Ty::SmallInt | Ty::Int | Ty::BigInt => "integer".into(),
            Ty::Decimal(Some((p, Some(s)))) => format!("numeric({p},{s})"),
            Ty::Decimal(Some((p, None))) => format!("numeric({p})"),
            Ty::Decimal(None) | Ty::Money => "numeric".into(),
            Ty::Real | Ty::Double => "real".into(),
            Ty::Bool => "boolean".into(),
            Ty::Char(n) => format!("char({})", n.unwrap_or(1)),
            Ty::Varchar(Some(n)) => format!("varchar({n})"),
            Ty::Date => "date".into(),
            Ty::Time => "time".into(),
            Ty::Timestamp | Ty::TimestampTz => "datetime".into(),
            Ty::Binary => "blob".into(),
            // JSON has NUMERIC affinity in SQLite; keep text affinity for documents.
            Ty::Varchar(None) | Ty::Text | Ty::Interval | Ty::Json | Ty::Array | Ty::Uuid | Ty::Xml | Ty::Enum
            | Ty::Other(_) => "text".into(),
        },
        Dialect::None => match ty {
            Ty::Other(s) => s.clone(),
            other => render_type(Dialect::Postgres, other),
        },
        // standard SQL names, for a database reached through JDBC
        Dialect::Generic => match ty {
            Ty::TinyInt { .. } | Ty::SmallInt => "SMALLINT".into(),
            Ty::Int => "INTEGER".into(),
            Ty::BigInt => "BIGINT".into(),
            Ty::Decimal(Some((p, Some(s)))) => format!("DECIMAL({p},{s})"),
            Ty::Decimal(Some((p, None))) => format!("DECIMAL({p})"),
            Ty::Decimal(None) => "DECIMAL".into(),
            Ty::Money => "DECIMAL(19,4)".into(),
            Ty::Real => "REAL".into(),
            Ty::Double => "DOUBLE PRECISION".into(),
            Ty::Bool => "BOOLEAN".into(),
            Ty::Char(n) => format!("CHAR({})", n.unwrap_or(1)),
            Ty::Varchar(Some(n)) => format!("VARCHAR({n})"),
            Ty::Date => "DATE".into(),
            Ty::Time => "TIME".into(),
            Ty::Timestamp => "TIMESTAMP".into(),
            Ty::TimestampTz => "TIMESTAMP WITH TIME ZONE".into(),
            Ty::Binary => "BLOB".into(),
            Ty::Uuid => "CHAR(36)".into(),
            Ty::Other(s) => s.clone(),
            Ty::Varchar(None) | Ty::Text | Ty::Interval | Ty::Json | Ty::Array | Ty::Xml | Ty::Enum => "CLOB".into(),
        },
    }
}

/// Map a column type between dialects. Same-dialect mapping returns the input unchanged.
#[allow(dead_code)] // public helper for schema compare / other callers
pub fn map_type(from: Dialect, to: Dialect, data_type: &str) -> String {
    if from == to || to == Dialect::None {
        return data_type.trim().to_string();
    }
    render_type(to, &parse_type(from, data_type).0)
}

pub fn type_class(d: Dialect, data_type: &str) -> TypeClass {
    match parse_type(d, data_type).0 {
        Ty::TinyInt { .. } | Ty::SmallInt | Ty::Int | Ty::BigInt => TypeClass::Integer,
        Ty::Real | Ty::Double => TypeClass::Float,
        Ty::Decimal(_) | Ty::Money => TypeClass::Decimal,
        Ty::Bool => TypeClass::Bool,
        Ty::Char(_) | Ty::Varchar(_) | Ty::Text | Ty::Enum | Ty::Xml => TypeClass::Text,
        Ty::Date | Ty::Time | Ty::Timestamp | Ty::TimestampTz | Ty::Interval => TypeClass::Temporal,
        Ty::Json | Ty::Array => TypeClass::Json,
        Ty::Binary => TypeClass::Binary,
        Ty::Uuid => TypeClass::Uuid,
        Ty::Other(_) => TypeClass::Other,
    }
}

fn is_integer_type(d: Dialect, data_type: &str) -> bool {
    matches!(parse_type(d, data_type).0, Ty::TinyInt { .. } | Ty::SmallInt | Ty::Int | Ty::BigInt)
}

fn is_serial_type(data_type: &str) -> bool {
    let t = data_type.trim().to_ascii_lowercase();
    matches!(t.as_str(), "serial" | "serial4" | "bigserial" | "serial8" | "smallserial" | "serial2")
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Fully qualified, quoted table name. Postgres ignores `database` (no cross-db
/// references); MySQL treats `database` (or `schema`) as the qualifier; SQLite
/// qualifies only with an attached schema other than `main`.
pub fn qualified_name(d: Dialect, t: &TableRef) -> String {
    let q = |s: &str| quote_ident(d, s);
    let nonempty = |o: &Option<String>| o.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    match d {
        Dialect::Postgres | Dialect::None | Dialect::Generic => match nonempty(&t.schema) {
            Some(s) => format!("{}.{}", q(&s), q(&t.name)),
            None => q(&t.name),
        },
        Dialect::Mysql => match nonempty(&t.database).or_else(|| nonempty(&t.schema)) {
            Some(db) => format!("{}.{}", q(&db), q(&t.name)),
            None => q(&t.name),
        },
        Dialect::Mssql => match (nonempty(&t.database), nonempty(&t.schema)) {
            (Some(db), Some(s)) => format!("{}.{}.{}", q(&db), q(&s), q(&t.name)),
            (Some(db), None) => format!("{}..{}", q(&db), q(&t.name)),
            (None, Some(s)) => format!("{}.{}", q(&s), q(&t.name)),
            (None, None) => q(&t.name),
        },
        Dialect::Sqlite => match nonempty(&t.schema).or_else(|| nonempty(&t.database)).filter(|s| s != "main") {
            Some(s) => format!("{}.{}", q(&s), q(&t.name)),
            None => q(&t.name),
        },
    }
}

fn col_list(d: Dialect, cols: &[String]) -> String {
    cols.iter().map(|c| quote_ident(d, c)).collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Strip one level of wrapping parentheses if the first `(` matches the last `)`.
fn strip_wrapping_parens(s: &str) -> Option<&str> {
    let s = s.trim();
    if !(s.starts_with('(') && s.ends_with(')')) {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '\'' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => {
                depth -= 1;
                if depth == 0 && i != s.len() - 1 {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(&s[1..s.len() - 1])
}

fn is_numeric_literal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    !s.is_empty() && s.parse::<f64>().is_ok() && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
}

/// Parse a single quoted SQL string literal (`'it''s'`, `N'x'`, `E'x'`) into its value.
fn parse_string_literal(s: &str) -> Option<String> {
    let s = s.trim();
    let body = s
        .strip_prefix("N'")
        .or_else(|| s.strip_prefix("E'"))
        .or_else(|| s.strip_prefix("_utf8mb4'"))
        .or_else(|| s.strip_prefix('\''))?
        .strip_suffix('\'')?;
    // reject things like 'a' || 'b'
    let mut chars = body.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
            } else {
                return None;
            }
        }
        out.push(c);
    }
    Some(out)
}

const NOW_FUNCS: &[&str] = &[
    "now()",
    "current_timestamp",
    "current_timestamp()",
    "getdate()",
    "sysdatetime()",
    "getutcdate()",
    "sysutcdatetime()",
    "sysdatetimeoffset()",
    "localtimestamp",
    "localtimestamp()",
    "localtime",
    "transaction_timestamp()",
    "statement_timestamp()",
    "clock_timestamp()",
    "datetime('now')",
    "datetime('now', 'localtime')",
];

static PG_CAST: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)::[a-z_][a-z0-9_ ]*(\(\d+(,\s*\d+)?\))?(\[\])?$").unwrap());
static NOW_WITH_PRECISION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(current_timestamp|now|localtimestamp|sysdate)\(\d*\)$").unwrap());

const UUID_FUNCS: &[&str] = &["gen_random_uuid()", "uuid_generate_v4()", "newid()", "uuid()", "newsequentialid()"];

fn bool_literal(to: Dialect, b: bool) -> String {
    match to {
        Dialect::Postgres => if b { "TRUE" } else { "FALSE" }.into(),
        _ => if b { "1" } else { "0" }.into(),
    }
}

/// Translate a column default expression from one dialect to another.
/// Unknown expressions are dropped (returns `None`) rather than producing invalid DDL.
pub fn convert_default(from: Dialect, to: Dialect, raw: &str, ty: &Ty) -> Option<String> {
    let mut s = raw.trim().to_string();
    while let Some(inner) = strip_wrapping_parens(&s) {
        s = inner.trim().to_string();
    }
    if from == Dialect::Postgres {
        // 'abc'::character varying, (0)::numeric, '{}'::jsonb
        while PG_CAST.is_match(&s) {
            s = PG_CAST.replace(&s, "").trim().to_string();
            while let Some(inner) = strip_wrapping_parens(&s) {
                s = inner.trim().to_string();
            }
        }
    }
    let lower = s.to_ascii_lowercase();
    if lower.is_empty() || lower == "null" || lower.starts_with("nextval(") {
        return None;
    }

    let is_now = NOW_FUNCS.contains(&lower.as_str())
        || NOW_WITH_PRECISION.is_match(&lower);
    if is_now {
        return Some(match (to, ty) {
            (Dialect::Mysql, Ty::Timestamp | Ty::TimestampTz) => "CURRENT_TIMESTAMP(6)".into(),
            (Dialect::Mysql, Ty::Date) => "(CURRENT_DATE)".into(),
            (Dialect::Mssql, Ty::TimestampTz) => "SYSDATETIMEOFFSET()".into(),
            (Dialect::Mssql, Ty::Date) => "CONVERT(date, SYSDATETIME())".into(),
            (Dialect::Mssql, _) => "SYSDATETIME()".into(),
            (_, Ty::Date) => "CURRENT_DATE".into(),
            _ => "CURRENT_TIMESTAMP".into(),
        });
    }
    if matches!(lower.as_str(), "current_date" | "curdate()" | "current_date()" | "date('now')") {
        return Some(match to {
            Dialect::Mysql => "(CURRENT_DATE)".into(),
            Dialect::Mssql => "CONVERT(date, SYSDATETIME())".into(),
            _ => "CURRENT_DATE".into(),
        });
    }
    if UUID_FUNCS.contains(&lower.as_str()) {
        return match to {
            Dialect::Postgres => Some("gen_random_uuid()".into()),
            Dialect::Mysql => Some("(uuid())".into()),
            Dialect::Mssql => Some("NEWID()".into()),
            _ => None,
        };
    }

    let bool_value = match lower.as_str() {
        "true" | "'true'" | "'t'" | "b'1'" | "'1'" => Some(true),
        "false" | "'false'" | "'f'" | "b'0'" | "'0'" => Some(false),
        "1" if *ty == Ty::Bool => Some(true),
        "0" if *ty == Ty::Bool => Some(false),
        _ => None,
    };
    if let Some(b) = bool_value
        && (*ty == Ty::Bool || !lower.starts_with('\''))
    {
        return Some(bool_literal(to, b));
    }
    if is_numeric_literal(&s) {
        return Some(s);
    }

    let text = match parse_string_literal(&s) {
        Some(t) => t,
        // MySQL reports string defaults unquoted (information_schema.COLUMN_DEFAULT)
        None if from == Dialect::Mysql
            && !s.contains('(')
            && !lower.starts_with("b'")
            && !lower.starts_with("x'")
            && !lower.starts_with("0x") =>
        {
            s.clone()
        }
        None => return if from == to { Some(s) } else { None },
    };
    let lit = literal(to, &Value::String(text));
    // MySQL only accepts expression defaults (in parens) on TEXT/BLOB/JSON columns.
    if to == Dialect::Mysql && matches!(ty, Ty::Text | Ty::Json | Ty::Array | Ty::Xml | Ty::Other(_) | Ty::Varchar(None)) {
        return Some(format!("({lit})"));
    }
    Some(lit)
}

// ---------------------------------------------------------------------------
// Structure conversion
// ---------------------------------------------------------------------------

/// Convert a table structure read from a `from` database into one suitable for `to`:
/// maps types, translates defaults, turns `serial` into auto-increment, makes
/// indexed text columns indexable (MySQL/MSSQL), drops dialect specific index methods.
pub fn convert_structure(from: Dialect, to: Dialect, t: &TableStructure) -> TableStructure {
    let mut out = t.clone();
    if from == to {
        return out;
    }
    let keyed: HashSet<String> = t
        .columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.clone())
        .chain(t.indexes.iter().flat_map(|i| i.columns.iter().cloned()))
        .chain(t.foreign_keys.iter().flat_map(|f| f.columns.iter().cloned()))
        .collect();
    for c in &mut out.columns {
        let (ty, serial) = parse_type(from, &c.data_type);
        c.auto_increment = c.auto_increment || serial;
        c.default = if c.auto_increment {
            None
        } else {
            c.default.as_deref().and_then(|d| convert_default(from, to, d, &ty))
        };
        let mut rendered = render_type(to, &ty);
        if keyed.contains(&c.name) {
            rendered = match (to, rendered.as_str()) {
                (Dialect::Mysql, "longtext" | "json") => "varchar(255)".into(),
                (Dialect::Mysql, "longblob") => "varbinary(255)".into(),
                (Dialect::Mssql, "nvarchar(max)") => "nvarchar(450)".into(),
                (Dialect::Mssql, "varbinary(max)") => "varbinary(900)".into(),
                _ => rendered,
            };
        }
        c.data_type = rendered;
    }
    let names: HashSet<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    out.indexes.retain(|i| i.columns.iter().all(|c| names.contains(c.as_str())));
    for i in &mut out.indexes {
        i.method = None;
    }
    for fk in &mut out.foreign_keys {
        fk.ref_schema = None;
        let norm = |a: &Option<String>| a.as_ref().map(|s| s.to_ascii_uppercase()).filter(|s| !(to == Dialect::Mssql && s == "RESTRICT"));
        fk.on_delete = norm(&fk.on_delete);
        fk.on_update = norm(&fk.on_update);
    }
    out
}

// ---------------------------------------------------------------------------
// CREATE / DROP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateTableOptions {
    pub foreign_keys: bool,
    pub indexes: bool,
    pub comments: bool,
    pub if_not_exists: bool,
}

impl Default for CreateTableOptions {
    fn default() -> Self {
        Self { foreign_keys: true, indexes: true, comments: true, if_not_exists: false }
    }
}

/// Primary key columns in key order.
pub fn primary_key_columns(t: &TableStructure) -> Vec<String> {
    if let Some(pk) = t.indexes.iter().find(|i| i.primary) {
        return pk.columns.clone();
    }
    let mut cols: Vec<&ColumnDef> = t.columns.iter().filter(|c| c.is_primary_key).collect();
    cols.sort_by_key(|c| c.ordinal);
    cols.into_iter().map(|c| c.name.clone()).collect()
}

/// Default clause value for `col` as it should appear in DDL of dialect `d`.
fn effective_default(d: Dialect, col: &ColumnDef) -> Option<String> {
    if col.auto_increment {
        return None;
    }
    let raw = col.default.as_deref()?;
    if d == Dialect::Mysql {
        let ty = parse_type(d, &col.data_type).0;
        return convert_default(d, d, raw, &ty);
    }
    Some(raw.trim().to_string())
}

fn column_sql(d: Dialect, col: &ColumnDef, sqlite_inline_pk: bool) -> String {
    let mut s = format!("{} {}", quote_ident(d, &col.name), col.data_type.trim());
    let not_null = !col.nullable || col.is_primary_key || col.auto_increment;
    if col.auto_increment {
        match d {
            Dialect::Postgres if !is_serial_type(&col.data_type) && is_integer_type(d, &col.data_type) => {
                s.push_str(" GENERATED BY DEFAULT AS IDENTITY")
            }
            Dialect::Mssql => s.push_str(" IDENTITY(1,1)"),
            _ => {}
        }
    }
    if sqlite_inline_pk {
        s.push_str(" PRIMARY KEY AUTOINCREMENT");
    }
    if not_null {
        s.push_str(" NOT NULL");
    }
    if col.auto_increment && d == Dialect::Mysql {
        s.push_str(" AUTO_INCREMENT");
    }
    if let Some(def) = effective_default(d, col) {
        s.push_str(" DEFAULT ");
        s.push_str(&def);
    }
    if d == Dialect::Mysql
        && let Some(c) = col.comment.as_deref().filter(|c| !c.is_empty())
    {
        s.push_str(" COMMENT ");
        s.push_str(&literal(d, &Value::String(c.into())));
    }
    s
}

fn fk_action(d: Dialect, a: &Option<String>) -> Option<String> {
    let a = a.as_deref()?.trim().to_ascii_uppercase();
    if a.is_empty() || a == "NO ACTION" || (d == Dialect::Mssql && a == "RESTRICT") {
        None
    } else {
        Some(a)
    }
}

/// `CONSTRAINT name FOREIGN KEY (..) REFERENCES ..` clause (no leading ALTER).
pub fn foreign_key_clause(d: Dialect, fk: &ForeignKeyDef) -> String {
    let ref_table = TableRef { database: None, schema: fk.ref_schema.clone(), name: fk.ref_table.clone() };
    let mut s = String::new();
    if !fk.name.is_empty() {
        s.push_str(&format!("CONSTRAINT {} ", quote_ident(d, &fk.name)));
    }
    s.push_str(&format!(
        "FOREIGN KEY ({}) REFERENCES {} ({})",
        col_list(d, &fk.columns),
        qualified_name(d, &ref_table),
        col_list(d, &fk.ref_columns)
    ));
    if let Some(a) = fk_action(d, &fk.on_delete) {
        s.push_str(&format!(" ON DELETE {a}"));
    }
    if let Some(a) = fk_action(d, &fk.on_update) {
        s.push_str(&format!(" ON UPDATE {a}"));
    }
    s
}

/// `ALTER TABLE .. ADD CONSTRAINT .. FOREIGN KEY ..` (not available on SQLite).
pub fn add_foreign_key_sql(d: Dialect, target: &TableRef, fk: &ForeignKeyDef) -> String {
    format!("ALTER TABLE {} ADD {}", qualified_name(d, target), foreign_key_clause(d, fk))
}

pub fn create_index_sql(d: Dialect, target: &TableRef, idx: &IndexDef) -> String {
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let using = match (d, idx.method.as_deref().map(str::to_ascii_lowercase)) {
        (Dialect::Postgres, Some(m)) if m != "btree" && !m.is_empty() => format!(" USING {m}"),
        _ => String::new(),
    };
    format!(
        "CREATE {unique}INDEX {} ON {}{using} ({})",
        quote_ident(d, &idx.name),
        qualified_name(d, target),
        col_list(d, &idx.columns)
    )
}

#[allow(dead_code)] // public helper for schema compare / other callers
pub fn drop_index_sql(d: Dialect, target: &TableRef, name: &str) -> String {
    match d {
        Dialect::Mysql | Dialect::Mssql => {
            format!("DROP INDEX {} ON {}", quote_ident(d, name), qualified_name(d, target))
        }
        Dialect::Postgres => {
            let r = TableRef { database: None, schema: target.schema.clone(), name: name.into() };
            format!("DROP INDEX {}", qualified_name(d, &r))
        }
        _ => format!("DROP INDEX {}", quote_ident(d, name)),
    }
}

/// When a table is created under a new name, rename objects whose names must be
/// unique beyond the table: indexes (Postgres/SQLite: per schema) and foreign keys
/// (MySQL: per database). `users_email_key` on `users` becomes `users_copy_email_key`.
fn renamed_for_target(d: Dialect, t: &TableStructure, target: &TableRef) -> TableStructure {
    let mut t = t.clone();
    let Some(src) = t.table.as_ref().map(|r| r.name.clone()).filter(|n| *n != target.name) else { return t };
    let rename = |name: &str| {
        if name.is_empty() {
            String::new()
        } else if name.contains(&src) {
            name.replacen(&src, &target.name, 1)
        } else {
            format!("{}_{name}", target.name)
        }
    };
    if matches!(d, Dialect::Postgres | Dialect::Sqlite) {
        for i in &mut t.indexes {
            i.name = rename(&i.name);
        }
    }
    if d == Dialect::Mysql {
        for fk in &mut t.foreign_keys {
            fk.name = rename(&fk.name);
        }
    }
    t
}

/// CREATE TABLE plus follow-up statements (indexes, comments), without trailing `;`.
pub fn create_table_statements(d: Dialect, t: &TableStructure, target: &TableRef, opts: &CreateTableOptions) -> Vec<String> {
    let t = &renamed_for_target(d, t, target);
    let name = qualified_name(d, target);
    let pk = primary_key_columns(t);
    let mut cols: Vec<&ColumnDef> = t.columns.iter().collect();
    cols.sort_by_key(|c| c.ordinal);

    // SQLite autoincrement only works on a single INTEGER PRIMARY KEY column.
    let sqlite_inline_pk = d == Dialect::Sqlite
        && pk.len() == 1
        && cols.iter().any(|c| c.name == pk[0] && c.auto_increment && c.data_type.trim().eq_ignore_ascii_case("integer"));

    let mut lines: Vec<String> = cols
        .iter()
        .map(|c| {
            let mut c = (*c).clone();
            c.is_primary_key = pk.contains(&c.name);
            column_sql(d, &c, sqlite_inline_pk && c.name == pk[0])
        })
        .collect();
    if !pk.is_empty() && !sqlite_inline_pk {
        lines.push(format!("PRIMARY KEY ({})", col_list(d, &pk)));
    }
    if opts.foreign_keys {
        lines.extend(t.foreign_keys.iter().map(|fk| foreign_key_clause(d, fk)));
    }

    let mut create = String::from("CREATE TABLE ");
    if opts.if_not_exists && d != Dialect::Mssql {
        create.push_str("IF NOT EXISTS ");
    }
    create.push_str(&name);
    create.push_str(" (\n  ");
    create.push_str(&lines.join(",\n  "));
    create.push_str("\n)");
    if d == Dialect::Mysql
        && opts.comments
        && let Some(c) = t.comment.as_deref().filter(|c| !c.is_empty())
    {
        create.push_str(&format!(" COMMENT={}", literal(d, &Value::String(c.into()))));
    }
    if opts.if_not_exists && d == Dialect::Mssql {
        let obj = literal(d, &Value::String(name.clone()));
        create = format!("IF OBJECT_ID({obj}, N'U') IS NULL {create}");
    }

    let mut out = vec![create];
    if opts.indexes {
        for idx in &t.indexes {
            if idx.primary || idx.columns.is_empty() || (idx.unique && idx.columns == pk) {
                continue;
            }
            out.push(create_index_sql(d, target, idx));
        }
    }
    if opts.comments && d == Dialect::Postgres {
        if let Some(c) = t.comment.as_deref().filter(|c| !c.is_empty()) {
            out.push(format!("COMMENT ON TABLE {name} IS {}", literal(d, &Value::String(c.into()))));
        }
        for col in &cols {
            if let Some(c) = col.comment.as_deref().filter(|c| !c.is_empty()) {
                out.push(format!(
                    "COMMENT ON COLUMN {name}.{} IS {}",
                    quote_ident(d, &col.name),
                    literal(d, &Value::String(c.into()))
                ));
            }
        }
    }
    out
}

/// Full CREATE TABLE script (table, indexes, foreign keys, comments), statements terminated by `;`.
/// `t` must already use `dialect`'s types — see [`convert_structure`] for cross-dialect use.
#[allow(dead_code)] // public helper for schema compare / other callers
pub fn create_table_sql(dialect: Dialect, t: &TableStructure, target: &TableRef) -> String {
    create_table_sql_with(dialect, t, target, &CreateTableOptions::default())
}

pub fn create_table_sql_with(dialect: Dialect, t: &TableStructure, target: &TableRef, opts: &CreateTableOptions) -> String {
    let mut s = create_table_statements(dialect, t, target, opts).join(";\n");
    s.push(';');
    s
}

pub fn drop_table_sql(dialect: Dialect, target: &TableRef, if_exists: bool) -> String {
    format!("DROP TABLE {}{}", if if_exists { "IF EXISTS " } else { "" }, qualified_name(dialect, target))
}

// ---------------------------------------------------------------------------
// ALTER (structure diff)
// ---------------------------------------------------------------------------

fn norm_type(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase().replace(", ", ",")
}

fn norm_default(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|d| d.trim().to_string()).filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("null"))
}

fn find_col<'a>(cols: &'a [ColumnDef], name: &str) -> Option<&'a ColumnDef> {
    cols.iter().find(|c| c.name == name).or_else(|| cols.iter().find(|c| c.name.eq_ignore_ascii_case(name)))
}

/// T-SQL batch (no semicolons, so script splitters keep it intact) that drops the
/// default constraint of a column, whatever its generated name is.
fn mssql_drop_default(target: &TableRef, table: &str, column: &str) -> String {
    let obj = literal(Dialect::Mssql, &Value::String(qualified_name(Dialect::Mssql, target)));
    let col = literal(Dialect::Mssql, &Value::String(column.into()));
    format!(
        "DECLARE @df sysname SELECT @df = d.name FROM sys.default_constraints d JOIN sys.columns c \
         ON d.parent_object_id = c.object_id AND d.parent_column_id = c.column_id \
         WHERE d.parent_object_id = OBJECT_ID({obj}) AND c.name = {col} \
         IF @df IS NOT NULL EXEC('ALTER TABLE {} DROP CONSTRAINT ' + QUOTENAME(@df))",
        table.replace('\'', "''")
    )
}

/// T-SQL batch dropping the primary key constraint of a table by lookup.
fn mssql_drop_pk(target: &TableRef, table: &str) -> String {
    let obj = literal(Dialect::Mssql, &Value::String(qualified_name(Dialect::Mssql, target)));
    format!(
        "DECLARE @pk sysname SELECT @pk = name FROM sys.key_constraints WHERE type = 'PK' AND parent_object_id = OBJECT_ID({obj}) \
         IF @pk IS NOT NULL EXEC('ALTER TABLE {} DROP CONSTRAINT ' + QUOTENAME(@pk))",
        table.replace('\'', "''")
    )
}

/// Diff two structures of the same table and produce the statements turning
/// `from` into `to` (no trailing `;`). Renames are not detected (drop + add).
/// SQLite gets the 12-step table rebuild recipe when ALTER TABLE cannot express the change.
#[allow(dead_code)] // public helper for schema compare / other callers
pub fn alter_table_sql(dialect: Dialect, from: &TableStructure, to: &TableStructure) -> Vec<String> {
    let d = dialect;
    let target = to
        .table
        .clone()
        .or_else(|| from.table.clone())
        .unwrap_or(TableRef { database: None, schema: None, name: String::new() });
    let tbl = qualified_name(d, &target);
    let q = |s: &str| quote_ident(d, s);

    let dropped: Vec<&ColumnDef> = from.columns.iter().filter(|c| find_col(&to.columns, &c.name).is_none()).collect();
    let added: Vec<&ColumnDef> = to.columns.iter().filter(|c| find_col(&from.columns, &c.name).is_none()).collect();
    let modified: Vec<(&ColumnDef, &ColumnDef)> = to
        .columns
        .iter()
        .filter_map(|n| find_col(&from.columns, &n.name).map(|o| (o, n)))
        .filter(|(o, n)| {
            norm_type(&o.data_type) != norm_type(&n.data_type)
                || o.nullable != n.nullable
                || norm_default(&o.default) != norm_default(&n.default)
                || o.auto_increment != n.auto_increment
                || o.comment.as_deref().unwrap_or("") != n.comment.as_deref().unwrap_or("")
        })
        .collect();

    let pk_from = primary_key_columns(from);
    let pk_to = primary_key_columns(to);
    let pk_changed = pk_from != pk_to;

    let idx_map = |t: &TableStructure| -> HashMap<String, IndexDef> {
        t.indexes.iter().filter(|i| !i.primary).map(|i| (i.name.clone(), i.clone())).collect()
    };
    let (ifrom, ito) = (idx_map(from), idx_map(to));
    let idx_same = |a: &IndexDef, b: &IndexDef| a.columns == b.columns && a.unique == b.unique;
    let mut idx_drop: Vec<&IndexDef> =
        from.indexes.iter().filter(|i| !i.primary && ito.get(&i.name).is_none_or(|n| !idx_same(i, n))).collect();
    let mut idx_add: Vec<&IndexDef> =
        to.indexes.iter().filter(|i| !i.primary && ifrom.get(&i.name).is_none_or(|o| !idx_same(o, i))).collect();

    let fk_same = |a: &ForeignKeyDef, b: &ForeignKeyDef| {
        a.columns == b.columns
            && a.ref_table == b.ref_table
            && a.ref_columns == b.ref_columns
            && fk_action(d, &a.on_delete) == fk_action(d, &b.on_delete)
            && fk_action(d, &a.on_update) == fk_action(d, &b.on_update)
    };
    let fk_drop: Vec<&ForeignKeyDef> = from
        .foreign_keys
        .iter()
        .filter(|f| !to.foreign_keys.iter().any(|n| n.name == f.name && fk_same(f, n)))
        .collect();
    let fk_add: Vec<&ForeignKeyDef> = to
        .foreign_keys
        .iter()
        .filter(|f| !from.foreign_keys.iter().any(|o| o.name == f.name && fk_same(o, f)))
        .collect();

    let mut out = Vec::new();

    if d == Dialect::Sqlite {
        let needs_rebuild = !dropped.is_empty()
            || !modified.is_empty()
            || pk_changed
            || !fk_drop.is_empty()
            || !fk_add.is_empty()
            || added.iter().any(|c| c.is_primary_key || (!c.nullable && norm_default(&c.default).is_none()));
        if needs_rebuild {
            let tmp = TableRef { database: target.database.clone(), schema: target.schema.clone(), name: format!("_devcat_tmp_{}", target.name) };
            let opts = CreateTableOptions { foreign_keys: true, indexes: false, comments: false, if_not_exists: false };
            let common: Vec<String> = to
                .columns
                .iter()
                .filter(|c| find_col(&from.columns, &c.name).is_some())
                .map(|c| q(&c.name))
                .collect();
            out.push("PRAGMA foreign_keys=OFF".into());
            out.push(create_table_statements(d, to, &tmp, &opts).remove(0));
            if !common.is_empty() {
                let list = common.join(", ");
                out.push(format!("INSERT INTO {} ({list}) SELECT {list} FROM {tbl}", qualified_name(d, &tmp)));
            }
            out.push(format!("DROP TABLE {tbl}"));
            out.push(format!("ALTER TABLE {} RENAME TO {}", qualified_name(d, &tmp), q(&target.name)));
            for idx in to.indexes.iter().filter(|i| !i.primary && !i.columns.is_empty()) {
                out.push(create_index_sql(d, &target, idx));
            }
            out.push("PRAGMA foreign_keys=ON".into());
            return out;
        }
        idx_drop.iter().for_each(|i| out.push(drop_index_sql(d, &target, &i.name)));
        added.iter().for_each(|c| out.push(format!("ALTER TABLE {tbl} ADD COLUMN {}", column_sql(d, c, false))));
        idx_add.iter().for_each(|i| out.push(create_index_sql(d, &target, i)));
        return out;
    }

    // Indexes on changed columns are rebuilt to be safe on MSSQL (ALTER COLUMN refuses indexed columns).
    if d == Dialect::Mssql {
        let touched: HashSet<&str> = modified.iter().map(|(_, n)| n.name.as_str()).chain(dropped.iter().map(|c| c.name.as_str())).collect();
        for i in from.indexes.iter().filter(|i| !i.primary && i.columns.iter().any(|c| touched.contains(c.as_str()))) {
            if !idx_drop.iter().any(|x| x.name == i.name) {
                idx_drop.push(i);
                if let Some(n) = ito.get(&i.name) {
                    idx_add.push(n);
                }
            }
        }
    }

    // 1. drop foreign keys and indexes
    for fk in &fk_drop {
        out.push(match d {
            Dialect::Mysql => format!("ALTER TABLE {tbl} DROP FOREIGN KEY {}", q(&fk.name)),
            _ => format!("ALTER TABLE {tbl} DROP CONSTRAINT {}", q(&fk.name)),
        });
    }
    for i in &idx_drop {
        // MySQL: unique/foreign-key backing indexes share names with FKs; dropping is still by index name.
        out.push(drop_index_sql(d, &target, &i.name));
    }
    // 2. primary key
    if pk_changed && !pk_from.is_empty() {
        out.push(match d {
            Dialect::Mysql => format!("ALTER TABLE {tbl} DROP PRIMARY KEY"),
            Dialect::Postgres => {
                let name = from
                    .indexes
                    .iter()
                    .find(|i| i.primary && !i.name.is_empty())
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| format!("{}_pkey", target.name));
                format!("ALTER TABLE {tbl} DROP CONSTRAINT {}", q(&name))
            }
            _ => mssql_drop_pk(&target, &tbl),
        });
    }
    // 3. columns
    for c in &dropped {
        if d == Dialect::Mssql && norm_default(&c.default).is_some() {
            out.push(mssql_drop_default(&target, &tbl, &c.name));
        }
        out.push(format!("ALTER TABLE {tbl} DROP COLUMN {}", q(&c.name)));
    }
    for c in &added {
        let mut c = (*c).clone();
        c.is_primary_key = false; // PK added below
        let kw = if d == Dialect::Mssql { "ADD" } else { "ADD COLUMN" };
        out.push(format!("ALTER TABLE {tbl} {kw} {}", column_sql(d, &c, false)));
        if d == Dialect::Postgres
            && let Some(cm) = c.comment.as_deref().filter(|s| !s.is_empty())
        {
            out.push(format!("COMMENT ON COLUMN {tbl}.{} IS {}", q(&c.name), literal(d, &Value::String(cm.into()))));
        }
    }
    for (o, n) in &modified {
        let col = q(&n.name);
        match d {
            Dialect::Mysql => {
                let mut n = (*n).clone();
                n.is_primary_key = false;
                out.push(format!("ALTER TABLE {tbl} MODIFY COLUMN {}", column_sql(d, &n, false)));
            }
            Dialect::Postgres => {
                if norm_type(&o.data_type) != norm_type(&n.data_type) {
                    out.push(format!(
                        "ALTER TABLE {tbl} ALTER COLUMN {col} TYPE {t} USING {col}::{t}",
                        t = n.data_type.trim()
                    ));
                }
                if o.nullable != n.nullable {
                    let op = if n.nullable { "DROP" } else { "SET" };
                    out.push(format!("ALTER TABLE {tbl} ALTER COLUMN {col} {op} NOT NULL"));
                }
                if o.auto_increment != n.auto_increment {
                    out.push(if n.auto_increment {
                        format!("ALTER TABLE {tbl} ALTER COLUMN {col} ADD GENERATED BY DEFAULT AS IDENTITY")
                    } else {
                        format!("ALTER TABLE {tbl} ALTER COLUMN {col} DROP IDENTITY IF EXISTS")
                    });
                }
                if norm_default(&o.default) != norm_default(&n.default) && !n.auto_increment {
                    out.push(match norm_default(&n.default) {
                        Some(v) => format!("ALTER TABLE {tbl} ALTER COLUMN {col} SET DEFAULT {v}"),
                        None => format!("ALTER TABLE {tbl} ALTER COLUMN {col} DROP DEFAULT"),
                    });
                }
                if o.comment.as_deref().unwrap_or("") != n.comment.as_deref().unwrap_or("") {
                    let v = match n.comment.as_deref().filter(|s| !s.is_empty()) {
                        Some(cm) => literal(d, &Value::String(cm.into())),
                        None => "NULL".into(),
                    };
                    out.push(format!("COMMENT ON COLUMN {tbl}.{col} IS {v}"));
                }
            }
            _ => {
                // MSSQL: identity cannot be altered in place; type/nullability can.
                let default_changed = norm_default(&o.default) != norm_default(&n.default);
                if default_changed && norm_default(&o.default).is_some() {
                    out.push(mssql_drop_default(&target, &tbl, &n.name));
                }
                if norm_type(&o.data_type) != norm_type(&n.data_type) || o.nullable != n.nullable {
                    let null = if n.nullable && !n.is_primary_key { "NULL" } else { "NOT NULL" };
                    out.push(format!("ALTER TABLE {tbl} ALTER COLUMN {col} {} {null}", n.data_type.trim()));
                }
                if default_changed && let Some(v) = norm_default(&n.default) {
                    out.push(format!("ALTER TABLE {tbl} ADD DEFAULT {v} FOR {col}"));
                }
            }
        }
    }
    // 4. re-add primary key, indexes, foreign keys
    if pk_changed && !pk_to.is_empty() {
        out.push(format!("ALTER TABLE {tbl} ADD PRIMARY KEY ({})", col_list(d, &pk_to)));
    }
    for i in &idx_add {
        if !i.columns.is_empty() {
            out.push(create_index_sql(d, &target, i));
        }
    }
    for fk in &fk_add {
        out.push(add_foreign_key_sql(d, &target, fk));
    }
    if d == Dialect::Postgres && from.comment.as_deref().unwrap_or("") != to.comment.as_deref().unwrap_or("") {
        let v = match to.comment.as_deref().filter(|s| !s.is_empty()) {
            Some(cm) => literal(d, &Value::String(cm.into())),
            None => "NULL".into(),
        };
        out.push(format!("COMMENT ON TABLE {tbl} IS {v}"));
    }
    if d == Dialect::Mysql && from.comment.as_deref().unwrap_or("") != to.comment.as_deref().unwrap_or("") {
        out.push(format!("ALTER TABLE {tbl} COMMENT = {}", literal(d, &Value::String(to.comment.clone().unwrap_or_default()))));
    }
    out
}

// ---------------------------------------------------------------------------
// Dependency ordering
// ---------------------------------------------------------------------------

/// Order tables so that FK parents come before children (stable w.r.t. input order).
/// Returns `(order, cyclic)`: indices into `tables`, and the indices of tables that
/// are part of (or depend on) a cycle — those are appended at the end of `order`
/// and their foreign keys must be added after all tables exist.
pub fn dependency_order(tables: &[TableStructure]) -> (Vec<usize>, HashSet<usize>) {
    let name_of = |t: &TableStructure| t.table.as_ref().map(|r| r.name.to_ascii_lowercase()).unwrap_or_default();
    let by_name: HashMap<String, usize> = tables.iter().enumerate().map(|(i, t)| (name_of(t), i)).collect();
    let deps: Vec<HashSet<usize>> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| {
            t.foreign_keys
                .iter()
                .filter_map(|fk| by_name.get(&fk.ref_table.to_ascii_lowercase()).copied())
                .filter(|&j| j != i)
                .collect()
        })
        .collect();
    let mut done = vec![false; tables.len()];
    let mut order = Vec::with_capacity(tables.len());
    loop {
        let next = (0..tables.len()).find(|&i| !done[i] && deps[i].iter().all(|&j| done[j]));
        match next {
            Some(i) => {
                done[i] = true;
                order.push(i);
            }
            None => break,
        }
    }
    let cyclic: HashSet<usize> = (0..tables.len()).filter(|&i| !done[i]).collect();
    order.extend((0..tables.len()).filter(|i| cyclic.contains(i)));
    (order, cyclic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Dialect::{Mssql, Mysql, Postgres, Sqlite};

    fn col(name: &str, ty: &str, nullable: bool, pk: bool, auto: bool, default: Option<&str>, ord: u32) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: ty.into(),
            nullable,
            default: default.map(Into::into),
            is_primary_key: pk,
            auto_increment: auto,
            comment: None,
            ordinal: ord,
        }
    }

    fn tref(schema: Option<&str>, name: &str) -> TableRef {
        TableRef { database: None, schema: schema.map(Into::into), name: name.into() }
    }

    fn users_pg() -> TableStructure {
        TableStructure {
            table: Some(tref(Some("public"), "users")),
            columns: vec![
                col("id", "integer", false, true, true, Some("nextval('users_id_seq'::regclass)"), 1),
                col("email", "character varying(255)", false, false, false, None, 2),
                col("active", "boolean", false, false, false, Some("true"), 3),
                col("created_at", "timestamp with time zone", false, false, false, Some("now()"), 4),
                col("profile", "jsonb", true, false, false, Some("'{}'::jsonb"), 5),
                col("balance", "numeric(12,2)", true, false, false, Some("0"), 6),
                col("avatar", "bytea", true, false, false, None, 7),
                col("token", "uuid", true, false, false, Some("gen_random_uuid()"), 8),
                col("bio", "text", true, false, false, Some("'it''s me'::text"), 9),
            ],
            indexes: vec![
                IndexDef { name: "users_pkey".into(), columns: vec!["id".into()], unique: true, primary: true, method: Some("btree".into()) },
                IndexDef { name: "users_email_key".into(), columns: vec!["email".into()], unique: true, primary: false, method: Some("btree".into()) },
                IndexDef { name: "users_profile_gin".into(), columns: vec!["profile".into()], unique: false, primary: false, method: Some("gin".into()) },
            ],
            foreign_keys: vec![],
            comment: Some("app users".into()),
        }
    }

    #[test]
    fn type_mapping_matrix() {
        let cases: &[(Dialect, Dialect, &str, &str)] = &[
            (Postgres, Mysql, "serial", "int"),
            (Postgres, Mysql, "bigserial", "bigint"),
            (Postgres, Mssql, "character varying(50)", "nvarchar(50)"),
            (Postgres, Mssql, "text", "nvarchar(max)"),
            (Postgres, Mysql, "text", "longtext"),
            (Postgres, Sqlite, "boolean", "boolean"),
            (Postgres, Mssql, "boolean", "bit"),
            (Postgres, Mysql, "boolean", "tinyint(1)"),
            (Mysql, Postgres, "tinyint(1)", "boolean"),
            (Mysql, Postgres, "tinyint(4)", "smallint"),
            (Mysql, Mssql, "tinyint", "smallint"),
            (Mssql, Mysql, "tinyint", "tinyint unsigned"),
            (Mssql, Postgres, "bit", "boolean"),
            (Postgres, Mssql, "timestamp with time zone", "datetimeoffset"),
            (Postgres, Mssql, "timestamp(3) without time zone", "datetime2"),
            (Mssql, Postgres, "datetime2(7)", "timestamp"),
            (Mssql, Postgres, "datetimeoffset", "timestamptz"),
            (Mysql, Postgres, "datetime(6)", "timestamp"),
            (Postgres, Mysql, "timestamptz", "datetime(6)"),
            (Postgres, Mssql, "jsonb", "nvarchar(max)"),
            (Mysql, Postgres, "json", "jsonb"),
            (Postgres, Mysql, "bytea", "longblob"),
            (Mysql, Mssql, "blob", "varbinary(max)"),
            (Mssql, Postgres, "varbinary(max)", "bytea"),
            (Postgres, Mysql, "numeric(10,2)", "decimal(10,2)"),
            (Postgres, Mssql, "numeric", "decimal(38,10)"),
            (Mssql, Postgres, "decimal(18, 4)", "numeric(18,4)"),
            (Postgres, Mssql, "uuid", "uniqueidentifier"),
            (Mssql, Postgres, "uniqueidentifier", "uuid"),
            (Postgres, Mysql, "uuid", "char(36)"),
            (Mysql, Postgres, "enum('a','b')", "text"),
            (Postgres, Mysql, "USER-DEFINED", "varchar(255)"),
            (Mysql, Postgres, "int(11) unsigned", "bigint"),
            (Mysql, Postgres, "bigint(20) unsigned", "numeric(20,0)"),
            (Mssql, Postgres, "nvarchar(max)", "text"),
            (Mssql, Postgres, "nvarchar(100)", "varchar(100)"),
            (Mssql, Mysql, "nvarchar(max)", "longtext"),
            (Mssql, Postgres, "timestamp", "bytea"),
            (Mysql, Postgres, "double", "double precision"),
            (Mysql, Postgres, "float", "real"),
            (Mssql, Postgres, "float", "double precision"),
            (Postgres, Mysql, "integer[]", "json"),
            (Postgres, Sqlite, "character varying(20)", "varchar(20)"),
            (Sqlite, Postgres, "INTEGER", "bigint"),
            (Sqlite, Postgres, "TEXT", "text"),
            (Sqlite, Postgres, "REAL", "real"),
            (Sqlite, Mysql, "BLOB", "longblob"),
            (Postgres, Mysql, "geometry", "longtext"),
            (Postgres, Mssql, "money", "decimal(19,4)"),
            (Postgres, Mysql, "interval", "varchar(64)"),
            (Postgres, Mysql, "character(3)", "char(3)"),
            (Mysql, Postgres, "year", "smallint"),
            (Dialect::None, Postgres, "string", "text"),
            (Dialect::None, Postgres, "objectId", "varchar(24)"),
            (Dialect::None, Mysql, "object", "json"),
        ];
        for (f, t, input, want) in cases {
            assert_eq!(map_type(*f, *t, input), *want, "{f:?} -> {t:?}: {input}");
        }
        assert_eq!(map_type(Postgres, Postgres, " character varying(10) "), "character varying(10)");
    }

    #[test]
    fn type_classes() {
        assert_eq!(type_class(Mysql, "tinyint(1)"), TypeClass::Bool);
        assert_eq!(type_class(Postgres, "int8"), TypeClass::Integer);
        assert_eq!(type_class(Postgres, "numeric(5,2)"), TypeClass::Decimal);
        assert_eq!(type_class(Mssql, "varbinary(max)"), TypeClass::Binary);
        assert_eq!(type_class(Postgres, "jsonb"), TypeClass::Json);
        assert_eq!(type_class(Postgres, "timestamptz"), TypeClass::Temporal);
    }

    #[test]
    fn defaults_translation() {
        assert_eq!(convert_default(Mssql, Postgres, "((0))", &Ty::Bool).as_deref(), Some("FALSE"));
        assert_eq!(convert_default(Mssql, Postgres, "((42))", &Ty::Int).as_deref(), Some("42"));
        assert_eq!(convert_default(Mssql, Postgres, "(getdate())", &Ty::Timestamp).as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(convert_default(Postgres, Mysql, "now()", &Ty::TimestampTz).as_deref(), Some("CURRENT_TIMESTAMP(6)"));
        assert_eq!(convert_default(Postgres, Mssql, "CURRENT_TIMESTAMP", &Ty::Timestamp).as_deref(), Some("SYSDATETIME()"));
        assert_eq!(convert_default(Postgres, Mssql, "'abc'::character varying", &Ty::Varchar(Some(5))).as_deref(), Some("N'abc'"));
        assert_eq!(convert_default(Postgres, Mysql, "'it''s'::text", &Ty::Text).as_deref(), Some("('it''s')"));
        assert_eq!(convert_default(Postgres, Mysql, "true", &Ty::Bool).as_deref(), Some("1"));
        assert_eq!(convert_default(Mysql, Postgres, "1", &Ty::Bool).as_deref(), Some("TRUE"));
        assert_eq!(convert_default(Mysql, Postgres, "b'0'", &Ty::Bool).as_deref(), Some("FALSE"));
        assert_eq!(convert_default(Mysql, Postgres, "hello", &Ty::Varchar(Some(10))).as_deref(), Some("'hello'"));
        assert_eq!(convert_default(Mysql, Postgres, "CURRENT_TIMESTAMP", &Ty::Timestamp).as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(convert_default(Postgres, Mysql, "nextval('x_seq'::regclass)", &Ty::Int), Option::None);
        assert_eq!(convert_default(Postgres, Mssql, "gen_random_uuid()", &Ty::Uuid).as_deref(), Some("NEWID()"));
        assert_eq!(convert_default(Postgres, Mysql, "(0)::numeric", &Ty::Decimal(Option::None)).as_deref(), Some("0"));
        assert_eq!(convert_default(Postgres, Mysql, "-1.5", &Ty::Double).as_deref(), Some("-1.5"));
        assert_eq!(convert_default(Postgres, Mysql, "lower('X')", &Ty::Text), Option::None);
        assert_eq!(convert_default(Postgres, Mysql, "NULL::character varying", &Ty::Text), Option::None);
        assert_eq!(convert_default(Mssql, Mysql, "(N'x')", &Ty::Varchar(Some(3))).as_deref(), Some("'x'"));
    }

    #[test]
    fn create_table_postgres_same_dialect() {
        let t = users_pg();
        let sql = create_table_sql(Postgres, &t, &tref(Some("public"), "users"));
        assert!(sql.starts_with("CREATE TABLE \"public\".\"users\" (\n"), "{sql}");
        assert!(sql.contains("\"id\" integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,"), "{sql}");
        assert!(!sql.contains("nextval"), "{sql}");
        assert!(sql.contains("\"active\" boolean NOT NULL DEFAULT true"), "{sql}");
        assert!(sql.contains("PRIMARY KEY (\"id\")"), "{sql}");
        assert!(sql.contains("CREATE UNIQUE INDEX \"users_email_key\" ON \"public\".\"users\" (\"email\");"), "{sql}");
        assert!(sql.contains("CREATE INDEX \"users_profile_gin\" ON \"public\".\"users\" USING gin (\"profile\");"), "{sql}");
        assert!(sql.contains("COMMENT ON TABLE \"public\".\"users\" IS 'app users';"), "{sql}");
        assert!(sql.ends_with(';'));
    }

    #[test]
    fn create_table_cross_dialect() {
        let t = users_pg();
        let my = convert_structure(Postgres, Mysql, &t);
        let sql = create_table_sql(Mysql, &my, &TableRef { database: Some("shop".into()), schema: Option::None, name: "users".into() });
        assert!(sql.contains("CREATE TABLE `shop`.`users`"), "{sql}");
        assert!(sql.contains("`id` int NOT NULL AUTO_INCREMENT,"), "{sql}");
        assert!(sql.contains("`email` varchar(255) NOT NULL"), "{sql}");
        assert!(sql.contains("`active` tinyint(1) NOT NULL DEFAULT 1"), "{sql}");
        assert!(sql.contains("`created_at` datetime(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)"), "{sql}");
        // profile is indexed: json -> varchar(255) so the index is valid
        assert!(sql.contains("`profile` varchar(255) DEFAULT '{}'"), "{sql}");
        assert!(sql.contains("`balance` decimal(12,2) DEFAULT 0"), "{sql}");
        assert!(sql.contains("`token` char(36) DEFAULT (uuid())"), "{sql}");
        assert!(sql.contains("`bio` longtext DEFAULT ('it''s me')"), "{sql}");
        assert!(sql.contains(") COMMENT='app users'"), "{sql}");
        assert!(sql.contains("CREATE INDEX `users_profile_gin` ON `shop`.`users` (`profile`)"), "{sql}");

        let ms = convert_structure(Postgres, Mssql, &t);
        let sql = create_table_sql(Mssql, &ms, &tref(Some("dbo"), "users"));
        assert!(sql.contains("[id] int IDENTITY(1,1) NOT NULL"), "{sql}");
        assert!(sql.contains("[active] bit NOT NULL DEFAULT 1"), "{sql}");
        assert!(sql.contains("[created_at] datetimeoffset NOT NULL DEFAULT SYSDATETIMEOFFSET()"), "{sql}");
        assert!(sql.contains("[profile] nvarchar(450) DEFAULT N'{}'"), "{sql}");
        assert!(sql.contains("[token] uniqueidentifier DEFAULT NEWID()"), "{sql}");

        let lite = convert_structure(Postgres, Sqlite, &t);
        let sql = create_table_sql(Sqlite, &lite, &tref(Option::None, "users"));
        assert!(sql.contains("\"id\" integer PRIMARY KEY AUTOINCREMENT NOT NULL,"), "{sql}");
        assert!(!sql.contains("PRIMARY KEY (\"id\")"), "{sql}");
        assert!(sql.contains("\"created_at\" datetime NOT NULL DEFAULT CURRENT_TIMESTAMP"), "{sql}");
    }

    #[test]
    fn mysql_to_postgres_serial_and_defaults() {
        let t = TableStructure {
            table: Some(TableRef { database: Some("shop".into()), schema: Option::None, name: "orders".into() }),
            columns: vec![
                col("id", "bigint unsigned", false, true, true, Option::None, 1),
                col("status", "enum('new','paid')", false, false, false, Some("new"), 2),
                col("paid", "tinyint(1)", false, false, false, Some("0"), 3),
                col("user_id", "int", false, false, false, Option::None, 4),
            ],
            indexes: vec![],
            foreign_keys: vec![ForeignKeyDef {
                name: "fk_orders_user".into(),
                columns: vec!["user_id".into()],
                ref_schema: Some("shop".into()),
                ref_table: "users".into(),
                ref_columns: vec!["id".into()],
                on_update: Some("NO ACTION".into()),
                on_delete: Some("cascade".into()),
            }],
            comment: Option::None,
        };
        let pg = convert_structure(Mysql, Postgres, &t);
        let sql = create_table_sql(Postgres, &pg, &tref(Some("public"), "orders"));
        // unsigned bigint becomes numeric(20,0): identity cannot apply to numeric
        assert!(sql.contains("\"id\" numeric(20,0) NOT NULL,"), "{sql}");
        assert!(sql.contains("\"status\" text NOT NULL DEFAULT 'new'"), "{sql}");
        assert!(sql.contains("\"paid\" boolean NOT NULL DEFAULT FALSE"), "{sql}");
        assert!(
            sql.contains("CONSTRAINT \"fk_orders_user\" FOREIGN KEY (\"user_id\") REFERENCES \"users\" (\"id\") ON DELETE CASCADE"),
            "{sql}"
        );
        assert!(!sql.contains("ON UPDATE"), "{sql}");
        let no_fk = create_table_sql_with(Postgres, &pg, &tref(Option::None, "orders"), &CreateTableOptions { foreign_keys: false, ..Default::default() });
        assert!(!no_fk.contains("FOREIGN KEY"));
    }

    #[test]
    fn mysql_same_dialect_quotes_bare_defaults() {
        let t = TableStructure {
            columns: vec![col("s", "varchar(10)", true, false, false, Some("abc"), 1), col("d", "datetime", true, false, false, Some("CURRENT_TIMESTAMP"), 2)],
            ..Default::default()
        };
        let sql = create_table_sql(Mysql, &t, &tref(Option::None, "t"));
        assert!(sql.contains("`s` varchar(10) DEFAULT 'abc'"), "{sql}");
        assert!(sql.contains("`d` datetime DEFAULT CURRENT_TIMESTAMP"), "{sql}");
    }

    #[test]
    fn drop_and_names() {
        assert_eq!(drop_table_sql(Postgres, &tref(Some("s"), "t"), true), "DROP TABLE IF EXISTS \"s\".\"t\"");
        assert_eq!(drop_table_sql(Mssql, &tref(Some("dbo"), "t"), false), "DROP TABLE [dbo].[t]");
        let r = TableRef { database: Some("db".into()), schema: Option::None, name: "t".into() };
        assert_eq!(qualified_name(Mysql, &r), "`db`.`t`");
        assert_eq!(qualified_name(Postgres, &r), "\"t\"");
        assert_eq!(qualified_name(Mssql, &r), "[db]..[t]");
        assert_eq!(qualified_name(Sqlite, &TableRef { database: Some("main".into()), schema: Option::None, name: "t".into() }), "\"t\"");
    }

    #[test]
    fn renamed_copy_gets_unique_index_names() {
        let sql = create_table_sql(Postgres, &users_pg(), &tref(Some("public"), "users_copy"));
        assert!(sql.contains("CREATE UNIQUE INDEX \"users_copy_email_key\" ON \"public\".\"users_copy\""), "{sql}");
        let mut t = users_pg();
        t.indexes[1].name = "uq_email".into();
        let sql = create_table_sql(Sqlite, &t, &tref(Option::None, "c"));
        assert!(sql.contains("CREATE UNIQUE INDEX \"c_uq_email\""), "{sql}");
    }

    #[test]
    fn mssql_if_not_exists() {
        let t = TableStructure { columns: vec![col("a", "int", true, false, false, Option::None, 1)], ..Default::default() };
        let sql = create_table_sql_with(Mssql, &t, &tref(Some("dbo"), "t"), &CreateTableOptions { if_not_exists: true, ..Default::default() });
        assert!(sql.starts_with("IF OBJECT_ID(N'[dbo].[t]', N'U') IS NULL CREATE TABLE [dbo].[t]"), "{sql}");
    }

    fn altered() -> (TableStructure, TableStructure) {
        let from = users_pg();
        let mut to = users_pg();
        to.columns.retain(|c| c.name != "avatar");
        to.columns.push(col("nickname", "varchar(40)", true, false, false, Some("'x'"), 10));
        let email = to.columns.iter_mut().find(|c| c.name == "email").unwrap();
        email.data_type = "varchar(320)".into();
        email.nullable = true;
        let bal = to.columns.iter_mut().find(|c| c.name == "balance").unwrap();
        bal.default = Option::None;
        to.indexes.retain(|i| i.name != "users_profile_gin");
        to.indexes.push(IndexDef { name: "users_nick_idx".into(), columns: vec!["nickname".into()], unique: false, primary: false, method: Option::None });
        to.foreign_keys.push(ForeignKeyDef {
            name: "users_team_fk".into(),
            columns: vec!["id".into()],
            ref_schema: Option::None,
            ref_table: "teams".into(),
            ref_columns: vec!["id".into()],
            on_update: Option::None,
            on_delete: Some("SET NULL".into()),
        });
        (from, to)
    }

    #[test]
    fn alter_postgres() {
        let (from, to) = altered();
        let s = alter_table_sql(Postgres, &from, &to);
        let t = "\"public\".\"users\"";
        let want = [
            format!("DROP INDEX \"public\".\"users_profile_gin\""),
            format!("ALTER TABLE {t} DROP COLUMN \"avatar\""),
            format!("ALTER TABLE {t} ADD COLUMN \"nickname\" varchar(40) DEFAULT 'x'"),
            format!("ALTER TABLE {t} ALTER COLUMN \"email\" TYPE varchar(320) USING \"email\"::varchar(320)"),
            format!("ALTER TABLE {t} ALTER COLUMN \"email\" DROP NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN \"balance\" DROP DEFAULT"),
            format!("CREATE INDEX \"users_nick_idx\" ON {t} (\"nickname\")"),
            format!("ALTER TABLE {t} ADD CONSTRAINT \"users_team_fk\" FOREIGN KEY (\"id\") REFERENCES \"teams\" (\"id\") ON DELETE SET NULL"),
        ];
        assert_eq!(s, want);
        assert!(alter_table_sql(Postgres, &from, &from).is_empty());
    }

    #[test]
    fn alter_mysql_and_mssql() {
        let (mut from, mut to) = altered();
        for t in [&mut from, &mut to] {
            t.table = Some(TableRef { database: Some("app".into()), schema: Option::None, name: "users".into() });
        }
        let s = alter_table_sql(Mysql, &from, &to);
        assert!(s.contains(&"DROP INDEX `users_profile_gin` ON `app`.`users`".to_string()), "{s:#?}");
        assert!(s.contains(&"ALTER TABLE `app`.`users` MODIFY COLUMN `email` varchar(320)".to_string()), "{s:#?}");
        assert!(s.contains(&"ALTER TABLE `app`.`users` MODIFY COLUMN `balance` numeric(12,2)".to_string()), "{s:#?}");

        let s = alter_table_sql(Mssql, &from, &to);
        assert!(s.iter().any(|x| x.starts_with("DECLARE @df sysname") && x.contains("N'balance'")), "{s:#?}");
        assert!(s.iter().any(|x| x == "ALTER TABLE [app]..[users] ALTER COLUMN [email] varchar(320) NULL"), "{s:#?}");
        assert!(s.iter().any(|x| x == "ALTER TABLE [app]..[users] ADD [nickname] varchar(40) DEFAULT 'x'"), "{s:#?}");
        // email is indexed: index is dropped before and recreated after ALTER COLUMN
        let drop_pos = s.iter().position(|x| x.starts_with("DROP INDEX [users_email_key]")).unwrap();
        let alter_pos = s.iter().position(|x| x.contains("ALTER COLUMN [email]")).unwrap();
        let create_pos = s.iter().position(|x| x.starts_with("CREATE UNIQUE INDEX [users_email_key]")).unwrap();
        assert!(drop_pos < alter_pos && alter_pos < create_pos);
        assert!(s.iter().all(|x| !x.contains(';')), "{s:#?}");
    }

    #[test]
    fn alter_primary_key() {
        let from = users_pg();
        let mut to = users_pg();
        for c in &mut to.columns {
            c.is_primary_key = c.name == "id" || c.name == "email";
        }
        to.indexes[0].columns = vec!["id".into(), "email".into()];
        let s = alter_table_sql(Postgres, &from, &to);
        assert_eq!(s, vec![
            "ALTER TABLE \"public\".\"users\" DROP CONSTRAINT \"users_pkey\"".to_string(),
            "ALTER TABLE \"public\".\"users\" ADD PRIMARY KEY (\"id\", \"email\")".to_string(),
        ]);
        let s = alter_table_sql(Mysql, &from, &to);
        assert!(s[0].ends_with("DROP PRIMARY KEY"));
    }

    #[test]
    fn alter_sqlite() {
        let base = TableStructure {
            table: Some(tref(Option::None, "t")),
            columns: vec![col("id", "integer", false, true, true, Option::None, 1), col("a", "text", true, false, false, Option::None, 2)],
            indexes: vec![IndexDef { name: "t_a".into(), columns: vec!["a".into()], unique: false, primary: false, method: Option::None }],
            ..Default::default()
        };
        // simple add column -> ALTER TABLE ADD COLUMN
        let mut add = base.clone();
        add.columns.push(col("b", "integer", true, false, false, Some("0"), 3));
        assert_eq!(alter_table_sql(Sqlite, &base, &add), vec!["ALTER TABLE \"t\" ADD COLUMN \"b\" integer DEFAULT 0".to_string()]);
        // type change -> rebuild
        let mut change = base.clone();
        change.columns[1].data_type = "varchar(10)".into();
        let s = alter_table_sql(Sqlite, &base, &change);
        assert_eq!(s.first().unwrap(), "PRAGMA foreign_keys=OFF");
        assert!(s[1].starts_with("CREATE TABLE \"_devcat_tmp_t\" (\n  \"id\" integer PRIMARY KEY AUTOINCREMENT NOT NULL,\n  \"a\" varchar(10)"), "{s:#?}");
        assert_eq!(s[2], "INSERT INTO \"_devcat_tmp_t\" (\"id\", \"a\") SELECT \"id\", \"a\" FROM \"t\"");
        assert_eq!(s[3], "DROP TABLE \"t\"");
        assert_eq!(s[4], "ALTER TABLE \"_devcat_tmp_t\" RENAME TO \"t\"");
        assert_eq!(s[5], "CREATE INDEX \"t_a\" ON \"t\" (\"a\")");
        assert_eq!(s.last().unwrap(), "PRAGMA foreign_keys=ON");
    }

    #[test]
    fn dependency_sorting() {
        let mk = |name: &str, refs: &[&str]| TableStructure {
            table: Some(tref(Option::None, name)),
            foreign_keys: refs
                .iter()
                .map(|r| ForeignKeyDef { name: format!("{name}_{r}"), ref_table: r.to_string(), ..Default::default() })
                .collect(),
            ..Default::default()
        };
        let tables = vec![mk("order_items", &["orders", "products"]), mk("orders", &["users"]), mk("users", &["users"]), mk("products", &[])];
        let (order, cyclic) = dependency_order(&tables);
        let names: Vec<_> = order.iter().map(|&i| tables[i].table.as_ref().unwrap().name.as_str()).collect();
        assert_eq!(names, vec!["users", "orders", "products", "order_items"]);
        assert!(cyclic.is_empty());

        let tables = vec![mk("a", &["b"]), mk("b", &["a"]), mk("c", &[])];
        let (order, cyclic) = dependency_order(&tables);
        assert_eq!(order, vec![2, 0, 1]);
        assert_eq!(cyclic, HashSet::from([0, 1]));
    }
}
