//! Generic data transfer built on top of `Driver`: export (CSV/JSON/SQL/XLSX),
//! import (CSV/JSON), SQL dump backup and restore, and table copy between
//! connections. Nothing here knows about concrete drivers.
//!
//! Progress is reported through a `&Progress` callback receiving JSON objects
//! (`{"rows", "done", ...}`); the command layer forwards them as
//! `transfer-progress` events with the job id attached.
use super::ddl::{self, CreateTableOptions, TypeClass};
use super::sql_common::{is_read_only_statement, literal, quote_ident, split_statements};
use super::{
    ColumnDef, ColumnInfo, Dialect, DynDriver, ExecOptions, NodeKind, RowChange, SortSpec, TableDataRequest, TableRef,
    TableStructure,
};
use crate::error::{AppError, AppResult};
use crate::models::ConnectionKind;
use base64::Engine;
use serde::de::{SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub type Progress<'a> = dyn Fn(Value) + Send + Sync + 'a;

/// Record read from an import file: (file column, value) in file order.
pub type Record = Vec<(String, Value)>;

// ---------------------------------------------------------------------------
// Options helper
// ---------------------------------------------------------------------------

/// Read-only accessor over the free-form `options` object sent by the frontend.
/// Keys may list aliases separated by `|` (`"nullAs|nullText"`); the first present wins.
#[derive(Clone, Copy)]
pub struct Opts<'a>(pub &'a Value);

impl<'a> Opts<'a> {
    fn get(&self, k: &str) -> Option<&'a Value> {
        k.split('|').find_map(|k| self.0.get(k).filter(|v| !v.is_null()))
    }
    pub fn str(&self, k: &str) -> Option<&'a str> {
        self.get(k).and_then(Value::as_str)
    }
    pub fn bool(&self, k: &str, default: bool) -> bool {
        match self.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => parse_bool(s).unwrap_or(default),
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            _ => default,
        }
    }
    pub fn u64(&self, k: &str, default: u64) -> u64 {
        match self.get(k) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(default),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
            _ => default,
        }
    }
    /// Single byte option (delimiter/quote). Accepts "\t"/"tab" for tabs.
    pub fn byte(&self, k: &str, default: u8) -> AppResult<u8> {
        match self.str(k) {
            None | Some("") => Ok(default),
            Some("\\t") | Some("tab") | Some("\t") => Ok(b'\t'),
            Some(s) if s.len() == 1 => Ok(s.as_bytes()[0]),
            Some(s) => Err(AppError::msg(format!("option {k} must be a single ASCII character, got {s:?}"))),
        }
    }
    pub fn strings(&self, k: &str) -> Option<Vec<String>> {
        let arr = self.get(k)?.as_array()?;
        Some(arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" | "on" => Some(true),
        "false" | "f" | "no" | "n" | "0" | "off" => Some(false),
        _ => None,
    }
}

pub fn parse_dialect(s: &str) -> AppResult<Dialect> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "pg" => Dialect::Postgres,
        "mysql" | "mariadb" => Dialect::Mysql,
        "sqlite" => Dialect::Sqlite,
        "mssql" | "sqlserver" | "sql server" => Dialect::Mssql,
        other => return Err(AppError::msg(format!("unknown SQL dialect {other:?}"))),
    })
}

fn exec_opts(database: Option<&str>, schema: Option<&str>) -> ExecOptions {
    ExecOptions { database: database.map(Into::into), schema: schema.map(Into::into), max_rows: Some(100) }
}

// ---------------------------------------------------------------------------
// Value formatting / coercion
// ---------------------------------------------------------------------------

/// Text form of a cell for CSV; `None` for NULL.
pub fn cell_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// `"\x0aff"` (driver binary encoding) -> `"0aff"`.
fn binary_hex(s: &str) -> Option<&str> {
    let h = s.strip_prefix("\\x")?;
    (h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

fn hex_literal(d: Dialect, hex: &str) -> String {
    match d {
        Dialect::Postgres | Dialect::None => format!("'\\x{hex}'::bytea"),
        Dialect::Mssql => format!("0x{hex}"),
        _ => format!("X'{hex}'"),
    }
}

fn bool_literal(d: Dialect, b: bool) -> String {
    match d {
        Dialect::Postgres => if b { "TRUE" } else { "FALSE" }.into(),
        _ => if b { "1" } else { "0" }.into(),
    }
}

/// Render a cell as a SQL literal for dialect `d`, using the column class to
/// produce proper booleans and binary literals.
pub fn sql_value(d: Dialect, v: &Value, class: TypeClass) -> String {
    match (class, v) {
        (_, Value::Null) => "NULL".into(),
        (TypeClass::Bool, Value::Number(n)) => bool_literal(d, n.as_f64() != Some(0.0)),
        (TypeClass::Bool, Value::String(s)) if parse_bool(s).is_some() => bool_literal(d, parse_bool(s).unwrap_or(false)),
        (_, Value::Bool(b)) => bool_literal(d, *b),
        (TypeClass::Binary, Value::String(s)) => {
            if let Some(h) = binary_hex(s) {
                return hex_literal(d, h);
            }
            match base64::engine::general_purpose::STANDARD.decode(s) {
                Ok(bytes) if !s.is_empty() => hex_literal(d, &hex::encode(bytes)),
                _ => literal(d, v),
            }
        }
        (_, Value::Object(_) | Value::Array(_)) => literal(d, &Value::String(v.to_string())),
        _ => literal(d, v),
    }
}

/// MongoDB extended JSON scalar wrappers (`{"$oid": ..}`, `{"$date": ..}`, ...) -> plain values.
fn unwrap_extended_json(v: Value) -> Value {
    let Value::Object(m) = &v else { return v };
    if m.len() != 1 {
        return v;
    }
    let Some((k, inner)) = m.iter().next() else { return v };
    match (k.as_str(), inner) {
        ("$oid" | "$numberDecimal" | "$uuid" | "$symbol", Value::String(s)) => Value::String(s.clone()),
        ("$numberLong" | "$numberInt", Value::String(s)) => s.parse::<i64>().map(Value::from).unwrap_or(v),
        ("$numberDouble", Value::String(s)) => s.parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::from).unwrap_or(v),
        ("$date", Value::String(s)) => Value::String(s.clone()),
        ("$date", Value::Number(n)) => n
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| Value::String(d.to_rfc3339()))
            .unwrap_or(v),
        ("$date", Value::Object(_)) => match unwrap_extended_json(inner.clone()) {
            Value::Number(n) => unwrap_extended_json(json!({ "$date": n })),
            _ => v,
        },
        _ => v,
    }
}

/// `2024-01-01 10:00:00+00` -> `2024-01-01 10:00:00+00:00` (MSSQL/MySQL need full offsets).
fn normalize_offset(s: String) -> String {
    let b = s.as_bytes();
    let n = b.len();
    if n > 10 && matches!(b[n - 3], b'+' | b'-') && b[n - 2].is_ascii_digit() && b[n - 1].is_ascii_digit() && b[n - 4].is_ascii_digit() {
        format!("{s}:00")
    } else {
        s
    }
}

/// Convert an incoming value to what a column of `class` expects
/// (import from text files, copy between dialects).
pub fn coerce(class: Option<TypeClass>, v: Value) -> Value {
    let Some(class) = class else { return v };
    match (class, unwrap_extended_json(v)) {
        (TypeClass::Temporal, Value::String(s)) => Value::String(normalize_offset(s)),
        (_, Value::Null) => Value::Null,
        (TypeClass::Integer, Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(i) => Value::from(i),
            Err(_) => match parse_bool(&s) {
                Some(b) if !s.trim().chars().all(|c| c.is_ascii_digit()) => Value::from(b as i64),
                _ => Value::String(s),
            },
        },
        (TypeClass::Integer, Value::Bool(b)) => Value::from(b as i64),
        (TypeClass::Integer, Value::Number(n)) if !n.is_i64() && !n.is_u64() => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => Value::from(f as i64),
            _ => Value::Number(n),
        },
        (TypeClass::Float, Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => Value::from(f),
            _ => Value::String(s),
        },
        (TypeClass::Decimal, Value::String(s)) => {
            // keep decimals as text unless they survive an f64 round trip
            let t = s.trim();
            match t.parse::<f64>() {
                Ok(f) if f.is_finite() && (f.to_string() == t || format!("{f:.1}") == t) => Value::from(f),
                _ => Value::String(s),
            }
        }
        (TypeClass::Bool, Value::String(s)) => match parse_bool(&s) {
            Some(b) => Value::Bool(b),
            None => Value::String(s),
        },
        (TypeClass::Bool, Value::Number(n)) => Value::Bool(n.as_f64() != Some(0.0)),
        (TypeClass::Text | TypeClass::Uuid | TypeClass::Temporal | TypeClass::Other, v @ (Value::Object(_) | Value::Array(_))) => {
            Value::String(v.to_string())
        }
        (TypeClass::Text | TypeClass::Uuid, Value::Number(n)) => Value::String(n.to_string()),
        (TypeClass::Text, Value::Bool(b)) => Value::String(b.to_string()),
        (_, v) => v,
    }
}

/// Type inference for schemaless targets (MongoDB) when reading text files.
fn infer_value(v: Value) -> Value {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>()
                && !(t.len() > 1 && t.starts_with('0'))
            {
                return Value::from(i);
            }
            if t.contains('.')
                && let Ok(f) = t.parse::<f64>()
                && f.is_finite()
            {
                return Value::from(f);
            }
            match t {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::String(s),
            }
        }
        v => v,
    }
}

// ---------------------------------------------------------------------------
// INSERT generation
// ---------------------------------------------------------------------------

/// One multi-row INSERT statement (no trailing `;`). For MSSQL tables with an
/// identity column (`identity == true`) the statement is wrapped in
/// `SET IDENTITY_INSERT` without semicolons so script splitters keep it together.
pub fn insert_sql(d: Dialect, table: &str, cols: &[String], classes: &[TypeClass], rows: &[Vec<Value>], identity: bool) -> String {
    let col_list = cols.iter().map(|c| quote_ident(d, c)).collect::<Vec<_>>().join(", ");
    let values = rows
        .iter()
        .map(|r| {
            let cells = r
                .iter()
                .enumerate()
                .map(|(i, v)| sql_value(d, v, classes.get(i).copied().unwrap_or(TypeClass::Other)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({cells})")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let insert = format!("INSERT INTO {table} ({col_list}) VALUES\n{values}");
    if identity && d == Dialect::Mssql {
        format!("SET IDENTITY_INSERT {table} ON\n{insert}\nSET IDENTITY_INSERT {table} OFF")
    } else {
        insert
    }
}

fn max_insert_batch(d: Dialect, requested: usize) -> usize {
    let n = requested.max(1);
    if d == Dialect::Mssql { n.min(1000) } else { n }
}

/// Statement resetting identity/serial sequences after explicit-id inserts (Postgres only).
fn sequence_reset_sql(d: Dialect, table: &TableRef, structure: &TableStructure) -> Vec<String> {
    if d != Dialect::Postgres {
        return vec![];
    }
    let name = ddl::qualified_name(d, table);
    structure
        .columns
        .iter()
        .filter(|c| c.auto_increment)
        .map(|c| {
            format!(
                "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE(MAX({}), 0) + 1, false) FROM {name}",
                literal(d, &Value::String(name.clone())),
                literal(d, &Value::String(c.name.clone())),
                quote_ident(d, &c.name)
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Inserting rows through a driver
// ---------------------------------------------------------------------------

/// Target table info used to coerce and insert rows.
struct InsertTarget {
    table: TableRef,
    dialect: Dialect,
    /// lowercase column name -> (real name, class)
    columns: HashMap<String, (String, TypeClass)>,
    /// MSSQL identity columns (explicit values need IDENTITY_INSERT)
    identity: HashSet<String>,
    structure: Option<TableStructure>,
}

impl InsertTarget {
    fn new(dialect: Dialect, table: TableRef, structure: Option<TableStructure>) -> Self {
        let mut columns = HashMap::new();
        let mut identity = HashSet::new();
        if let Some(s) = &structure {
            for c in &s.columns {
                columns.insert(c.name.to_lowercase(), (c.name.clone(), ddl::type_class(dialect, &c.data_type)));
                if c.auto_increment && dialect == Dialect::Mssql {
                    identity.insert(c.name.clone());
                }
            }
        }
        Self { table, dialect, columns, identity, structure }
    }

    fn column(&self, name: &str) -> Option<&(String, TypeClass)> {
        self.columns.get(&name.to_lowercase())
    }

    fn coerce_row(&self, row: Map<String, Value>) -> Map<String, Value> {
        row.into_iter()
            .map(|(k, v)| {
                let class = self.column(&k).map(|c| c.1);
                (k, coerce(class, v))
            })
            .collect()
    }

    async fn insert(&self, driver: &DynDriver, rows: Vec<Map<String, Value>>) -> AppResult<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let n = rows.len() as u64;
        let needs_identity_insert = !self.identity.is_empty()
            && rows.iter().any(|r| self.identity.iter().any(|c| r.get(c).is_some_and(|v| !v.is_null())));
        if needs_identity_insert {
            // MSSQL rejects explicit identity values unless IDENTITY_INSERT is on.
            let mut cols: Vec<String> = Vec::new();
            for r in &rows {
                for k in r.keys() {
                    if !cols.contains(k) {
                        cols.push(k.clone());
                    }
                }
            }
            let classes: Vec<TypeClass> =
                cols.iter().map(|c| self.column(c).map(|x| x.1).unwrap_or(TypeClass::Other)).collect();
            let name = ddl::qualified_name(self.dialect, &self.table);
            let opts = exec_opts(self.table.database.as_deref(), self.table.schema.as_deref());
            for chunk in rows.chunks(1000) {
                let values: Vec<Vec<Value>> =
                    chunk.iter().map(|r| cols.iter().map(|c| r.get(c).cloned().unwrap_or(Value::Null)).collect()).collect();
                driver.execute(&insert_sql(self.dialect, &name, &cols, &classes, &values, true), &opts).await?;
            }
            return Ok(n);
        }
        let changes: Vec<RowChange> = rows.into_iter().map(|values| RowChange::Insert { values }).collect();
        driver.apply_changes(&self.table, &changes).await?;
        Ok(n)
    }

    /// Advance Postgres sequences past explicitly inserted ids. Errors are ignored.
    async fn finish(&self, driver: &DynDriver) {
        if let Some(s) = &self.structure {
            let opts = exec_opts(self.table.database.as_deref(), self.table.schema.as_deref());
            for sql in sequence_reset_sql(self.dialect, &self.table, s) {
                let _ = driver.execute(&sql, &opts).await;
            }
        }
    }
}

fn rows_to_maps(cols: &[ColumnInfo], rows: Vec<Vec<Value>>) -> Vec<Map<String, Value>> {
    rows.into_iter()
        .map(|r| cols.iter().map(|c| c.name.clone()).zip(r).collect::<Map<String, Value>>())
        .collect()
}

fn pk_sort(structure: Option<&TableStructure>) -> Vec<SortSpec> {
    structure
        .map(ddl::primary_key_columns)
        .unwrap_or_default()
        .into_iter()
        .map(|column| SortSpec { column, desc: false })
        .collect()
}

async fn delete_all(driver: &DynDriver, table: &TableRef) -> AppResult<()> {
    let d = driver.dialect();
    let opts = exec_opts(table.database.as_deref(), table.schema.as_deref());
    let sql = if d == Dialect::None {
        format!("db.getCollection({}).deleteMany({{}})", serde_json::to_string(&table.name)?)
    } else {
        format!("DELETE FROM {}", ddl::qualified_name(d, table))
    };
    driver.execute(&sql, &opts).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// Export sinks
// ---------------------------------------------------------------------------

trait Sink: Send {
    fn begin(&mut self, cols: &[ColumnInfo]) -> AppResult<()>;
    /// `cols` describes `rows`; it can differ between pages for schemaless sources.
    fn write(&mut self, cols: &[ColumnInfo], rows: &[Vec<Value>]) -> AppResult<()>;
    fn finish(self: Box<Self>) -> AppResult<()>;
}

/// Reorders `row` (described by `cols`) to `header` order; unknown columns become NULL.
fn remap(header: &[String], cols: &[ColumnInfo], rows: &[Vec<Value>]) -> Option<Vec<Vec<Value>>> {
    if header.len() == cols.len() && header.iter().zip(cols).all(|(h, c)| *h == c.name) {
        return None;
    }
    let idx: Vec<Option<usize>> = header.iter().map(|h| cols.iter().position(|c| c.name == *h)).collect();
    Some(
        rows.iter()
            .map(|r| idx.iter().map(|i| i.and_then(|i| r.get(i).cloned()).unwrap_or(Value::Null)).collect())
            .collect(),
    )
}

fn create_file(path: &Path) -> AppResult<BufWriter<File>> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    Ok(BufWriter::with_capacity(256 * 1024, File::create(path)?))
}

struct CsvSink {
    w: csv::Writer<BufWriter<File>>,
    header: Vec<String>,
    write_header: bool,
    null_as: String,
}

impl CsvSink {
    fn new(path: &Path, o: Opts) -> AppResult<Self> {
        let mut file = create_file(path)?;
        if o.bool("bom", false) {
            file.write_all(b"\xEF\xBB\xBF")?;
        }
        let terminator = match o.str("lineEnding").unwrap_or("\n") {
            "\r\n" | "crlf" | "CRLF" => csv::Terminator::CRLF,
            "\r" | "cr" | "CR" => csv::Terminator::Any(b'\r'),
            _ => csv::Terminator::Any(b'\n'),
        };
        let w = csv::WriterBuilder::new()
            .delimiter(o.byte("delimiter", b',')?)
            .quote(o.byte("quote", b'"')?)
            .terminator(terminator)
            .flexible(true)
            .from_writer(file);
        Ok(Self {
            w,
            header: vec![],
            write_header: o.bool("header", true),
            null_as: o.str("nullAs|nullText").unwrap_or("").to_string(),
        })
    }
}

fn csv_err(e: csv::Error) -> AppError {
    AppError::msg(format!("CSV: {e}"))
}

impl Sink for CsvSink {
    fn begin(&mut self, cols: &[ColumnInfo]) -> AppResult<()> {
        self.header = cols.iter().map(|c| c.name.clone()).collect();
        if self.write_header && !self.header.is_empty() {
            self.w.write_record(&self.header).map_err(csv_err)?;
        }
        Ok(())
    }
    fn write(&mut self, cols: &[ColumnInfo], rows: &[Vec<Value>]) -> AppResult<()> {
        let remapped = remap(&self.header, cols, rows);
        for r in remapped.as_deref().unwrap_or(rows) {
            let rec: Vec<String> = r.iter().map(|v| cell_text(v).unwrap_or_else(|| self.null_as.clone())).collect();
            self.w.write_record(&rec).map_err(csv_err)?;
        }
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> AppResult<()> {
        self.w.flush()?;
        Ok(())
    }
}

struct JsonSink {
    w: BufWriter<File>,
    pretty: bool,
    lines: bool,
    first: bool,
}

impl JsonSink {
    fn object(&self, cols: &[ColumnInfo], row: &[Value]) -> AppResult<String> {
        // Hand-rolled so keys keep column order.
        let mut parts = Vec::with_capacity(cols.len());
        for (c, v) in cols.iter().zip(row) {
            let key = serde_json::to_string(&c.name)?;
            let val = if self.pretty && !self.lines {
                serde_json::to_string_pretty(v)?.replace('\n', "\n    ")
            } else {
                serde_json::to_string(v)?
            };
            parts.push(if self.pretty && !self.lines { format!("    {key}: {val}") } else { format!("{key}:{val}") });
        }
        Ok(if self.pretty && !self.lines {
            format!("  {{\n{}\n  }}", parts.join(",\n"))
        } else {
            format!("{{{}}}", parts.join(","))
        })
    }
}

impl Sink for JsonSink {
    fn begin(&mut self, _cols: &[ColumnInfo]) -> AppResult<()> {
        if !self.lines {
            self.w.write_all(b"[")?;
        }
        Ok(())
    }
    fn write(&mut self, cols: &[ColumnInfo], rows: &[Vec<Value>]) -> AppResult<()> {
        for r in rows {
            let obj = self.object(cols, r)?;
            if self.lines {
                self.w.write_all(obj.as_bytes())?;
                self.w.write_all(b"\n")?;
                continue;
            }
            if !self.first {
                self.w.write_all(b",")?;
            }
            if self.pretty {
                self.w.write_all(b"\n")?;
            }
            self.w.write_all(obj.as_bytes())?;
            self.first = false;
        }
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> AppResult<()> {
        if !self.lines {
            self.w.write_all(if self.pretty && !self.first { b"\n]\n" } else { b"]\n" })?;
        }
        self.w.flush()?;
        Ok(())
    }
}

struct SqlSink {
    w: BufWriter<File>,
    source: Dialect,
    target: Dialect,
    table: TableRef,
    structure: Option<TableStructure>,
    create: bool,
    drop: bool,
    batch: usize,
    identity: bool,
}

impl Sink for SqlSink {
    fn begin(&mut self, cols: &[ColumnInfo]) -> AppResult<()> {
        let name = ddl::qualified_name(self.target, &self.table);
        writeln!(self.w, "-- DevCat SQL export of {name}")?;
        writeln!(self.w, "-- Date: {}\n", chrono::Local::now().to_rfc3339())?;
        let structure = self.structure.clone().unwrap_or_else(|| TableStructure {
            table: None,
            columns: cols
                .iter()
                .enumerate()
                .map(|(i, c)| ColumnDef {
                    name: c.name.clone(),
                    data_type: c.type_name.clone(),
                    nullable: true,
                    ordinal: i as u32 + 1,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
        let converted = ddl::convert_structure(self.source, self.target, &structure);
        self.identity = self.target == Dialect::Mssql && converted.columns.iter().any(|c| c.auto_increment);
        if self.drop {
            writeln!(self.w, "{};", ddl::drop_table_sql(self.target, &self.table, true))?;
        }
        if self.create {
            let opts = CreateTableOptions { foreign_keys: false, ..Default::default() };
            writeln!(self.w, "{}\n", ddl::create_table_sql_with(self.target, &converted, &self.table, &opts))?;
        }
        Ok(())
    }
    fn write(&mut self, cols: &[ColumnInfo], rows: &[Vec<Value>]) -> AppResult<()> {
        let names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
        let classes: Vec<TypeClass> = cols.iter().map(|c| ddl::type_class(self.source, &c.type_name)).collect();
        let name = ddl::qualified_name(self.target, &self.table);
        for chunk in rows.chunks(max_insert_batch(self.target, self.batch)) {
            writeln!(self.w, "{};", insert_sql(self.target, &name, &names, &classes, chunk, self.identity))?;
        }
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> AppResult<()> {
        self.w.flush()?;
        Ok(())
    }
}

struct XlsxSink {
    path: PathBuf,
    workbook: rust_xlsxwriter::Workbook,
    sheet: rust_xlsxwriter::Worksheet,
    header: Vec<String>,
    write_header: bool,
    row: u32,
}

fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> AppError {
    AppError::msg(format!("XLSX: {e}"))
}

impl XlsxSink {
    fn new(path: &Path, o: Opts) -> AppResult<Self> {
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let mut sheet = workbook.new_worksheet_with_constant_memory();
        let name: String = o.str("sheetName").unwrap_or("Sheet1").chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
        sheet.set_name(if name.is_empty() { "Sheet1" } else { &name }).map_err(xlsx_err)?;
        Ok(Self { path: path.to_path_buf(), workbook, sheet, header: vec![], write_header: o.bool("header", true), row: 0 })
    }
}

impl Sink for XlsxSink {
    fn begin(&mut self, cols: &[ColumnInfo]) -> AppResult<()> {
        self.header = cols.iter().map(|c| c.name.clone()).collect();
        if self.write_header && !self.header.is_empty() {
            let bold = rust_xlsxwriter::Format::new().set_bold();
            for (i, h) in self.header.iter().enumerate() {
                self.sheet.write_string_with_format(0, i as u16, h.as_str(), &bold).map_err(xlsx_err)?;
            }
            self.row = 1;
        }
        Ok(())
    }
    fn write(&mut self, cols: &[ColumnInfo], rows: &[Vec<Value>]) -> AppResult<()> {
        const MAX_ROWS: u32 = 1_048_576;
        let remapped = remap(&self.header, cols, rows);
        for r in remapped.as_deref().unwrap_or(rows) {
            if self.row >= MAX_ROWS {
                return Err(AppError::msg("XLSX: more than 1,048,576 rows; use CSV instead"));
            }
            for (i, v) in r.iter().enumerate().take(16_384) {
                let c = i as u16;
                match v {
                    Value::Null => {}
                    Value::Bool(b) => {
                        self.sheet.write_boolean(self.row, c, *b).map_err(xlsx_err)?;
                    }
                    Value::Number(n) if n.as_f64().is_some_and(|f| f.abs() < 1e15) => {
                        self.sheet.write_number(self.row, c, n.as_f64().unwrap_or(0.0)).map_err(xlsx_err)?;
                    }
                    other => {
                        let s: String = cell_text(other).unwrap_or_default().chars().take(32_767).collect();
                        self.sheet.write_string(self.row, c, s).map_err(xlsx_err)?;
                    }
                }
            }
            self.row += 1;
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> AppResult<()> {
        let XlsxSink { path, mut workbook, sheet, .. } = *self;
        workbook.push_worksheet(sheet);
        workbook.save(&path).map_err(xlsx_err)
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

pub enum ExportSource {
    Table(TableRef),
    Query { sql: String, database: Option<String> },
}

/// Export a table (paged) or a query result to `path`. Returns rows written.
pub async fn export_data(
    driver: &DynDriver,
    source: ExportSource,
    format: &str,
    path: &Path,
    options: &Value,
    progress: &Progress<'_>,
) -> AppResult<u64> {
    let o = Opts(options);
    let src_dialect = driver.dialect();
    // Exporting a query result runs the query again: refuse anything that would change data a
    // second time (e.g. `DELETE … RETURNING`, `INSERT … RETURNING`), before any file is created.
    if let ExportSource::Query { sql, .. } = &source
        && src_dialect != Dialect::None
        && let Some(stmt) = split_statements(src_dialect, sql).into_iter().find(|s| !is_read_only_statement(src_dialect, s))
    {
        let head: String = stmt.trim().chars().take(60).collect();
        return Err(AppError::msg(format!(
            "Export runs the query again, and this statement changes data: {head}… Export the table instead, or run a SELECT and export that."
        )));
    }
    let structure = match &source {
        ExportSource::Table(t) => driver.table_structure(t).await.ok(),
        ExportSource::Query { .. } => None,
    };
    let mut sink: Box<dyn Sink> = match format.to_ascii_lowercase().as_str() {
        "csv" | "tsv" => {
            let mut opts = options.clone();
            if format.eq_ignore_ascii_case("tsv") && o.str("delimiter").is_none() {
                opts["delimiter"] = json!("\t");
            }
            Box::new(CsvSink::new(path, Opts(&opts))?)
        }
        "json" | "jsonl" | "ndjson" => Box::new(JsonSink {
            w: create_file(path)?,
            pretty: o.bool("pretty|jsonPretty", true),
            lines: o.bool("lines", format != "json"),
            first: true,
        }),
        "sql" => {
            let target = match o.str("targetDialect") {
                Some(s) => parse_dialect(s)?,
                None if src_dialect == Dialect::None => Dialect::Postgres,
                None => src_dialect,
            };
            let base = match &source {
                ExportSource::Table(t) => t.clone(),
                ExportSource::Query { .. } => TableRef { database: None, schema: None, name: "query_result".into() },
            };
            let table = TableRef {
                database: None,
                schema: if target == src_dialect && o.bool("qualified", true) && target != Dialect::Mysql {
                    base.schema.clone()
                } else {
                    None
                },
                name: o.str("tableName|sqlTableName").map(str::to_string).unwrap_or(base.name),
            };
            Box::new(SqlSink {
                w: create_file(path)?,
                source: src_dialect,
                target,
                table,
                structure: structure.clone(),
                create: o.bool("includeCreate|sqlIncludeCreate", false),
                drop: o.bool("includeDrop|sqlIncludeDrop", false),
                batch: o.u64("insertBatch", 100) as usize,
                identity: false,
            })
        }
        "xlsx" | "excel" => Box::new(XlsxSink::new(path, o)?),
        other => return Err(AppError::Unsupported(format!("export format {other}"))),
    };

    let mut written: u64 = 0;
    match source {
        ExportSource::Query { sql, database } => {
            let max_rows = o.u64("maxRows", 1_000_000) as usize;
            let opts = ExecOptions { database, schema: o.str("schema").map(Into::into), max_rows: Some(max_rows) };
            let results = driver.execute(&sql, &opts).await?;
            let res = results.into_iter().rev().find(|r| !r.columns.is_empty()).unwrap_or_default();
            sink.begin(&res.columns)?;
            let chunk = 5_000;
            for rows in res.rows.chunks(chunk) {
                sink.write(&res.columns, rows)?;
                written += rows.len() as u64;
                progress(json!({ "rows": written, "done": false }));
            }
        }
        ExportSource::Table(table) => {
            let batch = o.u64("batchSize", 1000).max(1);
            let sort = pk_sort(structure.as_ref());
            let filter = o.str("filter").filter(|f| !f.trim().is_empty()).map(str::to_string);
            let mut begun = false;
            let mut offset = 0u64;
            loop {
                let req = TableDataRequest { table: table.clone(), offset, limit: batch, filter: filter.clone(), sort: sort.clone() };
                let page = driver.table_data(&req).await?;
                let res = page.result;
                if !begun {
                    sink.begin(&res.columns)?;
                    begun = true;
                }
                let n = res.rows.len() as u64;
                sink.write(&res.columns, &res.rows)?;
                written += n;
                progress(json!({ "rows": written, "total": page.total, "done": false }));
                if n < batch {
                    break;
                }
                offset += n;
            }
        }
    }
    sink.finish()?;
    progress(json!({ "rows": written, "done": true }));
    Ok(written)
}

// ---------------------------------------------------------------------------
// Import file reading
// ---------------------------------------------------------------------------

struct ReadOptions {
    json: bool,
    delimiter: u8,
    quote: u8,
    header: bool,
    skip_rows: usize,
    empty_as_null: bool,
    null_as: Option<String>,
}

impl ReadOptions {
    fn new(format: &str, o: Opts) -> AppResult<Self> {
        let f = format.to_ascii_lowercase();
        let json = match f.as_str() {
            "csv" | "tsv" | "txt" => false,
            "json" | "jsonl" | "ndjson" => true,
            other => return Err(AppError::Unsupported(format!("import format {other}"))),
        };
        Ok(Self {
            json,
            delimiter: o.byte("delimiter", if f == "tsv" { b'\t' } else { b',' })?,
            quote: o.byte("quote", b'"')?,
            header: o.bool("header", true),
            skip_rows: o.u64("skipRows", 0) as usize,
            empty_as_null: o.bool("emptyAsNull", !json),
            null_as: o.str("nullAs|nullText").filter(|s| !s.is_empty()).map(str::to_string),
        })
    }

    fn cell(&self, v: Value) -> Value {
        match v {
            Value::String(s) if (s.is_empty() && self.empty_as_null) || self.null_as.as_deref() == Some(s.as_str()) => {
                Value::Null
            }
            v => v,
        }
    }
}

/// Stream records from a CSV/JSON file. `on_record` returns `false` to stop early.
/// Returns the column names in order of first appearance.
fn read_records(path: &Path, ro: &ReadOptions, mut on_record: impl FnMut(Record) -> bool) -> AppResult<Vec<String>> {
    let file = File::open(path).map_err(|e| AppError::msg(format!("cannot open {}: {e}", path.display())))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    if ro.json { read_json(reader, ro, on_record) } else {
        for _ in 0..ro.skip_rows {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
        }
        let mut csv = csv::ReaderBuilder::new()
            .delimiter(ro.delimiter)
            .quote(ro.quote)
            .has_headers(false)
            .flexible(true)
            .from_reader(reader);
        let mut columns: Vec<String> = Vec::new();
        let mut first = true;
        for rec in csv.records() {
            let rec = rec.map_err(|e| match e.kind() {
                csv::ErrorKind::Utf8 { .. } => AppError::msg("file is not valid UTF-8 (only UTF-8 is supported)"),
                _ => csv_err(e),
            })?;
            if first && ro.header {
                first = false;
                columns = rec.iter().enumerate().map(|(i, h)| {
                    let h = h.trim().trim_start_matches('\u{feff}');
                    if h.is_empty() { format!("column{}", i + 1) } else { h.to_string() }
                }).collect();
                continue;
            }
            first = false;
            if rec.len() == 1 && rec.get(0) == Some("") {
                continue; // blank line
            }
            while columns.len() < rec.len() {
                columns.push(format!("column{}", columns.len() + 1));
            }
            let record: Record =
                rec.iter().enumerate().map(|(i, v)| (columns[i].clone(), ro.cell(Value::String(v.to_string())))).collect();
            if !on_record(record) {
                break;
            }
        }
        Ok(columns)
    }
}

fn object_record(v: Value, ro: &ReadOptions, columns: &mut Vec<String>, seen: &mut HashSet<String>) -> AppResult<Record> {
    let Value::Object(map) = v else {
        return Err(AppError::msg("JSON import expects an array of objects or one object per line"));
    };
    let mut rec = Vec::with_capacity(map.len());
    for (k, v) in map {
        if seen.insert(k.clone()) {
            columns.push(k.clone());
        }
        rec.push((k, ro.cell(v)));
    }
    Ok(rec)
}

fn read_json<R: Read>(mut reader: BufReader<R>, ro: &ReadOptions, mut on_record: impl FnMut(Record) -> bool) -> AppResult<Vec<String>> {
    // Peek the first significant byte (skipping a UTF-8 BOM) to pick array vs JSON lines.
    let first = loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break None;
        }
        if buf.starts_with(b"\xEF\xBB\xBF") {
            reader.consume(3);
            continue;
        }
        match buf.iter().position(|b| !b.is_ascii_whitespace()) {
            Some(i) => {
                let b = buf[i];
                reader.consume(i);
                break Some(b);
            }
            None => {
                let n = buf.len();
                reader.consume(n);
            }
        }
    };
    let mut columns = Vec::new();
    let mut seen = HashSet::new();
    match first {
        None => Ok(columns),
        Some(b'[') => {
            struct Seq<'a, F> {
                f: &'a mut F,
                stopped: &'a mut bool,
            }
            impl<'de, F: FnMut(Value) -> bool> Visitor<'de> for Seq<'_, F> {
                type Value = ();
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an array of objects")
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                    while let Some(v) = seq.next_element::<Value>()? {
                        if !(self.f)(v) {
                            *self.stopped = true;
                            return Err(serde::de::Error::custom("stopped"));
                        }
                    }
                    Ok(())
                }
            }
            let mut err: Option<AppError> = None;
            let mut stopped = false;
            let mut handle = |v: Value| match object_record(v, ro, &mut columns, &mut seen) {
                Ok(rec) => on_record(rec),
                Err(e) => {
                    err = Some(e);
                    false
                }
            };
            let mut de = serde_json::Deserializer::from_reader(reader);
            let res = serde::Deserializer::deserialize_seq(&mut de, Seq { f: &mut handle, stopped: &mut stopped });
            if let Some(e) = err {
                return Err(e);
            }
            if !stopped {
                res.map_err(|e| AppError::msg(format!("invalid JSON: {e}")))?;
            }
            Ok(columns)
        }
        Some(_) => {
            for v in serde_json::Deserializer::from_reader(reader).into_iter::<Value>() {
                let v = v.map_err(|e| AppError::msg(format!("invalid JSON: {e}")))?;
                if !on_record(object_record(v, ro, &mut columns, &mut seen)?) {
                    break;
                }
            }
            Ok(columns)
        }
    }
}

/// First rows of a CSV/JSON file: `{ columns: [..], rows: [[..]] }`.
pub fn import_preview(format: &str, path: &Path, options: &Value) -> AppResult<Value> {
    let o = Opts(options);
    let ro = ReadOptions::new(format, o)?;
    let limit = o.u64("limit", 100).max(1) as usize;
    let mut records: Vec<Record> = Vec::new();
    let columns = read_records(path, &ro, |r| {
        records.push(r);
        records.len() < limit
    })?;
    let rows: Vec<Vec<Value>> = records
        .into_iter()
        .map(|r| {
            let m: HashMap<String, Value> = r.into_iter().collect();
            columns.iter().map(|c| m.get(c).cloned().unwrap_or(Value::Null)).collect()
        })
        .collect();
    Ok(json!({ "columns": columns, "rows": rows }))
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Resolves file columns to table columns (explicit mapping, case-insensitive
/// name match, or position for header-less CSV).
struct ColumnMapper {
    explicit: Option<HashMap<String, Option<String>>>,
    target_order: Vec<String>,
    target: HashMap<String, String>,
    positional: bool,
    cache: HashMap<String, Option<String>>,
}

impl ColumnMapper {
    fn new(o: Opts, structure: Option<&TableStructure>, positional: bool) -> Self {
        let explicit = o.get("mapping").and_then(Value::as_object).filter(|m| !m.is_empty()).map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().filter(|s| !s.is_empty()).map(str::to_string)))
                .collect()
        });
        let mut cols: Vec<&ColumnDef> = structure.map(|s| s.columns.iter().collect()).unwrap_or_default();
        cols.sort_by_key(|c| c.ordinal);
        Self {
            explicit,
            target_order: cols.iter().map(|c| c.name.clone()).collect(),
            target: cols.iter().map(|c| (c.name.to_lowercase(), c.name.clone())).collect(),
            positional,
            cache: HashMap::new(),
        }
    }

    fn resolve(&mut self, file_col: &str) -> Option<String> {
        if let Some(hit) = self.cache.get(file_col) {
            return hit.clone();
        }
        let r = if let Some(m) = &self.explicit {
            m.get(file_col).cloned().flatten()
        } else if self.target.is_empty() {
            Some(file_col.to_string()) // schemaless target
        } else if let Some(hit) = self.target.get(&file_col.to_lowercase()) {
            Some(hit.clone())
        } else if self.positional {
            file_col
                .strip_prefix("column")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| self.target_order.get(n.wrapping_sub(1)).cloned())
        } else {
            None
        };
        self.cache.insert(file_col.to_string(), r.clone());
        r
    }
}

/// Import a CSV/JSON file into an existing table. Returns rows inserted.
pub async fn import_data(
    driver: &DynDriver,
    table: &TableRef,
    format: &str,
    path: &Path,
    options: &Value,
    progress: &Progress<'_>,
) -> AppResult<u64> {
    let o = Opts(options);
    if driver.kind() == ConnectionKind::Redis {
        return Err(AppError::Unsupported("import into Redis".into()));
    }
    let ro = ReadOptions::new(format, o)?;
    let mode = o.str("mode").unwrap_or("append").to_ascii_lowercase();
    // Emptying the table waits until the file's first batch has been read and mapped, so a file
    // that can't be parsed (wrong format, bad encoding, missing columns) leaves the table intact.
    let mut pending_delete = match mode.as_str() {
        "append" => false,
        "truncate" | "replace" => true,
        other => return Err(AppError::Unsupported(format!("import mode {other}"))),
    };
    let d = driver.dialect();
    let structure = match driver.table_structure(table).await {
        Ok(s) => Some(s).filter(|s| !s.columns.is_empty()),
        Err(e) if d != Dialect::None => return Err(e),
        Err(_) => None,
    };
    let schemaless = d == Dialect::None;
    let infer = schemaless && !ro.json;
    let target = InsertTarget::new(d, table.clone(), if schemaless { None } else { structure.clone() });
    let mut mapper = ColumnMapper::new(o, structure.as_ref(), !ro.header && !ro.json);
    let batch = o.u64("batchSize", 500).max(1) as usize;

    // Parse on a blocking thread and stream batches back.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AppResult<Vec<Record>>>(4);
    let path = path.to_path_buf();
    let reader = tokio::task::spawn_blocking(move || {
        let mut buf = Vec::with_capacity(batch);
        let res = read_records(&path, &ro, |r| {
            buf.push(r);
            if buf.len() >= batch {
                return tx.blocking_send(Ok(std::mem::take(&mut buf))).is_ok();
            }
            true
        });
        match res {
            Ok(_) if !buf.is_empty() => {
                let _ = tx.blocking_send(Ok(buf));
            }
            Ok(_) => {}
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
            }
        }
    });

    let mut inserted = 0u64;
    let mut saw_records = false;
    let result: AppResult<()> = async {
        while let Some(batch) = rx.recv().await {
            let batch = batch?;
            saw_records |= !batch.is_empty();
            let rows: Vec<Map<String, Value>> = batch
                .into_iter()
                .map(|rec| {
                    let row: Map<String, Value> = rec
                        .into_iter()
                        .filter_map(|(k, v)| mapper.resolve(&k).map(|c| (c, if infer { infer_value(v) } else { v })))
                        .collect();
                    target.coerce_row(row)
                })
                .filter(|r| !r.is_empty())
                .collect();
            if rows.is_empty() {
                continue;
            }
            if std::mem::take(&mut pending_delete) {
                delete_all(driver, table).await?;
            }
            inserted += target.insert(driver, rows).await?;
            progress(json!({ "rows": inserted, "done": false }));
        }
        // a file with no rows at all still replaces the table's contents
        if pending_delete && !saw_records {
            delete_all(driver, table).await?;
        }
        Ok(())
    }
    .await;
    drop(rx);
    let _ = reader.await;
    target.finish(driver).await;
    result?;
    progress(json!({ "rows": inserted, "done": true }));
    Ok(inserted)
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

fn terminate(stmt: &str) -> String {
    let s = stmt.trim_end();
    if s.ends_with(';') || s.lines().last().is_some_and(|l| l.trim().eq_ignore_ascii_case("go")) {
        s.to_string()
    } else {
        format!("{s};")
    }
}

static NEXTVAL_COLUMN: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r#"(?m)^\s*("(?:[^"]|"")+"|[A-Za-z_][A-Za-z0-9_$]*)\s.*?nextval\('((?:[^']|'')+)'"#).unwrap()
});

/// Postgres table DDL from the catalog references `nextval('seq')` without creating
/// the sequence. Returns statements to run before (CREATE SEQUENCE) and after
/// (OWNED BY, so `pg_get_serial_sequence` and DROP TABLE work) the CREATE TABLE.
fn pg_sequence_statements(table_ddl: &str, table_sql: &str) -> (Vec<String>, Vec<String>) {
    if table_ddl.to_ascii_uppercase().contains("CREATE SEQUENCE") {
        return (vec![], vec![]);
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    for cap in NEXTVAL_COLUMN.captures_iter(table_ddl) {
        let seq = cap[2].replace("''", "'");
        before.push(format!("CREATE SEQUENCE IF NOT EXISTS {seq};"));
        after.push(format!("ALTER SEQUENCE {seq} OWNED BY {table_sql}.{};", &cap[1]));
    }
    (before, after)
}

/// Reference written into dump files: no database qualifier so the dump can be
/// restored into any database; schema kept where it is meaningful.
fn dump_ref(d: Dialect, t: &TableRef) -> TableRef {
    TableRef {
        database: None,
        schema: if matches!(d, Dialect::Postgres | Dialect::Mssql) { t.schema.clone() } else { None },
        name: t.name.clone(),
    }
}

async fn list_collections(driver: &DynDriver, database: Option<&str>) -> AppResult<Vec<TableRef>> {
    let snap = driver.schema_snapshot(database, None).await;
    if let Ok(s) = &snap {
        let refs: Vec<TableRef> = s.tables.iter().filter_map(|t| t.table.clone()).collect();
        if !refs.is_empty() {
            return Ok(refs);
        }
    }
    let Some(db) = database else { return snap.map(|_| vec![]) };
    let mut out = Vec::new();
    let mut stack = vec![vec![db.to_string()]];
    while let Some(path) = stack.pop() {
        for n in driver.children(&path).await? {
            match n.kind {
                NodeKind::Collection | NodeKind::Table => out.push(n.table.unwrap_or(TableRef {
                    database: Some(db.into()),
                    schema: None,
                    name: n.label.clone(),
                })),
                NodeKind::Folder if path.len() < 3 => stack.push(n.path.clone()),
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Copy every page of `table` into `f`; returns rows read.
async fn for_each_page(
    driver: &DynDriver,
    table: &TableRef,
    sort: &[SortSpec],
    batch: u64,
    mut f: impl FnMut(&[ColumnInfo], Vec<Vec<Value>>) -> AppResult<()>,
) -> AppResult<u64> {
    let mut offset = 0u64;
    loop {
        let req = TableDataRequest { table: table.clone(), offset, limit: batch, filter: None, sort: sort.to_vec() };
        let res = driver.table_data(&req).await?.result;
        let n = res.rows.len() as u64;
        if n > 0 {
            f(&res.columns, res.rows)?;
        }
        offset += n;
        if n < batch {
            return Ok(offset);
        }
    }
}

async fn backup_mongo(driver: &DynDriver, database: Option<&str>, dir: &Path, o: Opts<'_>, progress: &Progress<'_>) -> AppResult<String> {
    let started = Instant::now();
    std::fs::create_dir_all(dir)?;
    let mut colls = list_collections(driver, database).await?;
    if let Some(only) = o.strings("tables") {
        colls.retain(|c| only.contains(&c.name));
    }
    let (mut rows, mut bytes) = (0u64, 0u64);
    for c in &colls {
        let file = dir.join(format!("{}.jsonl", c.name.replace(['/', '\\'], "_")));
        let mut w = create_file(&file)?;
        let mut table = c.clone();
        if table.database.is_none() {
            table.database = database.map(Into::into);
        }
        let n = for_each_page(driver, &table, &[], 1000, |cols, page| {
            for r in rows_to_maps(cols, page) {
                serde_json::to_writer(&mut w, &r)?;
                w.write_all(b"\n")?;
            }
            Ok(())
        })
        .await?;
        w.flush()?;
        rows += n;
        bytes += std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        progress(json!({ "table": c.name, "rows": rows, "done": false }));
    }
    progress(json!({ "rows": rows, "done": true }));
    Ok(format!(
        "Backed up {} collections, {rows} documents ({bytes} bytes) to {} in {:.1}s",
        colls.len(),
        dir.display(),
        started.elapsed().as_secs_f64()
    ))
}

/// Dump schema (+ data) to a .sql file (SQL dialects) or a directory of JSON lines files (MongoDB).
pub async fn backup_database(
    driver: &DynDriver,
    database: Option<&str>,
    schema: Option<&str>,
    path: &Path,
    options: &Value,
    progress: &Progress<'_>,
) -> AppResult<String> {
    let o = Opts(options);
    let d = driver.dialect();
    match driver.kind() {
        ConnectionKind::Redis => return Err(AppError::Unsupported("backup of Redis".into())),
        ConnectionKind::Mongodb => return backup_mongo(driver, database, path, o, progress).await,
        _ => {}
    }
    let started = Instant::now();
    let include_data = o.bool("includeData", true) && !o.bool("structureOnly", false);
    let include_structure = o.bool("includeStructure", true);
    let drop_existing = include_structure && o.bool("dropExisting|dropTables|dropIfExists", true);
    let include_views = o.bool("includeViews", true);
    let insert_batch = max_insert_batch(d, o.u64("insertBatch", 100) as usize);
    let page = o.u64("batchSize", 1000).max(1);

    let snap = driver.schema_snapshot(database, schema).await?;
    let only = o.strings("tables");
    let wanted = |r: &TableRef| only.as_ref().is_none_or(|l| l.iter().any(|n| n == &r.name));
    let tables: Vec<TableStructure> = snap.tables.into_iter().filter(|t| t.table.as_ref().is_some_and(wanted)).collect();
    let views: Vec<(TableRef, String)> =
        if include_views { snap.views.into_iter().filter(|(r, _)| wanted(r)).collect() } else { vec![] };
    let (order, cyclic) = ddl::dependency_order(&tables);

    let mut w = create_file(path)?;
    let version = driver.server_version().await.unwrap_or_default();
    writeln!(w, "-- DevCat SQL dump")?;
    writeln!(w, "-- Source: {:?} {}", driver.kind(), version.lines().next().unwrap_or(""))?;
    if let Some(db) = database {
        writeln!(w, "-- Database: {db}")?;
    }
    if let Some(s) = schema {
        writeln!(w, "-- Schema: {s}")?;
    }
    writeln!(w, "-- Date: {}", chrono::Local::now().to_rfc3339())?;
    writeln!(w, "-- Tables: {}, views: {}{}\n", tables.len(), views.len(), if include_data { "" } else { " (structure only)" })?;
    match d {
        Dialect::Mysql => writeln!(w, "SET NAMES utf8mb4;\nSET FOREIGN_KEY_CHECKS = 0;\n")?,
        Dialect::Sqlite => writeln!(w, "PRAGMA foreign_keys = OFF;\n")?,
        _ => {}
    }

    if drop_existing {
        for (v, _) in views.iter().rev() {
            writeln!(w, "DROP VIEW IF EXISTS {};", ddl::qualified_name(d, &dump_ref(d, v)))?;
        }
        for &i in order.iter().rev() {
            let r = dump_ref(d, tables[i].table.as_ref().expect("filtered"));
            writeln!(w, "{};", ddl::drop_table_sql(d, &r, true))?;
        }
        writeln!(w)?;
    }

    let mut deferred_fks: Vec<String> = Vec::new();
    let mut total_rows = 0u64;
    for &i in &order {
        let t = &tables[i];
        let src_ref = t.table.clone().expect("filtered");
        let r = dump_ref(d, &src_ref);
        writeln!(w, "-- ----------------------------\n-- Table {}\n-- ----------------------------", ddl::qualified_name(d, &r))?;
        let in_cycle = cyclic.contains(&i) && d != Dialect::Sqlite;
        let native = if !include_structure {
            None
        } else if in_cycle {
            None
        } else {
            driver.object_ddl(&src_ref, &NodeKind::Table).await.ok().filter(|s| !s.trim().is_empty())
        };
        match native {
            _ if !include_structure => {}
            Some(sql) if d == Dialect::Postgres => {
                let (before, after) = pg_sequence_statements(&sql, &ddl::qualified_name(d, &r));
                for s in &before {
                    writeln!(w, "{s}")?;
                }
                writeln!(w, "{}", terminate(&sql))?;
                for s in &after {
                    writeln!(w, "{s}")?;
                }
                writeln!(w)?;
            }
            Some(sql) => writeln!(w, "{}\n", terminate(&sql))?,
            None => {
                let opts = CreateTableOptions { foreign_keys: !in_cycle, ..Default::default() };
                writeln!(w, "{}\n", ddl::create_table_sql_with(d, t, &r, &opts))?;
                if in_cycle {
                    deferred_fks.extend(t.foreign_keys.iter().map(|fk| ddl::add_foreign_key_sql(d, &r, fk)));
                }
            }
        }
        if include_data {
            let name = ddl::qualified_name(d, &r);
            let identity = d == Dialect::Mssql && t.columns.iter().any(|c| c.auto_increment);
            let class_of: HashMap<&str, TypeClass> =
                t.columns.iter().map(|c| (c.name.as_str(), ddl::type_class(d, &c.data_type))).collect();
            let n = for_each_page(driver, &src_ref, &pk_sort(Some(t)), page, |cols, rows| {
                let names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
                let classes: Vec<TypeClass> = cols
                    .iter()
                    .map(|c| class_of.get(c.name.as_str()).copied().unwrap_or_else(|| ddl::type_class(d, &c.type_name)))
                    .collect();
                for chunk in rows.chunks(insert_batch) {
                    writeln!(w, "{};", insert_sql(d, &name, &names, &classes, chunk, identity))?;
                }
                Ok(())
            })
            .await?;
            if n > 0 {
                for s in sequence_reset_sql(d, &r, t) {
                    writeln!(w, "{s};")?;
                }
            }
            total_rows += n;
            writeln!(w)?;
        }
        progress(json!({ "table": src_ref.name, "rows": total_rows, "done": false }));
    }

    if !deferred_fks.is_empty() {
        writeln!(w, "-- Foreign keys (circular references)")?;
        for fk in &deferred_fks {
            writeln!(w, "{fk};")?;
        }
        writeln!(w)?;
    }

    for (v, def) in views.iter().filter(|_| include_structure) {
        let r = dump_ref(d, v);
        let sql = match driver.object_ddl(v, &NodeKind::View).await.ok().filter(|s| !s.trim().is_empty()) {
            Some(s) => s,
            None if def.trim_start().to_ascii_lowercase().starts_with("create") => def.clone(),
            None => format!("CREATE VIEW {} AS\n{}", ddl::qualified_name(d, &r), def.trim().trim_end_matches(';')),
        };
        writeln!(w, "-- View {}\n{}\n", ddl::qualified_name(d, &r), terminate(&sql))?;
    }

    match d {
        Dialect::Mysql => writeln!(w, "SET FOREIGN_KEY_CHECKS = 1;")?,
        Dialect::Sqlite => writeln!(w, "PRAGMA foreign_keys = ON;")?,
        _ => {}
    }
    w.flush()?;
    drop(w);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    progress(json!({ "rows": total_rows, "done": true }));
    Ok(format!(
        "Backed up {} tables, {} views, {total_rows} rows to {} ({bytes} bytes) in {:.1}s",
        tables.len(),
        views.len(),
        path.display(),
        started.elapsed().as_secs_f64()
    ))
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/// True when the statement has nothing but comments / whitespace.
fn is_comment_only(stmt: &str) -> bool {
    let mut s = stmt.trim();
    loop {
        if s.is_empty() {
            return true;
        }
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map(|x| x.1).unwrap_or("").trim_start();
        } else if s.starts_with("/*") && !s.starts_with("/*!") {
            match s.find("*/") {
                Some(end) => s = s[end + 2..].trim_start(),
                None => return true,
            }
        } else {
            return false;
        }
    }
}

/// Run a .sql script (or, for MongoDB, a directory of `<collection>.jsonl` files).
pub async fn restore_database(driver: &DynDriver, database: Option<&str>, path: &Path, options: &Value, progress: &Progress<'_>) -> AppResult<String> {
    let o = Opts(options);
    let started = Instant::now();
    let d = driver.dialect();
    if driver.kind() == ConnectionKind::Redis {
        return Err(AppError::Unsupported("restore into Redis".into()));
    }
    if d == Dialect::None {
        if !path.is_dir() {
            return Err(AppError::Unsupported("MongoDB restore expects a directory of .jsonl files".into()));
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl" || x == "json"))
            .collect();
        files.sort();
        let mut total = 0;
        for f in &files {
            let name = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let table = TableRef { database: database.map(Into::into), schema: None, name };
            let opts = json!({ "mode": if o.bool("dropExisting", false) { "truncate" } else { "append" } });
            total += import_data(driver, &table, "json", f, &opts, &|_| {}).await?;
            progress(json!({ "table": table.name, "rows": total, "done": false }));
        }
        progress(json!({ "rows": total, "done": true }));
        return Ok(format!("Restored {} collections, {total} documents in {:.1}s", files.len(), started.elapsed().as_secs_f64()));
    }

    let script = std::fs::read_to_string(path).map_err(|e| AppError::msg(format!("cannot read {}: {e}", path.display())))?;
    let statements: Vec<String> = split_statements(d, &script).into_iter().filter(|s| !is_comment_only(s)).collect();
    drop(script);
    let total = statements.len();
    let continue_on_error = o.bool("continueOnError", false);
    let opts = ExecOptions { database: database.map(Into::into), schema: o.str("schema").map(Into::into), max_rows: Some(10) };
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut errors: Vec<String> = Vec::new();
    for (i, stmt) in statements.iter().enumerate() {
        match driver.execute(stmt, &opts).await {
            Ok(_) => ok += 1,
            Err(e) => {
                failed += 1;
                let preview: String = stmt.trim().chars().take(160).collect();
                let msg = format!("#{}: {e}\n  {preview}", i + 1);
                if !continue_on_error {
                    progress(json!({ "rows": ok, "total": total, "done": true, "error": e.to_string() }));
                    return Err(AppError::msg(format!("Restore stopped after {ok} statements. Statement {msg}")));
                }
                if errors.len() < 20 {
                    errors.push(msg);
                }
            }
        }
        if (i + 1) % 25 == 0 {
            progress(json!({ "rows": i + 1, "total": total, "failed": failed, "done": false }));
        }
    }
    progress(json!({ "rows": total, "total": total, "failed": failed, "done": true }));
    let mut summary =
        format!("Executed {ok} of {total} statements, {failed} failed, in {:.1}s", started.elapsed().as_secs_f64());
    if !errors.is_empty() {
        summary.push_str("\nErrors:\n");
        summary.push_str(&errors.join("\n"));
        if failed > errors.len() {
            summary.push_str(&format!("\n... and {} more", failed - errors.len()));
        }
    }
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Transfer between connections
// ---------------------------------------------------------------------------

pub struct TransferTarget {
    pub database: Option<String>,
    pub schema: Option<String>,
}

/// Copy tables from `src` to `dst` (optionally creating them). Returns rows copied.
pub async fn transfer_tables(
    src: &DynDriver,
    dst: &DynDriver,
    tables: &[TableRef],
    target: &TransferTarget,
    options: &Value,
    progress: &Progress<'_>,
) -> AppResult<u64> {
    let o = Opts(options);
    if src.kind() == ConnectionKind::Redis || dst.kind() == ConnectionKind::Redis {
        return Err(AppError::Unsupported("data transfer with Redis".into()));
    }
    let (sd, dd) = (src.dialect(), dst.dialect());
    let create = o.bool("createTables", true);
    let drop_existing = o.bool("dropExisting", false);
    let truncate = o.bool("truncate", false);
    let with_indexes = o.bool("indexes", true);
    let with_fks = o.bool("foreignKeys", false);
    let continue_on_error = o.bool("continueOnError", false);
    let batch = o.u64("batchSize", 1000).max(1);
    let target_schema = target.schema.clone().or_else(|| (dd == Dialect::Mssql).then(|| "dbo".to_string()));
    let tgt_opts = exec_opts(target.database.as_deref(), target_schema.as_deref());

    // Source structures, ordered so FK parents are copied first.
    let mut structures = Vec::with_capacity(tables.len());
    for t in tables {
        let mut s = match src.table_structure(t).await {
            Ok(s) => s,
            Err(e) if sd == Dialect::None => {
                let _ = e;
                TableStructure::default()
            }
            Err(e) => return Err(e),
        };
        s.table = Some(t.clone());
        structures.push(s);
    }
    let (order, _) = ddl::dependency_order(&structures);
    let target_ref = |t: &TableRef| TableRef {
        database: target.database.clone(),
        schema: if dd == Dialect::Mysql { None } else { target_schema.clone() },
        name: t.name.clone(),
    };

    if drop_existing && dd != Dialect::None {
        for &i in order.iter().rev() {
            let r = target_ref(structures[i].table.as_ref().expect("set above"));
            let _ = dst.execute(&ddl::drop_table_sql(dd, &r, true), &tgt_opts).await;
        }
    }

    let mut total = 0u64;
    let mut errors: Vec<String> = Vec::new();
    let mut fk_statements: Vec<String> = Vec::new();
    for (pos, &i) in order.iter().enumerate() {
        let s = &structures[i];
        let src_ref = s.table.clone().expect("set above");
        let tref = target_ref(&src_ref);
        let res: AppResult<u64> = async {
            let existing = if dd == Dialect::None {
                None
            } else {
                dst.table_structure(&tref).await.ok().filter(|t| !t.columns.is_empty())
            };
            let dst_structure = match existing {
                Some(e) => Some(e),
                None if create && dd != Dialect::None => {
                    let mut conv = ddl::convert_structure(sd, dd, s);
                    for fk in &mut conv.foreign_keys {
                        fk.ref_schema = if dd == Dialect::Mysql { target.database.clone() } else { target_schema.clone() };
                    }
                    let opts = CreateTableOptions { foreign_keys: false, indexes: with_indexes, comments: true, if_not_exists: false };
                    for stmt in ddl::create_table_statements(dd, &conv, &tref, &opts) {
                        dst.execute(&stmt, &tgt_opts).await?;
                    }
                    if with_fks && dd != Dialect::Sqlite {
                        fk_statements.extend(conv.foreign_keys.iter().map(|fk| ddl::add_foreign_key_sql(dd, &tref, fk)));
                    }
                    dst.table_structure(&tref).await.ok().or(Some(conv))
                }
                None if dd == Dialect::None => None,
                None => return Err(AppError::msg(format!("target table {} does not exist", tref.name))),
            };
            if truncate {
                delete_all(dst, &tref).await?;
            }
            let ins = InsertTarget::new(dd, tref.clone(), dst_structure);
            let mut copied = 0u64;
            let mut offset = 0u64;
            let sort = pk_sort(Some(s));
            loop {
                let req = TableDataRequest { table: src_ref.clone(), offset, limit: batch, filter: None, sort: sort.clone() };
                let page = src.table_data(&req).await?;
                let n = page.result.rows.len() as u64;
                let rows: Vec<Map<String, Value>> = rows_to_maps(&page.result.columns, page.result.rows)
                    .into_iter()
                    .map(|r| {
                        // drop source columns the target does not have
                        let r: Map<String, Value> = if ins.columns.is_empty() {
                            r
                        } else {
                            r.into_iter().filter_map(|(k, v)| ins.column(&k).map(|c| (c.0.clone(), v))).collect()
                        };
                        ins.coerce_row(r)
                    })
                    .collect();
                copied += ins.insert(dst, rows).await?;
                progress(json!({
                    "table": src_ref.name, "tableIndex": pos, "tables": order.len(),
                    "tableRows": copied, "tableTotal": page.total, "rows": total + copied, "done": false
                }));
                if n < batch {
                    break;
                }
                offset += n;
            }
            ins.finish(dst).await;
            Ok(copied)
        }
        .await;
        match res {
            Ok(n) => total += n,
            Err(e) if continue_on_error => errors.push(format!("{}: {e}", src_ref.name)),
            Err(e) => {
                progress(json!({ "rows": total, "done": true, "error": e.to_string() }));
                return Err(AppError::msg(format!("{}: {e}", src_ref.name)));
            }
        }
    }
    for stmt in &fk_statements {
        if let Err(e) = dst.execute(stmt, &tgt_opts).await {
            errors.push(format!("foreign key: {e}"));
        }
    }
    progress(json!({ "rows": total, "done": true, "errors": errors }));
    if !errors.is_empty() && total == 0 {
        return Err(AppError::msg(errors.join("\n")));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devcat-transfer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn cols(names: &[&str]) -> Vec<ColumnInfo> {
        names.iter().map(|n| ColumnInfo { name: n.to_string(), type_name: "text".into() }).collect()
    }

    #[test]
    fn sql_values() {
        use TypeClass::*;
        assert_eq!(sql_value(Dialect::Postgres, &json!(1), Bool), "TRUE");
        assert_eq!(sql_value(Dialect::Mysql, &json!(true), Bool), "1");
        assert_eq!(sql_value(Dialect::Postgres, &json!("\\x00ff"), Binary), "'\\x00ff'::bytea");
        assert_eq!(sql_value(Dialect::Mssql, &json!("\\x00ff"), Binary), "0x00ff");
        assert_eq!(sql_value(Dialect::Mysql, &json!("AP8="), Binary), "X'00ff'");
        assert_eq!(sql_value(Dialect::Mssql, &json!({"a":"b'c"}), Json), "N'{\"a\":\"b''c\"}'");
        assert_eq!(sql_value(Dialect::Mysql, &json!("a\\b"), Text), "'a\\\\b'");
        assert_eq!(sql_value(Dialect::Sqlite, &Value::Null, Text), "NULL");
        assert_eq!(sql_value(Dialect::Postgres, &json!(1.5), Decimal), "1.5");
    }

    #[test]
    fn coercion() {
        use TypeClass::*;
        assert_eq!(coerce(Some(Integer), json!(" 42 ")), json!(42));
        assert_eq!(coerce(Some(Integer), json!("x")), json!("x"));
        assert_eq!(coerce(Some(Bool), json!("yes")), json!(true));
        assert_eq!(coerce(Some(Bool), json!(0)), json!(false));
        assert_eq!(coerce(Some(Float), json!("1.25")), json!(1.25));
        assert_eq!(coerce(Some(Decimal), json!("12.50")), json!("12.50"));
        assert_eq!(coerce(Some(Decimal), json!("12.5")), json!(12.5));
        assert_eq!(coerce(Some(Decimal), json!("123456789012345678.12")), json!("123456789012345678.12"));
        assert_eq!(coerce(Some(Text), json!({"a":1})), json!("{\"a\":1}"));
        assert_eq!(coerce(Some(Text), json!(7)), json!("7"));
        assert_eq!(coerce(Some(Json), json!({"a":1})), json!({"a":1}));
        assert_eq!(coerce(None, json!("7")), json!("7"));
        assert_eq!(coerce(Some(Text), json!({"$oid": "abc"})), json!("abc"));
        assert_eq!(coerce(Some(Integer), json!({"$numberLong": "9007199254740993"})), json!(9007199254740993i64));
        assert_eq!(coerce(Some(Temporal), json!({"$date": {"$numberLong": "0"}})), json!("1970-01-01T00:00:00+00:00"));
        assert_eq!(coerce(Some(Temporal), json!("2024-01-01 10:00:00.5+00")), json!("2024-01-01 10:00:00.5+00:00"));
        assert_eq!(coerce(Some(Temporal), json!("2024-01-01 10:00:00+05:30")), json!("2024-01-01 10:00:00+05:30"));
        assert_eq!(coerce(Some(Temporal), json!("2024-01-01")), json!("2024-01-01"));
        assert_eq!(coerce(Some(Json), json!({"a": {"$oid": "x"}})), json!({"a": {"$oid": "x"}}));
        assert_eq!(infer_value(json!("7")), json!(7));
        assert_eq!(infer_value(json!("007")), json!("007"));
        assert_eq!(infer_value(json!("1.5")), json!(1.5));
        assert_eq!(infer_value(json!("true")), json!(true));
    }

    #[test]
    fn insert_statement() {
        let s = insert_sql(
            Dialect::Mssql,
            "[dbo].[t]",
            &["id".into(), "flag".into(), "name".into()],
            &[TypeClass::Integer, TypeClass::Bool, TypeClass::Text],
            &[vec![json!(1), json!(true), json!("a;b")], vec![json!(2), Value::Null, json!("o'k")]],
            true,
        );
        assert_eq!(
            s,
            "SET IDENTITY_INSERT [dbo].[t] ON\nINSERT INTO [dbo].[t] ([id], [flag], [name]) VALUES\n(1, 1, N'a;b'),\n(2, NULL, N'o''k')\nSET IDENTITY_INSERT [dbo].[t] OFF"
        );
        let t = TableRef { database: None, schema: Some("public".into()), name: "t".into() };
        let st = TableStructure {
            columns: vec![ColumnDef { name: "id".into(), auto_increment: true, ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(
            sequence_reset_sql(Dialect::Postgres, &t, &st),
            vec!["SELECT setval(pg_get_serial_sequence('\"public\".\"t\"', 'id'), COALESCE(MAX(\"id\"), 0) + 1, false) FROM \"public\".\"t\""]
        );
    }

    #[test]
    fn csv_and_json_sinks_roundtrip() {
        let p = tmp("out.csv");
        let o = json!({ "delimiter": ";", "nullAs": "\\N", "lineEnding": "crlf" });
        let mut sink: Box<dyn Sink> = Box::new(CsvSink::new(&p, Opts(&o)).unwrap());
        let c = cols(&["a", "b"]);
        sink.begin(&c).unwrap();
        sink.write(&c, &[vec![json!(1), json!("x;y")], vec![Value::Null, json!({"k": true})]]).unwrap();
        // a later page with different column order / missing column
        sink.write(&cols(&["b"]), &[vec![json!("only b")]]).unwrap();
        sink.finish().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text, "a;b\r\n1;\"x;y\"\r\n\\N;\"{\"\"k\"\":true}\"\r\n\\N;only b\r\n");

        // read it back
        let ro = ReadOptions::new("csv", Opts(&json!({ "delimiter": ";", "nullAs": "\\N" }))).unwrap();
        let mut recs = vec![];
        let columns = read_records(&p, &ro, |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(columns, vec!["a", "b"]);
        assert_eq!(recs[0], vec![("a".to_string(), json!("1")), ("b".to_string(), json!("x;y"))]);
        assert_eq!(recs[1][0].1, Value::Null);
        assert_eq!(recs.len(), 3);

        let pj = tmp("out.json");
        let mut sink: Box<dyn Sink> = Box::new(JsonSink { w: create_file(&pj).unwrap(), pretty: true, lines: false, first: true });
        sink.begin(&c).unwrap();
        sink.write(&c, &[vec![json!(1), json!("z")], vec![json!(2), json!({"n": [1, 2]})]]).unwrap();
        sink.finish().unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&pj).unwrap()).unwrap();
        assert_eq!(parsed, json!([{"a": 1, "b": "z"}, {"a": 2, "b": {"n": [1, 2]}}]));
        // keys keep column order (b after a even though sorted would agree; check z,a)
        let mut sink: Box<dyn Sink> = Box::new(JsonSink { w: create_file(&pj).unwrap(), pretty: false, lines: false, first: true });
        let zc = cols(&["z", "a"]);
        sink.begin(&zc).unwrap();
        sink.write(&zc, &[vec![json!(1), json!(2)]]).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&pj).unwrap(), "[{\"z\":1,\"a\":2}]\n");
        let mut sink: Box<dyn Sink> = Box::new(JsonSink { w: create_file(&pj).unwrap(), pretty: false, lines: false, first: true });
        sink.begin(&zc).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&pj).unwrap(), "[]\n");
    }

    #[test]
    fn json_reading_array_and_lines() {
        let p = tmp("in.json");
        std::fs::write(&p, "\u{feff}  [ {\"id\": 1, \"name\": \"a\"}, {\"id\": 2, \"extra\": null, \"name\": \"\"} ]").unwrap();
        let ro = ReadOptions::new("json", Opts(&json!({ "emptyAsNull": true }))).unwrap();
        let mut recs = vec![];
        let columns = read_records(&p, &ro, |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(columns, vec!["id", "name", "extra"]);
        assert!(recs[1].contains(&("name".to_string(), Value::Null)));

        // early stop on arrays is not an error
        let mut count = 0;
        read_records(&p, &ro, |_| {
            count += 1;
            false
        })
        .unwrap();
        assert_eq!(count, 1);

        let pl = tmp("in.jsonl");
        std::fs::write(&pl, "{\"a\":1}\n\n{\"a\":2,\"b\":[1]}\n").unwrap();
        let preview = import_preview("jsonl", &pl, &json!({})).unwrap();
        assert_eq!(preview, json!({ "columns": ["a", "b"], "rows": [[1, null], [2, [1]]] }));

        std::fs::write(&pl, "[1, 2]").unwrap();
        assert!(import_preview("json", &pl, &json!({})).is_err());
        std::fs::write(&pl, "[{\"a\":1}, {\"a\": ]").unwrap();
        assert!(import_preview("json", &pl, &json!({})).is_err());
    }

    #[test]
    fn csv_reading_options() {
        let p = tmp("in.csv");
        std::fs::write(&p, "junk line\nanother\n1\t\"x\ty\"\t\n2\tz\tNULL\n").unwrap();
        let o = json!({ "delimiter": "\\t", "header": false, "skipRows": 2, "nullAs": "NULL" });
        let preview = import_preview("csv", &p, &o).unwrap();
        assert_eq!(
            preview,
            json!({ "columns": ["column1", "column2", "column3"], "rows": [["1", "x\ty", null], ["2", "z", null]] })
        );
        let o = json!({ "limit": 1 });
        std::fs::write(&p, "a,b\n1,2\n3,4\n").unwrap();
        assert_eq!(import_preview("csv", &p, &o).unwrap()["rows"], json!([["1", "2"]]));
    }

    #[test]
    fn column_mapping() {
        let st = TableStructure {
            columns: vec![
                ColumnDef { name: "Id".into(), ordinal: 1, ..Default::default() },
                ColumnDef { name: "Name".into(), ordinal: 2, ..Default::default() },
            ],
            ..Default::default()
        };
        let empty = json!({});
        let mut m = ColumnMapper::new(Opts(&empty), Some(&st), false);
        assert_eq!(m.resolve("id").as_deref(), Some("Id"));
        assert_eq!(m.resolve("NAME").as_deref(), Some("Name"));
        assert_eq!(m.resolve("other"), None);
        let mut m = ColumnMapper::new(Opts(&empty), Some(&st), true);
        assert_eq!(m.resolve("column2").as_deref(), Some("Name"));
        let o = json!({ "mapping": { "full_name": "Name", "skip": "" } });
        let mut m = ColumnMapper::new(Opts(&o), Some(&st), false);
        assert_eq!(m.resolve("full_name").as_deref(), Some("Name"));
        assert_eq!(m.resolve("skip"), None);
        assert_eq!(m.resolve("Id"), None);
        let mut m = ColumnMapper::new(Opts(&empty), None, false);
        assert_eq!(m.resolve("anything").as_deref(), Some("anything"));
    }

    #[test]
    fn pg_sequences_for_native_ddl() {
        let ddl = "CREATE TABLE \"s\".\"t\" (\n    \"id\" integer DEFAULT nextval('s.t_id_seq'::regclass) NOT NULL,\n    \"Other\" bigint DEFAULT nextval('\"S\".\"Odd\"'::regclass)\n);";
        let (b, a) = pg_sequence_statements(ddl, "\"s\".\"t\"");
        assert_eq!(b, vec!["CREATE SEQUENCE IF NOT EXISTS s.t_id_seq;", "CREATE SEQUENCE IF NOT EXISTS \"S\".\"Odd\";"]);
        assert_eq!(a[0], "ALTER SEQUENCE s.t_id_seq OWNED BY \"s\".\"t\".\"id\";");
        assert_eq!(a[1], "ALTER SEQUENCE \"S\".\"Odd\" OWNED BY \"s\".\"t\".\"Other\";");
        assert_eq!(pg_sequence_statements("CREATE SEQUENCE x; CREATE TABLE t (id int DEFAULT nextval('x'))", "t"), (vec![], vec![]));
    }

    #[test]
    fn comment_only_detection() {
        assert!(is_comment_only("-- hello\n-- world"));
        assert!(is_comment_only("/* x */ \n -- y"));
        assert!(!is_comment_only("-- t\nCREATE TABLE x (a int)"));
        assert!(!is_comment_only("/*!40101 SET NAMES utf8 */"));
        assert_eq!(terminate("SELECT 1"), "SELECT 1;");
        assert_eq!(terminate("SELECT 1;\n"), "SELECT 1;");
    }

    #[test]
    fn xlsx_sink_writes_file() {
        let p = tmp("out.xlsx");
        let o = json!({ "sheetName": "Data/1" });
        let mut sink: Box<dyn Sink> = Box::new(XlsxSink::new(&p, Opts(&o)).unwrap());
        let c = cols(&["n", "s", "b"]);
        sink.begin(&c).unwrap();
        sink.write(&c, &[vec![json!(1.5), json!("x"), json!(true)], vec![Value::Null, json!({"a":1}), json!(false)]]).unwrap();
        sink.finish().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.starts_with(b"PK"), "xlsx is a zip file");
    }

    #[test]
    fn sql_sink_with_create() {
        let p = tmp("out.sql");
        let structure = TableStructure {
            columns: vec![
                ColumnDef { name: "id".into(), data_type: "serial".into(), is_primary_key: true, ordinal: 1, ..Default::default() },
                ColumnDef { name: "ok".into(), data_type: "boolean".into(), nullable: true, ordinal: 2, ..Default::default() },
            ],
            ..Default::default()
        };
        let mut sink: Box<dyn Sink> = Box::new(SqlSink {
            w: create_file(&p).unwrap(),
            source: Dialect::Postgres,
            target: Dialect::Mssql,
            table: TableRef { database: None, schema: None, name: "t".into() },
            structure: Some(structure),
            create: true,
            drop: true,
            batch: 2,
            identity: false,
        });
        let c = vec![
            ColumnInfo { name: "id".into(), type_name: "INT4".into() },
            ColumnInfo { name: "ok".into(), type_name: "BOOL".into() },
        ];
        sink.begin(&c).unwrap();
        sink.write(&c, &[vec![json!(1), json!(true)], vec![json!(2), json!(false)], vec![json!(3), Value::Null]]).unwrap();
        sink.finish().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("DROP TABLE IF EXISTS [t];"), "{text}");
        assert!(text.contains("[id] int IDENTITY(1,1) NOT NULL"), "{text}");
        assert!(text.contains("SET IDENTITY_INSERT [t] ON\nINSERT INTO [t] ([id], [ok]) VALUES\n(1, 1),\n(2, 0)\nSET IDENTITY_INSERT [t] OFF;"), "{text}");
        assert!(text.contains("(3, NULL)"), "{text}");
    }
}

/// Integration tests against the docker-compose dev servers. Run with
/// `cargo test --lib -- --ignored transfer::integration`.
#[cfg(test)]
mod integration {
    use super::*;
    use crate::models::ConnectionConfig;

    fn cfg(kind: &str, port: u16, user: &str, password: &str, database: &str) -> ConnectionConfig {
        serde_json::from_value(json!({
            "id": format!("test-{kind}"), "name": kind, "kind": kind, "host": "127.0.0.1", "port": port,
            "user": user, "password": password, "database": database, "sslMode": "disable"
        }))
        .unwrap()
    }

    pub(super) fn pg_cfg() -> ConnectionConfig {
        cfg("postgres", 55432, "postgres", "devcat", "devcat")
    }
    fn mysql_cfg() -> ConnectionConfig {
        cfg("mysql", 53306, "root", "devcat", "devcat")
    }
    fn mssql_cfg() -> ConnectionConfig {
        cfg("mssql", 51433, "sa", "DevCat_Passw0rd!", "master")
    }
    fn mongo_cfg() -> ConnectionConfig {
        cfg("mongodb", 57017, "devcat", "devcat", "devcat")
    }
    fn sqlite_cfg(path: &Path) -> ConnectionConfig {
        let mut c = cfg("sqlite", 0, "", "", "main");
        c.file_path = Some(path.to_string_lossy().into_owned());
        c
    }

    async fn connect(c: ConnectionConfig) -> DynDriver {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        crate::db::connect(&c).await.unwrap_or_else(|e| panic!("connect {:?}: {e}", c.kind))
    }

    async fn exec(d: &DynDriver, db: Option<&str>, sql: &str) -> Vec<crate::db::QueryResult> {
        let opts = ExecOptions { database: db.map(Into::into), schema: None, max_rows: Some(10_000) };
        d.execute(sql, &opts).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn scalar(d: &DynDriver, db: Option<&str>, sql: &str) -> i64 {
        let r = exec(d, db, sql).await;
        let v = &r.last().unwrap().rows[0][0];
        v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or_else(|| panic!("{sql} -> {v}"))
    }

    fn noop() -> impl Fn(Value) + Send + Sync {
        |_| {}
    }

    fn tdir(test: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devcat-it-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn t(db: Option<&str>, schema: Option<&str>, name: &str) -> TableRef {
        TableRef { database: db.map(Into::into), schema: schema.map(Into::into), name: name.into() }
    }

    pub(super) const PG_SEED: &str = r#"
        DROP SCHEMA IF EXISTS xfer CASCADE;
        CREATE SCHEMA xfer;
        CREATE TABLE xfer.users (
            id serial PRIMARY KEY,
            email varchar(100) NOT NULL UNIQUE,
            active boolean NOT NULL DEFAULT true,
            balance numeric(10,2) DEFAULT 0,
            created timestamptz DEFAULT now(),
            profile jsonb,
            avatar bytea,
            note text
        );
        CREATE INDEX users_note_idx ON xfer.users (note);
        CREATE TABLE xfer.orders (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            user_id int NOT NULL REFERENCES xfer.users(id) ON DELETE CASCADE,
            total numeric(10,2)
        );
        INSERT INTO xfer.users (email, active, balance, profile, avatar, note)
            SELECT 'u' || g || '@x.io', g % 2 = 0, g * 1.25, jsonb_build_object('n', g, 'tags', jsonb_build_array('a', 'b')),
                   decode('00ff' || lpad(to_hex(g % 256), 2, '0'), 'hex'), CASE WHEN g % 10 = 0 THEN NULL ELSE 'note; it''s #' || g END
            FROM generate_series(1, 2500) g;
        INSERT INTO xfer.orders (user_id, total) SELECT (g % 2500) + 1, g FROM generate_series(1, 1200) g;
        CREATE VIEW xfer.big_orders AS SELECT * FROM xfer.orders WHERE total > 1000;
    "#;

    #[tokio::test]
    #[ignore]
    async fn pg_export_import_backup_restore() {
        let pg = connect(pg_cfg()).await;
        let db = Some("devcat");
        exec(&pg, db, &PG_SEED.replace("xfer", "xfer_exp")).await;
        let dir = tdir("pg_export_import_backup_restore");
        let users = t(db, Some("xfer_exp"), "users");

        // CSV export of a table (paged: 2500 rows / 1000 per page)
        let events = std::sync::Mutex::new(Vec::new());
        let progress = |v: Value| events.lock().unwrap().push(v);
        let csv_path = dir.join("users.csv");
        let n = export_data(&pg, ExportSource::Table(users.clone()), "csv", &csv_path, &json!({}), &progress).await.unwrap();
        assert_eq!(n, 2500);
        let text = std::fs::read_to_string(&csv_path).unwrap();
        assert_eq!(text.lines().next().unwrap(), "id,email,active,balance,created,profile,avatar,note");
        assert!(events.lock().unwrap().last().unwrap()["done"].as_bool().unwrap());

        // JSON / JSONL / XLSX of a query
        let q = ExportSource::Query { sql: "SELECT id, email, profile FROM xfer_exp.users ORDER BY id".into(), database: db.map(Into::into) };
        let jp = dir.join("q.json");
        assert_eq!(export_data(&pg, q, "json", &jp, &json!({}), &noop()).await.unwrap(), 2500);
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&jp).unwrap()).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2500);
        assert_eq!(parsed[0]["email"], "u1@x.io");
        let xp = dir.join("users.xlsx");
        assert_eq!(export_data(&pg, ExportSource::Table(users.clone()), "xlsx", &xp, &json!({}), &noop()).await.unwrap(), 2500);

        // SQL export with CREATE prelude into a new table name, then run it
        let sp = dir.join("users_copy.sql");
        let o = json!({ "includeCreate": true, "includeDrop": true, "tableName": "users_copy" });
        export_data(&pg, ExportSource::Table(users.clone()), "sql", &sp, &o, &noop()).await.unwrap();
        let summary = restore_database(&pg, db, &sp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains(", 0 failed"), "{summary}");
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_copy").await, 2500);
        assert_eq!(
            scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users u JOIN xfer_exp.users_copy c USING (id) WHERE u.avatar = c.avatar AND u.profile = c.profile AND u.note IS NOT DISTINCT FROM c.note AND u.created = c.created AND u.active = c.active").await,
            2500
        );

        // CSV import into an empty clone, then truncate + re-import
        exec(&pg, db, "DROP TABLE IF EXISTS xfer_exp.users_import; CREATE TABLE xfer_exp.users_import (LIKE xfer_exp.users INCLUDING ALL)").await;
        let imp = t(db, Some("xfer_exp"), "users_import");
        let n = import_data(&pg, &imp, "csv", &csv_path, &json!({}), &noop()).await.unwrap();
        assert_eq!(n, 2500);
        let n = import_data(&pg, &imp, "csv", &csv_path, &json!({ "mode": "truncate", "batchSize": 333 }), &noop()).await.unwrap();
        assert_eq!(n, 2500);
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_import").await, 2500);
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_import WHERE note IS NULL").await, 250);
        // a truncate import of a file that can't be parsed leaves the table alone
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "this is not json").unwrap();
        assert!(import_data(&pg, &imp, "json", &bad, &json!({ "mode": "truncate" }), &noop()).await.is_err());
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_import").await, 2500);
        // exporting a query result never re-runs a data-changing statement
        let del = ExportSource::Query { sql: "DELETE FROM xfer_exp.users_import WHERE id < 10 RETURNING id".into(), database: db.map(Into::into) };
        assert!(export_data(&pg, del, "csv", &dir.join("del.csv"), &json!({}), &noop()).await.is_err());
        assert!(!dir.join("del.csv").exists());
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_import").await, 2500);
        assert_eq!(
            scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users u JOIN xfer_exp.users_import c USING (id) WHERE u.avatar = c.avatar AND u.profile = c.profile AND u.balance = c.balance").await,
            2500
        );
        // JSON import with mapping
        exec(&pg, db, "TRUNCATE xfer_exp.users_import").await;
        let n = import_data(&pg, &imp, "json", &jp, &json!({ "mapping": { "email": "email", "profile": "profile" } }), &noop())
            .await
            .unwrap();
        assert_eq!(n, 2500);
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users_import WHERE profile->>'n' IS NOT NULL").await, 2500);

        // Backup the schema, wipe it, restore it
        exec(&pg, db, "DROP TABLE xfer_exp.users_copy, xfer_exp.users_import").await;
        let bp = dir.join("backup.sql");
        let summary = backup_database(&pg, db, Some("xfer_exp"), &bp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains("2 tables, 1 views, 3700 rows"), "{summary}");
        exec(&pg, db, "DROP SCHEMA xfer_exp CASCADE; CREATE SCHEMA xfer_exp").await;
        let summary = restore_database(&pg, db, &bp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains(", 0 failed"), "{summary}");
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.users").await, 2500);
        assert_eq!(scalar(&pg, db, "SELECT count(*) FROM xfer_exp.big_orders").await, 200);
        // sequences were advanced past the restored ids
        assert_eq!(scalar(&pg, db, "INSERT INTO xfer_exp.users (email) VALUES ('new@x.io') RETURNING id").await, 2501);
        assert_eq!(scalar(&pg, db, "INSERT INTO xfer_exp.orders (user_id, total) VALUES (1, 1) RETURNING id").await, 1201);

        // continueOnError: restoring again over existing objects fails some statements but carries on
        let bad = dir.join("bad.sql");
        std::fs::write(&bad, "CREATE TABLE xfer_exp.t1 (a int);\nINSERT INTO nope VALUES (1);\nINSERT INTO xfer_exp.t1 VALUES (1);").unwrap();
        assert!(restore_database(&pg, db, &bad, &json!({}), &noop()).await.is_err());
        exec(&pg, db, "DROP TABLE xfer_exp.t1").await;
        let s = restore_database(&pg, db, &bad, &json!({ "continueOnError": true }), &noop()).await.unwrap();
        assert!(s.starts_with("Executed 2 of 3 statements, 1 failed"), "{s}");
        exec(&pg, db, "DROP SCHEMA xfer_exp CASCADE").await;
    }

    /// Each test seeds its own schema so the suite can run in parallel.
    async fn seed_pg(schema: &str) -> DynDriver {
        let pg = connect(pg_cfg()).await;
        exec(&pg, Some("devcat"), &PG_SEED.replace("xfer", schema)).await;
        pg
    }

    #[tokio::test]
    #[ignore]
    async fn pg_to_mssql_and_back() {
        let pg = seed_pg("xfer_ms").await;
        let ms = connect(mssql_cfg()).await;
        exec(&ms, Some("master"), "IF DB_ID('devcat_xfer_ms') IS NULL CREATE DATABASE devcat_xfer_ms").await;
        let tables = vec![t(Some("devcat"), Some("xfer_ms"), "orders"), t(Some("devcat"), Some("xfer_ms"), "users")];
        let target = TransferTarget { database: Some("devcat_xfer_ms".into()), schema: Some("dbo".into()) };
        let o = json!({ "dropExisting": true, "foreignKeys": true, "batchSize": 700 });
        let n = transfer_tables(&pg, &ms, &tables, &target, &o, &noop()).await.unwrap();
        assert_eq!(n, 3700);
        let msdb = Some("devcat_xfer_ms");
        assert_eq!(scalar(&ms, msdb, "SELECT COUNT(*) FROM dbo.users").await, 2500);
        assert_eq!(scalar(&ms, msdb, "SELECT COUNT(*) FROM dbo.users WHERE active = 1").await, 1250);
        assert_eq!(scalar(&ms, msdb, "SELECT COUNT(*) FROM dbo.users WHERE note IS NULL").await, 250);
        assert_eq!(scalar(&ms, msdb, "SELECT COUNT(*) FROM sys.foreign_keys WHERE parent_object_id = OBJECT_ID('dbo.orders')").await, 1);
        assert_eq!(scalar(&ms, msdb, "SELECT COUNT(*) FROM dbo.users WHERE JSON_VALUE(profile, '$.n') = CAST(id AS nvarchar(20))").await, 2500);
        // ids were preserved through IDENTITY_INSERT and identity keeps working
        assert_eq!(scalar(&ms, msdb, "SELECT MAX(id) FROM dbo.orders").await, 1200);
        exec(&ms, msdb, "INSERT INTO dbo.users (email) VALUES ('new@x.io')").await;
        assert_eq!(scalar(&ms, msdb, "SELECT MAX(id) FROM dbo.users").await, 2501);

        // and back into a fresh Postgres schema
        exec(&pg, Some("devcat"), "DROP SCHEMA IF EXISTS xfer_ms_back CASCADE; CREATE SCHEMA xfer_ms_back").await;
        let back = vec![t(msdb, Some("dbo"), "users"), t(msdb, Some("dbo"), "orders")];
        let target = TransferTarget { database: Some("devcat".into()), schema: Some("xfer_ms_back".into()) };
        let n = transfer_tables(&ms, &pg, &back, &target, &json!({ "foreignKeys": true }), &noop()).await.unwrap();
        assert_eq!(n, 3701);
        let db = Some("devcat");
        assert_eq!(
            scalar(&pg, db, "SELECT count(*) FROM xfer_ms.users u JOIN xfer_ms_back.users b USING (id) WHERE u.email = b.email AND u.active = b.active AND u.balance = b.balance AND u.avatar = b.avatar AND u.note IS NOT DISTINCT FROM b.note").await,
            2500
        );
        assert_eq!(scalar(&pg, db, "INSERT INTO xfer_ms_back.users (email) VALUES ('again@x.io') RETURNING id").await, 2502);

        // backup on MSSQL (identity inserts) and restore into a second database
        let dir = tdir("pg_to_mssql_and_back");
        let bp = dir.join("mssql.sql");
        let summary = backup_database(&ms, msdb, Some("dbo"), &bp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains("2 tables"), "{summary}");
        exec(&ms, Some("master"), "IF DB_ID('devcat_xfer_ms2') IS NULL CREATE DATABASE devcat_xfer_ms2").await;
        let s = restore_database(&ms, Some("devcat_xfer_ms2"), &bp, &json!({}), &noop()).await.unwrap();
        assert!(s.contains(", 0 failed"), "{s}");
        assert_eq!(scalar(&ms, Some("devcat_xfer_ms2"), "SELECT COUNT(*) FROM dbo.orders").await, 1200);
        exec(&ms, Some("master"), "DROP DATABASE devcat_xfer_ms2; DROP DATABASE devcat_xfer_ms").await;
        exec(&pg, db, "DROP SCHEMA xfer_ms_back CASCADE; DROP SCHEMA xfer_ms CASCADE").await;
    }

    #[tokio::test]
    #[ignore]
    async fn pg_mongo_roundtrip() {
        let pg = seed_pg("xfer_mg").await;
        let mg = connect(mongo_cfg()).await;
        let tables = vec![t(Some("devcat"), Some("xfer_mg"), "users")];
        let target = TransferTarget { database: Some("devcat_xfer_mg".into()), schema: None };
        let coll = t(Some("devcat_xfer_mg"), None, "users");
        let _ = delete_all(&mg, &coll).await;
        let n = transfer_tables(&pg, &mg, &tables, &target, &json!({ "truncate": true }), &noop()).await.unwrap();
        assert_eq!(n, 2500);
        let page = mg
            .table_data(&TableDataRequest { table: coll.clone(), offset: 0, limit: 5, filter: None, sort: vec![] })
            .await
            .unwrap();
        assert_eq!(page.result.rows.len(), 5);

        // Mongo backup to a directory and restore into another database
        let dir = tdir("pg_mongo_roundtrip").join("mongo-dump");
        let s = backup_database(&mg, Some("devcat_xfer_mg"), None, &dir, &json!({}), &noop()).await.unwrap();
        assert!(s.contains("2500 documents"), "{s}");
        let s = restore_database(&mg, Some("devcat_xfer_mg2"), &dir, &json!({ "dropExisting": true }), &noop()).await.unwrap();
        assert!(s.contains("2500 documents"), "{s}");

        // CSV import into a collection infers numbers/booleans
        let csv = tdir("pg_mongo_roundtrip").join("people.csv");
        std::fs::write(&csv, "name,age,admin,zip\nann,31,true,0123\nbob,,false,99\n").unwrap();
        let people = t(Some("devcat_xfer_mg2"), None, "people");
        let _ = delete_all(&mg, &people).await;
        assert_eq!(import_data(&mg, &people, "csv", &csv, &json!({}), &noop()).await.unwrap(), 2);
        let page = mg
            .table_data(&TableDataRequest { table: people.clone(), offset: 0, limit: 10, filter: Some("{\"name\": \"ann\"}".into()), sort: vec![] })
            .await
            .unwrap();
        let cols: Vec<&str> = page.result.columns.iter().map(|c| c.name.as_str()).collect();
        let row = &page.result.rows[0];
        assert_eq!(row[cols.iter().position(|c| *c == "age").unwrap()], json!(31));
        assert_eq!(row[cols.iter().position(|c| *c == "admin").unwrap()], json!(true));
        assert_eq!(row[cols.iter().position(|c| *c == "zip").unwrap()], json!("0123"));

        // Mongo -> Postgres creates a table from the inferred structure
        exec(&pg, Some("devcat"), "DROP SCHEMA IF EXISTS xfer_mg_mongo CASCADE; CREATE SCHEMA xfer_mg_mongo").await;
        let target = TransferTarget { database: Some("devcat".into()), schema: Some("xfer_mg_mongo".into()) };
        let n = transfer_tables(&mg, &pg, &[coll.clone()], &target, &json!({}), &noop()).await.unwrap();
        assert_eq!(n, 2500);
        assert_eq!(scalar(&pg, Some("devcat"), "SELECT count(*) FROM xfer_mg_mongo.users").await, 2500);
        exec(&pg, Some("devcat"), "DROP SCHEMA xfer_mg_mongo CASCADE; DROP SCHEMA xfer_mg CASCADE").await;
        let _ = mg.execute("db.dropDatabase()", &exec_opts(Some("devcat_xfer_mg"), None)).await;
        let _ = mg.execute("db.dropDatabase()", &exec_opts(Some("devcat_xfer_mg2"), None)).await;
    }

    #[tokio::test]
    #[ignore]
    async fn redis_is_unsupported() {
        let rd = connect(cfg("redis", 56379, "", "", "0")).await;
        let p = tdir("redis_is_unsupported").join("x.sql");
        assert!(matches!(backup_database(&rd, None, None, &p, &json!({}), &noop()).await, Err(AppError::Unsupported(_))));
        std::fs::write(&p, "a,b\n1,2\n").unwrap();
        let r = import_data(&rd, &t(None, None, "k"), "csv", &p, &json!({}), &noop()).await;
        assert!(matches!(r, Err(AppError::Unsupported(_))));
    }

    #[tokio::test]
    #[ignore]
    async fn pg_to_mysql_and_back() {
        let pg = seed_pg("xfer_mysql").await;
        let my = connect(mysql_cfg()).await;
        exec(&my, None, "DROP DATABASE IF EXISTS devcat_xfer_mysql; CREATE DATABASE devcat_xfer_mysql; DROP DATABASE IF EXISTS devcat_xfer_mysql2; CREATE DATABASE devcat_xfer_mysql2").await;
        let tables = vec![t(Some("devcat"), Some("xfer_mysql"), "users"), t(Some("devcat"), Some("xfer_mysql"), "orders")];
        let target = TransferTarget { database: Some("devcat_xfer_mysql".into()), schema: None };
        let n = transfer_tables(&pg, &my, &tables, &target, &json!({ "foreignKeys": true }), &noop()).await.unwrap();
        assert_eq!(n, 3700);
        let mdb = Some("devcat_xfer_mysql");
        assert_eq!(scalar(&my, mdb, "SELECT COUNT(*) FROM users WHERE active = 1").await, 1250);
        assert_eq!(scalar(&my, mdb, "SELECT COUNT(*) FROM users WHERE JSON_EXTRACT(profile, '$.n') = id").await, 2500);
        assert_eq!(scalar(&my, mdb, "SELECT COUNT(*) FROM users WHERE HEX(avatar) = CONCAT('00FF', LPAD(HEX(id % 256), 2, '0'))").await, 2500);
        assert_eq!(scalar(&my, mdb, "SELECT COUNT(*) FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = 'devcat_xfer_mysql'").await, 1);
        exec(&my, mdb, "INSERT INTO users (email) VALUES ('new@x.io')").await;
        assert_eq!(scalar(&my, mdb, "SELECT MAX(id) FROM users").await, 2501);

        // MySQL dump restored into another database
        let bp = tdir("pg_to_mysql_and_back").join("mysql.sql");
        let summary = backup_database(&my, mdb, None, &bp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains("2 tables"), "{summary}");
        let s = restore_database(&my, Some("devcat_xfer_mysql2"), &bp, &json!({}), &noop()).await.unwrap();
        assert!(s.contains(", 0 failed"), "{s}");
        assert_eq!(scalar(&my, Some("devcat_xfer_mysql2"), "SELECT COUNT(*) FROM orders").await, 1200);
        assert_eq!(scalar(&my, Some("devcat_xfer_mysql2"), "SELECT COUNT(*) FROM users").await, 2501);

        // back to Postgres: tinyint(1) -> boolean, json -> jsonb, longblob -> bytea
        exec(&pg, Some("devcat"), "DROP SCHEMA IF EXISTS xfer_mysql_my CASCADE; CREATE SCHEMA xfer_mysql_my").await;
        let back = vec![t(Some("devcat_xfer_mysql2"), None, "users"), t(Some("devcat_xfer_mysql2"), None, "orders")];
        let target = TransferTarget { database: Some("devcat".into()), schema: Some("xfer_mysql_my".into()) };
        assert_eq!(transfer_tables(&my, &pg, &back, &target, &json!({}), &noop()).await.unwrap(), 3701);
        assert_eq!(
            scalar(&pg, Some("devcat"), "SELECT count(*) FROM xfer_mysql.users u JOIN xfer_mysql_my.users b USING (id) WHERE u.active = b.active AND u.avatar = b.avatar AND u.profile = b.profile AND u.balance = b.balance").await,
            2500
        );
        // SQL export from MySQL targeting Postgres
        let ep = tdir("pg_to_mysql_and_back").join("my_to_pg.sql");
        let o = json!({ "targetDialect": "postgres", "includeCreate": true, "includeDrop": true, "tableName": "users_from_sql" });
        export_data(&my, ExportSource::Table(t(Some("devcat_xfer_mysql2"), None, "users")), "sql", &ep, &o, &noop()).await.unwrap();
        let s = restore_database(&pg, Some("devcat"), &ep, &json!({ "schema": "xfer_mysql_my" }), &noop()).await.unwrap();
        assert!(s.contains(", 0 failed"), "{s}");
        exec(&my, None, "DROP DATABASE devcat_xfer_mysql; DROP DATABASE devcat_xfer_mysql2").await;
        exec(&pg, Some("devcat"), "DROP SCHEMA xfer_mysql_my CASCADE; DROP TABLE IF EXISTS public.users_from_sql; DROP SCHEMA xfer_mysql CASCADE").await;
    }

    #[tokio::test]
    #[ignore]
    async fn pg_to_sqlite_backup_restore() {
        let pg = seed_pg("xfer_lt").await;
        let dir = tdir("pg_to_sqlite_backup_restore");
        let file = dir.join("copy.db");
        let file2 = dir.join("restored.db");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&file2);
        std::fs::File::create(&file).unwrap();
        std::fs::File::create(&file2).unwrap();
        let lite = connect(sqlite_cfg(&file)).await;
        let tables = vec![t(Some("devcat"), Some("xfer_lt"), "orders"), t(Some("devcat"), Some("xfer_lt"), "users")];
        let target = TransferTarget { database: Some("main".into()), schema: None };
        assert_eq!(transfer_tables(&pg, &lite, &tables, &target, &json!({}), &noop()).await.unwrap(), 3700);
        assert_eq!(scalar(&lite, None, "SELECT COUNT(*) FROM users WHERE active = 1").await, 1250);
        assert_eq!(scalar(&lite, None, "SELECT COUNT(*) FROM users WHERE hex(avatar) = '00FF' || printf('%02X', id % 256)").await, 2500);

        let bp = dir.join("sqlite.sql");
        let summary = backup_database(&lite, Some("main"), None, &bp, &json!({}), &noop()).await.unwrap();
        assert!(summary.contains("2 tables"), "{summary}");
        let lite2 = connect(sqlite_cfg(&file2)).await;
        let s = restore_database(&lite2, Some("main"), &bp, &json!({}), &noop()).await.unwrap();
        assert!(s.contains(", 0 failed"), "{s}");
        assert_eq!(scalar(&lite2, None, "SELECT COUNT(*) FROM orders").await, 1200);

        // CSV round trip through SQLite
        let csv = dir.join("lite.csv");
        export_data(&lite2, ExportSource::Table(t(Some("main"), None, "users")), "csv", &csv, &json!({}), &noop()).await.unwrap();
        exec(&lite2, None, "DELETE FROM orders; DELETE FROM users").await;
        assert_eq!(import_data(&lite2, &t(Some("main"), None, "users"), "csv", &csv, &json!({}), &noop()).await.unwrap(), 2500);
        exec(&pg, Some("devcat"), "DROP SCHEMA xfer_lt CASCADE").await;
    }
}
