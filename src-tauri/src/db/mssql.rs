//! Microsoft SQL Server driver (tiberius over tokio TCP).
//!
//! Tree: `[]` → databases; `[db]` → schemas; `[db, schema]` → folders
//! (`tables`, `views`, `procedures`, `functions`, `triggers`, `sequences`);
//! `[db, schema, folder]` → objects; `[db, schema, "tables", t]` → folders
//! `columns` / `indexes` / `foreign_keys`, each listing its items.
//!
//! Catalog queries use three-part names (`[db].sys.tables`) so pooled connections
//! never need to switch database. `execute` issues `USE [db]` first because user
//! scripts expect an implicit database context.
//!
//! Encryption: `ssl_mode` "disable" → no TLS; unset/"prefer" → TLS without
//! certificate validation; "require" → TLS (validated unless `options.trustCert`);
//! "verify-ca"/"verify-full" → TLS with validation.
use super::*;
use crate::db::sql_common::{literal, quote_ident, split_statements};
use crate::db::ssh::{open_endpoint, SshTunnel};
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use async_trait::async_trait;
use chrono::{DateTime as ChronoDateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use futures::TryStreamExt;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, Row, SqlBrowser, ToSql};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type TClient = Client<Compat<TcpStream>>;

/// Binary cells are rendered as `\x<hex>` of at most this many bytes.
const BINARY_CAP: usize = 4096;
/// Idle pooled connections older than this are pinged before reuse.
const IDLE_PING_AFTER: Duration = Duration::from_secs(30);
/// After `max_rows` is reached, keep draining up to this many rows so the batch
/// finishes normally; beyond that the batch is cancelled.
const DRAIN_LIMIT: usize = 100_000;

struct Conn {
    client: TClient,
    spid: i32,
    last_used: Instant,
}

struct Pool {
    config: Config,
    /// Connect via SQL Browser resolution (`host\\INSTANCE`); plain TCP otherwise
    /// (connect_named falls back to host:port when no instance is configured).
    use_browser: bool,
    idle: parking_lot::Mutex<Vec<Conn>>,
    sem: Semaphore,
}

/// A checked-out connection. Returned to the pool on drop only if `release()`
/// was called (i.e. the protocol stream is known to be in a clean state).
struct PooledConn<'a> {
    conn: Option<Conn>,
    pool: &'a Pool,
    healthy: bool,
    _permit: SemaphorePermit<'a>,
}

impl PooledConn<'_> {
    fn client(&mut self) -> &mut TClient {
        &mut self.conn.as_mut().expect("connection").client
    }
    fn spid(&self) -> i32 {
        self.conn.as_ref().map(|c| c.spid).unwrap_or(0)
    }
    fn release(&mut self) {
        self.healthy = true;
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if self.healthy {
            if let Some(mut c) = self.conn.take() {
                c.last_used = Instant::now();
                self.pool.idle.lock().push(c);
            }
        }
    }
}

/// Whether the connection is still usable after this error (severity 20+
/// errors, e.g. after `KILL`, terminate the session).
fn recoverable(e: &tiberius::error::Error) -> bool {
    matches!(e, tiberius::error::Error::Server(t) if t.class() < 20)
}

impl Pool {
    async fn open(&self) -> AppResult<Conn> {
        let mut config = self.config.clone();
        let mut client = loop {
            let connecting = async {
                if self.use_browser {
                    TcpStream::connect_named(&config).await.map_err(AppError::from)
                } else {
                    TcpStream::connect(config.get_addr()).await.map_err(AppError::from)
                }
            };
            let tcp = tokio::time::timeout(Duration::from_secs(15), connecting)
                .await
                .map_err(|_| AppError::msg(format!("Timed out connecting to {}", config.get_addr())))??;
            tcp.set_nodelay(true)?;
            match Client::connect(config.clone(), tcp.compat_write()).await {
                Ok(c) => break c,
                // Azure SQL gateway redirect.
                Err(tiberius::error::Error::Routing { host, port }) => {
                    config.host(&host);
                    config.port(port);
                }
                Err(e) => return Err(e.into()),
            }
        };
        let row = client.simple_query("SELECT CAST(@@SPID AS int)").await?.into_row().await?;
        let spid = row.and_then(|r| r.get::<i32, _>(0)).unwrap_or(0);
        Ok(Conn { client, spid, last_used: Instant::now() })
    }

    async fn get(&self) -> AppResult<PooledConn<'_>> {
        let permit = self.sem.acquire().await.map_err(|_| AppError::NotConnected("pool closed".into()))?;
        // Reuse idle connections, pinging those idle for a while (the server or a
        // NAT may have dropped them).
        let conn = loop {
            let idle = self.idle.lock().pop();
            match idle {
                Some(mut c) if c.last_used.elapsed() > IDLE_PING_AFTER => {
                    let ok = match c.client.simple_query("SELECT 1").await {
                        Ok(s) => s.into_results().await.is_ok(),
                        Err(_) => false,
                    };
                    if ok {
                        break c;
                    }
                }
                Some(c) => break c,
                None => break self.open().await?,
            }
        };
        Ok(PooledConn { conn: Some(conn), pool: self, healthy: false, _permit: permit })
    }
}

pub struct MssqlDriver {
    pool: Pool,
    default_db: Option<String>,
    read_only: bool,
    /// SPIDs of connections currently running `execute` (for `cancel`).
    running: parking_lot::Mutex<HashSet<i32>>,
    _tunnel: Option<SshTunnel>,
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

fn fmt_frac(nanos: u32, max_digits: usize) -> String {
    let digits = format!("{:09}", nanos);
    let d = digits[..max_digits].trim_end_matches('0');
    if d.is_empty() { String::new() } else { format!(".{d}") }
}

/// ISO-8601 without exceeding the precision SQL Server accepts back
/// (3 fractional digits for datetime, 7 for datetime2/time).
fn fmt_naive(dt: &NaiveDateTime, max_digits: usize) -> String {
    format!("{}{}", dt.format("%Y-%m-%dT%H:%M:%S"), fmt_frac(dt.nanosecond(), max_digits))
}

fn fmt_time(t: &NaiveTime) -> String {
    format!("{}{}", t.format("%H:%M:%S"), fmt_frac(t.nanosecond(), 7))
}

fn hex_cell(b: &[u8]) -> Value {
    let shown = &b[..b.len().min(BINARY_CAP)];
    let mut s = format!("\\x{}", hex::encode(shown));
    if b.len() > BINARY_CAP {
        s.push_str(&format!("… ({} bytes)", b.len()));
    }
    Value::String(s)
}

fn float_cell(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or_else(|| Value::String(f.to_string()))
}

/// Convert one cell to JSON. Never panics on unknown/unsupported types.
fn cell(data: &ColumnData<'static>, ty: Option<ColumnType>) -> Value {
    let money = matches!(ty, Some(ColumnType::Money) | Some(ColumnType::Money4));
    match data {
        ColumnData::U8(v) => v.map(|x| json!(x)).unwrap_or(Value::Null),
        ColumnData::I16(v) => v.map(|x| json!(x)).unwrap_or(Value::Null),
        ColumnData::I32(v) => v.map(|x| json!(x)).unwrap_or(Value::Null),
        ColumnData::I64(v) => v.map(|x| json!(x)).unwrap_or(Value::Null),
        ColumnData::F32(v) => v.map(|x| float_cell(x as f64)).unwrap_or(Value::Null),
        ColumnData::F64(v) if money => v.map(|x| Value::String(format!("{x:.4}"))).unwrap_or(Value::Null),
        ColumnData::F64(v) => v.map(float_cell).unwrap_or(Value::Null),
        ColumnData::Bit(v) => v.map(Value::Bool).unwrap_or(Value::Null),
        ColumnData::String(v) => v.as_ref().map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        ColumnData::Guid(v) => v.map(|g| Value::String(g.to_string().to_uppercase())).unwrap_or(Value::Null),
        ColumnData::Binary(v) => v.as_ref().map(|b| hex_cell(b)).unwrap_or(Value::Null),
        ColumnData::Numeric(v) => v.map(|n| Value::String(n.to_string())).unwrap_or(Value::Null),
        ColumnData::Xml(v) => v.as_ref().map(|x| Value::String(x.to_string())).unwrap_or(Value::Null),
        ColumnData::DateTime(None)
        | ColumnData::SmallDateTime(None)
        | ColumnData::DateTime2(None)
        | ColumnData::Date(None)
        | ColumnData::Time(None)
        | ColumnData::DateTimeOffset(None) => Value::Null,
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) => NaiveDateTime::from_sql(data)
            .ok()
            .flatten()
            .map(|d| Value::String(fmt_naive(&d, 3)))
            .unwrap_or(Value::Null),
        ColumnData::DateTime2(_) => NaiveDateTime::from_sql(data)
            .ok()
            .flatten()
            .map(|d| Value::String(fmt_naive(&d, 7)))
            .unwrap_or(Value::Null),
        ColumnData::Date(_) => NaiveDate::from_sql(data)
            .ok()
            .flatten()
            .map(|d| Value::String(d.format("%Y-%m-%d").to_string()))
            .unwrap_or(Value::Null),
        ColumnData::Time(_) => NaiveTime::from_sql(data).ok().flatten().map(|t| Value::String(fmt_time(&t))).unwrap_or(Value::Null),
        ColumnData::DateTimeOffset(_) => ChronoDateTime::<FixedOffset>::from_sql(data)
            .ok()
            .flatten()
            .map(|d| Value::String(format!("{}{}", fmt_naive(&d.naive_local(), 7), d.format("%:z"))))
            .unwrap_or(Value::Null),
    }
}

fn type_name(t: ColumnType) -> &'static str {
    match t {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigBinary => "binary",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::Text => "text",
        ColumnType::Image => "image",
        ColumnType::NText => "ntext",
        ColumnType::SSVariant => "sql_variant",
    }
}

fn row_values(row: &Row) -> Vec<Value> {
    let types: Vec<ColumnType> = row.columns().iter().map(|c| c.column_type()).collect();
    row.cells().enumerate().map(|(i, (_, d))| cell(d, types.get(i).copied())).collect()
}

fn vs(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn vo(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        other => Some(vs(other)),
    }
}

fn vi(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::Bool(b) => *b as i64,
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn vb(v: &Value) -> bool {
    vi(v) != 0
}

/// `((0))` → `0`, `(getdate())` → `getdate()`.
fn strip_parens(s: &str) -> String {
    let mut s = s.trim();
    while s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        let mut depth = 0i32;
        let balanced = inner.chars().all(|c| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth >= 0
        });
        if !balanced || depth != 0 {
            break;
        }
        s = inner.trim();
    }
    s.to_string()
}

/// Human readable SQL type from sys.types info.
fn format_type(name: &str, max_len: i64, precision: i64, scale: i64) -> String {
    match name {
        "varchar" | "char" | "varbinary" | "binary" => {
            if max_len == -1 { format!("{name}(max)") } else { format!("{name}({max_len})") }
        }
        "nvarchar" | "nchar" => {
            if max_len == -1 { format!("{name}(max)") } else { format!("{name}({})", max_len / 2) }
        }
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" => format!("{name}({scale})"),
        "float" if precision != 53 => format!("float({precision})"),
        _ => name.to_string(),
    }
}

fn q(s: &str) -> String {
    quote_ident(Dialect::Mssql, s)
}

fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

fn action(s: &str) -> String {
    s.replace('_', " ")
}

const FOLDERS: &[(&str, &str)] = &[
    ("tables", "Tables"),
    ("views", "Views"),
    ("procedures", "Procedures"),
    ("functions", "Functions"),
    ("triggers", "Triggers"),
    ("sequences", "Sequences"),
];

/// Bulk structure info for a set of tables; filled by `load_structures`.
#[derive(Default)]
struct StructRows {
    tables: BTreeMap<(String, String), TableStructure>,
}

impl MssqlDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> AppResult<Self> {
        let ssh_on = cfg.ssh.as_ref().map(|s| s.enabled).unwrap_or(false);
        let mut config = match cfg.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(url) if url.to_lowercase().starts_with("jdbc:") => Config::from_jdbc_string(url)?,
            Some(url) => Config::from_ado_string(url)?,
            None => Config::new(),
        };

        // Split `host\INSTANCE`.
        let (host, instance) = match cfg.host.split_once('\\') {
            Some((h, i)) => (h.to_string(), Some(i.to_string())),
            None => (cfg.host.clone(), None),
        };
        let has_url = cfg.url.as_deref().map(str::trim).is_some_and(|u| !u.is_empty());
        let mut tunnel = None;
        if !has_url || ssh_on {
            let mut c = cfg.clone();
            c.host = host.clone();
            if has_url {
                // Tunnel to the server named in the connection string.
                if let Some((h, p)) = config.get_addr().rsplit_once(':') {
                    c.host = h.to_string();
                    c.port = p.parse().ok();
                }
            }
            let ep = open_endpoint(&c, 1433).await?;
            config.host(&ep.host);
            if ep.tunnel.is_none() && instance.is_some() && cfg.port.is_none() && !has_url {
                // Resolve the port through SQL Browser (UDP 1434).
                config.instance_name(instance.clone().unwrap_or_default());
            } else {
                config.port(ep.port);
            }
            tunnel = ep.tunnel;
        }
        if !has_url {
            if !cfg.user.is_empty() {
                config.authentication(AuthMethod::sql_server(&cfg.user, cfg.password.as_deref().unwrap_or("")));
            }
            let trust_opt = cfg.options.get("trustCert").map(|v| v == "true").unwrap_or(false);
            match cfg.ssl_mode.as_deref() {
                Some("disable") => config.encryption(EncryptionLevel::NotSupported),
                Some("require") => {
                    config.encryption(EncryptionLevel::Required);
                    if trust_opt {
                        config.trust_cert();
                    }
                }
                Some("verify-ca") | Some("verify-full") => config.encryption(EncryptionLevel::Required),
                _ => {
                    config.encryption(EncryptionLevel::On);
                    config.trust_cert();
                }
            }
        }
        if let Some(db) = cfg.database.as_deref().filter(|d| !d.is_empty()) {
            config.database(db);
        }
        config.application_name("DevCat");

        let size = cfg.options.get("poolSize").and_then(|s| s.parse().ok()).unwrap_or(4usize).clamp(1, 16);
        let pool = Pool { config, use_browser: tunnel.is_none(), idle: parking_lot::Mutex::new(Vec::new()), sem: Semaphore::new(size) };
        // Fail fast on bad credentials.
        let first = pool.open().await?;
        pool.idle.lock().push(first);
        Ok(MssqlDriver {
            pool,
            default_db: cfg.database.clone().filter(|d| !d.is_empty()),
            read_only: cfg.read_only,
            running: parking_lot::Mutex::new(HashSet::new()),
            _tunnel: tunnel,
        })
    }

    fn db_of<'a>(&'a self, db: Option<&'a str>) -> AppResult<&'a str> {
        db.filter(|d| !d.is_empty())
            .or(self.default_db.as_deref())
            .ok_or_else(|| AppError::msg("No database selected"))
    }

    /// Run a parameterized query and return the first result set as JSON rows.
    /// Retries once on a fresh connection if a pooled connection turned out dead.
    async fn rows(&self, sql: &str, params: &[&dyn ToSql]) -> AppResult<Vec<Vec<Value>>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut c = self.pool.get().await?;
            let res: Result<Vec<Row>, tiberius::error::Error> = async {
                let stream = c.client().query(sql, params).await?;
                stream.into_first_result().await
            }
            .await;
            match res {
                Ok(rows) => {
                    c.release();
                    return Ok(rows.iter().map(row_values).collect());
                }
                Err(e) if recoverable(&e) => {
                    c.release();
                    return Err(e.into());
                }
                Err(e) if attempt == 1 && matches!(e, tiberius::error::Error::Io { .. }) => {
                    // Likely a server restart: drop every idle connection and retry once.
                    self.pool.idle.lock().clear();
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Run one batch and collect each result set (plus a rows-affected result
    /// for batches without result sets).
    async fn run_batch(client: &mut TClient, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>, tiberius::error::Error> {
        let started = Instant::now();
        let mut results: Vec<QueryResult> = Vec::new();
        let mut cancelled = false;
        {
            let mut stream = client.simple_query(sql.to_string()).await?;
            let mut extra = 0usize;
            while let Some(item) = stream.try_next().await? {
                if let Some(meta) = item.as_metadata() {
                    results.push(QueryResult {
                        statement: sql.to_string(),
                        columns: meta
                            .columns()
                            .iter()
                            .map(|c| ColumnInfo { name: c.name().to_string(), type_name: type_name(c.column_type()).into() })
                            .collect(),
                        ..Default::default()
                    });
                    continue;
                }
                let Some(row) = item.into_row() else { continue };
                if results.is_empty() {
                    results.push(QueryResult { statement: sql.to_string(), ..Default::default() });
                }
                let r = results.last_mut().expect("result");
                if r.rows.len() < max_rows {
                    r.rows.push(row_values(&row));
                } else {
                    r.truncated = true;
                    extra += 1;
                    if extra > DRAIN_LIMIT {
                        cancelled = true;
                        break;
                    }
                }
            }
        }
        if cancelled {
            client.cancel_query().await?;
            if let Some(r) = results.last_mut() {
                r.message = Some("Result was too large; the rest of this batch was cancelled.".into());
            }
        }
        if results.is_empty() {
            // @@ROWCOUNT survives into the next batch on the same session.
            let n = client
                .simple_query("SELECT CAST(@@ROWCOUNT AS bigint)")
                .await?
                .into_row()
                .await?
                .and_then(|r| r.get::<i64, _>(0))
                .unwrap_or(0);
            results.push(QueryResult {
                statement: sql.to_string(),
                rows_affected: Some(n.max(0) as u64),
                message: Some(format!("{n} row(s) affected")),
                ..Default::default()
            });
        }
        let elapsed = started.elapsed().as_millis() as u64;
        for r in &mut results {
            r.elapsed_ms = elapsed;
        }
        Ok(results)
    }

    async fn load_structures(&self, db: &str, schema: Option<&str>, table: Option<&str>) -> AppResult<StructRows> {
        let d = q(db);
        let mut filter = String::new();
        if let Some(s) = schema {
            filter.push_str(&format!(" AND s.name = {}", lit(s)));
        }
        if let Some(t) = table {
            filter.push_str(&format!(" AND t.name = {}", lit(t)));
        }
        let mut out = StructRows::default();

        let tables = self
            .rows(
                &format!(
                    "SELECT s.name, t.name, CAST(ep.value AS nvarchar(max))
                     FROM {d}.sys.tables t JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                     LEFT JOIN {d}.sys.extended_properties ep ON ep.major_id = t.object_id AND ep.minor_id = 0
                          AND ep.class = 1 AND ep.name = 'MS_Description'
                     WHERE t.is_ms_shipped = 0{filter} ORDER BY s.name, t.name"
                ),
                &[],
            )
            .await?;
        for r in tables {
            let key = (vs(&r[0]), vs(&r[1]));
            out.tables.insert(
                key.clone(),
                TableStructure {
                    table: Some(TableRef { database: Some(db.into()), schema: Some(key.0), name: key.1 }),
                    comment: vo(&r[2]),
                    ..Default::default()
                },
            );
        }
        if out.tables.is_empty() {
            return Ok(out);
        }

        let cols = self
            .rows(
                &format!(
                    "SELECT s.name, t.name, c.name, ty.name, CAST(c.max_length AS int), CAST(c.precision AS int),
                            CAST(c.scale AS int), c.is_nullable, c.is_identity, dc.definition,
                            CAST(ep.value AS nvarchar(max)), cc.definition, c.column_id,
                            CAST(CASE WHEN pk.column_id IS NULL THEN 0 ELSE 1 END AS bit), ty.is_user_defined
                     FROM {d}.sys.columns c
                     JOIN {d}.sys.tables t ON t.object_id = c.object_id
                     JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                     JOIN {d}.sys.types ty ON ty.user_type_id = c.user_type_id
                     LEFT JOIN {d}.sys.default_constraints dc ON dc.object_id = c.default_object_id
                     LEFT JOIN {d}.sys.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
                     LEFT JOIN {d}.sys.extended_properties ep ON ep.major_id = c.object_id AND ep.minor_id = c.column_id
                          AND ep.class = 1 AND ep.name = 'MS_Description'
                     LEFT JOIN (SELECT ic.object_id, ic.column_id FROM {d}.sys.index_columns ic
                                JOIN {d}.sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
                                WHERE i.is_primary_key = 1) pk ON pk.object_id = c.object_id AND pk.column_id = c.column_id
                     WHERE t.is_ms_shipped = 0{filter}
                     ORDER BY s.name, t.name, c.column_id"
                ),
                &[],
            )
            .await?;
        for r in cols {
            let Some(ts) = out.tables.get_mut(&(vs(&r[0]), vs(&r[1]))) else { continue };
            let computed = vo(&r[11]);
            let comment = vo(&r[10]).or_else(|| computed.as_ref().map(|c| format!("computed: {c}")));
            ts.columns.push(ColumnDef {
                name: vs(&r[2]),
                data_type: format_type(&vs(&r[3]), vi(&r[4]), vi(&r[5]), vi(&r[6])),
                nullable: vb(&r[7]),
                default: vo(&r[9]).map(|s| strip_parens(&s)),
                is_primary_key: vb(&r[13]),
                auto_increment: vb(&r[8]),
                comment,
                ordinal: vi(&r[12]) as u32,
            });
        }

        let idx = self
            .rows(
                &format!(
                    "SELECT s.name, t.name, i.name, i.is_unique, i.is_primary_key, i.type_desc, c.name
                     FROM {d}.sys.indexes i
                     JOIN {d}.sys.tables t ON t.object_id = i.object_id
                     JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                     JOIN {d}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                     JOIN {d}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                     WHERE i.name IS NOT NULL AND ic.is_included_column = 0 AND t.is_ms_shipped = 0{filter}
                     ORDER BY s.name, t.name, i.index_id, ic.key_ordinal"
                ),
                &[],
            )
            .await?;
        for r in idx {
            let Some(ts) = out.tables.get_mut(&(vs(&r[0]), vs(&r[1]))) else { continue };
            let name = vs(&r[2]);
            match ts.indexes.iter_mut().find(|i| i.name == name) {
                Some(i) => i.columns.push(vs(&r[6])),
                None => ts.indexes.push(IndexDef {
                    name,
                    columns: vec![vs(&r[6])],
                    unique: vb(&r[3]),
                    primary: vb(&r[4]),
                    method: Some(vs(&r[5]).to_lowercase()),
                }),
            }
        }

        let fks = self
            .rows(
                &format!(
                    "SELECT s.name, t.name, fk.name, pc.name, rs.name, rt.name, rc.name,
                            fk.update_referential_action_desc, fk.delete_referential_action_desc
                     FROM {d}.sys.foreign_keys fk
                     JOIN {d}.sys.tables t ON t.object_id = fk.parent_object_id
                     JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                     JOIN {d}.sys.tables rt ON rt.object_id = fk.referenced_object_id
                     JOIN {d}.sys.schemas rs ON rs.schema_id = rt.schema_id
                     JOIN {d}.sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
                     JOIN {d}.sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
                     JOIN {d}.sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
                     WHERE t.is_ms_shipped = 0{filter}
                     ORDER BY s.name, t.name, fk.name, fkc.constraint_column_id"
                ),
                &[],
            )
            .await?;
        for r in fks {
            let Some(ts) = out.tables.get_mut(&(vs(&r[0]), vs(&r[1]))) else { continue };
            let name = vs(&r[2]);
            match ts.foreign_keys.iter_mut().find(|f| f.name == name) {
                Some(f) => {
                    f.columns.push(vs(&r[3]));
                    f.ref_columns.push(vs(&r[6]));
                }
                None => ts.foreign_keys.push(ForeignKeyDef {
                    name,
                    columns: vec![vs(&r[3])],
                    ref_schema: Some(vs(&r[4])),
                    ref_table: vs(&r[5]),
                    ref_columns: vec![vs(&r[6])],
                    on_update: Some(action(&vs(&r[7]))),
                    on_delete: Some(action(&vs(&r[8]))),
                }),
            }
        }
        Ok(out)
    }

    async fn table_ddl(&self, db: &str, schema: &str, name: &str) -> AppResult<String> {
        let d = q(db);
        let p_schema = schema.to_string();
        let p_name = name.to_string();
        let params: [&dyn ToSql; 2] = [&p_schema, &p_name];
        let obj = format!(
            "(SELECT t.object_id FROM {d}.sys.tables t JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
              WHERE s.name = @P1 AND t.name = @P2)"
        );
        let cols = self
            .rows(
                &format!(
                    "SELECT c.name, ty.name, CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int),
                            c.is_nullable, c.is_identity, CAST(ic.seed_value AS nvarchar(50)),
                            CAST(ic.increment_value AS nvarchar(50)), dc.name, dc.definition, cc.definition,
                            cc.is_persisted, c.collation_name, ty.is_user_defined, SCHEMA_NAME(ty.schema_id), dc.is_system_named
                     FROM {d}.sys.columns c
                     JOIN {d}.sys.types ty ON ty.user_type_id = c.user_type_id
                     LEFT JOIN {d}.sys.identity_columns ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id
                     LEFT JOIN {d}.sys.default_constraints dc ON dc.object_id = c.default_object_id
                     LEFT JOIN {d}.sys.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
                     WHERE c.object_id = {obj} ORDER BY c.column_id"
                ),
                &params,
            )
            .await?;
        if cols.is_empty() {
            return Err(AppError::msg(format!("Table {schema}.{name} not found")));
        }
        let full = format!("{}.{}", q(schema), q(name));
        let mut lines: Vec<String> = Vec::new();
        for r in &cols {
            if let Some(expr) = vo(&r[11]) {
                lines.push(format!("    {} AS {}{}", q(&vs(&r[0])), expr, if vb(&r[12]) { " PERSISTED" } else { "" }));
                continue;
            }
            let ty = if vb(&r[14]) {
                format!("{}.{}", q(&vs(&r[15])), q(&vs(&r[1])))
            } else {
                format_type(&vs(&r[1]), vi(&r[2]), vi(&r[3]), vi(&r[4]))
            };
            let mut l = format!("    {} {}", q(&vs(&r[0])), ty);
            if vb(&r[6]) {
                l.push_str(&format!(" IDENTITY({},{})", vs(&r[7]), vs(&r[8])));
            }
            l.push_str(if vb(&r[5]) { " NULL" } else { " NOT NULL" });
            if let Some(def) = vo(&r[10]) {
                if vb(&r[16]) {
                    l.push_str(&format!(" DEFAULT {def}"));
                } else {
                    l.push_str(&format!(" CONSTRAINT {} DEFAULT {}", q(&vs(&r[9])), def));
                }
            }
            lines.push(l);
        }

        // Key / unique constraints.
        let keys = self
            .rows(
                &format!(
                    "SELECT kc.name, kc.type, i.type_desc, c.name, ic.is_descending_key, kc.is_system_named
                     FROM {d}.sys.key_constraints kc
                     JOIN {d}.sys.indexes i ON i.object_id = kc.parent_object_id AND i.index_id = kc.unique_index_id
                     JOIN {d}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                     JOIN {d}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                     WHERE kc.parent_object_id = {obj} ORDER BY kc.type, kc.name, ic.key_ordinal"
                ),
                &params,
            )
            .await?;
        let mut constraint_index_names: HashSet<String> = HashSet::new();
        let mut grouped: Vec<(String, String, String, Vec<String>, bool)> = Vec::new();
        for r in &keys {
            let name = vs(&r[0]);
            let col = format!("{} {}", q(&vs(&r[3])), if vb(&r[4]) { "DESC" } else { "ASC" });
            match grouped.iter_mut().find(|g| g.0 == name) {
                Some(g) => g.3.push(col),
                None => grouped.push((name.clone(), vs(&r[1]), vs(&r[2]), vec![col], vb(&r[5]))),
            }
            constraint_index_names.insert(name);
        }
        for (name, kind, type_desc, cols, system_named) in grouped {
            let what = if kind.trim() == "PK" { "PRIMARY KEY" } else { "UNIQUE" };
            let named = if system_named { String::new() } else { format!("CONSTRAINT {} ", q(&name)) };
            lines.push(format!("    {named}{what} {type_desc} ({})", cols.join(", ")));
        }

        // Check constraints.
        for r in self
            .rows(
                &format!("SELECT name, definition, is_system_named FROM {d}.sys.check_constraints WHERE parent_object_id = {obj} ORDER BY name"),
                &params,
            )
            .await?
        {
            let named = if vb(&r[2]) { String::new() } else { format!("CONSTRAINT {} ", q(&vs(&r[0]))) };
            lines.push(format!("    {named}CHECK {}", vs(&r[1])));
        }

        // Foreign keys.
        let ts = self.load_structures(db, Some(schema), Some(name)).await?;
        if let Some(t) = ts.tables.values().next() {
            for fk in &t.foreign_keys {
                let mut l = format!(
                    "    CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}.{} ({})",
                    q(&fk.name),
                    fk.columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "),
                    q(fk.ref_schema.as_deref().unwrap_or("dbo")),
                    q(&fk.ref_table),
                    fk.ref_columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
                );
                if let Some(a) = fk.on_delete.as_deref().filter(|a| *a != "NO ACTION") {
                    l.push_str(&format!(" ON DELETE {a}"));
                }
                if let Some(a) = fk.on_update.as_deref().filter(|a| *a != "NO ACTION") {
                    l.push_str(&format!(" ON UPDATE {a}"));
                }
                lines.push(l);
            }
        }

        let mut out = format!("CREATE TABLE {full} (\n{}\n);\n", lines.join(",\n"));

        // Other indexes.
        let idx = self
            .rows(
                &format!(
                    "SELECT i.name, i.is_unique, i.type_desc, c.name, ic.is_descending_key, ic.is_included_column, i.filter_definition
                     FROM {d}.sys.indexes i
                     JOIN {d}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                     JOIN {d}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                     WHERE i.object_id = {obj} AND i.name IS NOT NULL AND i.is_primary_key = 0
                           AND i.is_unique_constraint = 0
                     ORDER BY i.index_id, ic.is_included_column, ic.key_ordinal, ic.index_column_id"
                ),
                &params,
            )
            .await?;
        let mut ix: Vec<(String, bool, String, Vec<String>, Vec<String>, Option<String>)> = Vec::new();
        for r in &idx {
            let name = vs(&r[0]);
            if constraint_index_names.contains(&name) {
                continue;
            }
            if !ix.iter().any(|i| i.0 == name) {
                ix.push((name.clone(), vb(&r[1]), vs(&r[2]), vec![], vec![], vo(&r[6])));
            }
            let e = ix.iter_mut().find(|i| i.0 == name).expect("index");
            if vb(&r[5]) {
                e.4.push(q(&vs(&r[3])));
            } else {
                e.3.push(format!("{} {}", q(&vs(&r[3])), if vb(&r[4]) { "DESC" } else { "ASC" }));
            }
        }
        for (name, unique, type_desc, cols, incl, filter) in ix {
            out.push_str(&format!(
                "\nCREATE {}{} INDEX {} ON {}",
                if unique { "UNIQUE " } else { "" },
                type_desc,
                q(&name),
                full
            ));
            if !cols.is_empty() {
                out.push_str(&format!(" ({})", cols.join(", ")));
            }
            if !incl.is_empty() {
                out.push_str(&format!(" INCLUDE ({})", incl.join(", ")));
            }
            if let Some(f) = filter {
                out.push_str(&format!(" WHERE {f}"));
            }
            out.push_str(";\n");
        }

        // Comments.
        if let Some(t) = ts.tables.values().next() {
            let add = |level2: Option<&str>, text: &str| {
                let mut s = format!(
                    "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = {}, @level0type = N'SCHEMA', @level0name = {}, @level1type = N'TABLE', @level1name = {}",
                    lit(text),
                    lit(schema),
                    lit(name)
                );
                if let Some(c) = level2 {
                    s.push_str(&format!(", @level2type = N'COLUMN', @level2name = {}", lit(c)));
                }
                s.push_str(";\n");
                s
            };
            let mut comments = String::new();
            if let Some(c) = &t.comment {
                comments.push_str(&add(None, c));
            }
            for c in &t.columns {
                if let Some(text) = c.comment.as_deref().filter(|t| !t.starts_with("computed: ")) {
                    comments.push_str(&add(Some(&c.name), text));
                }
            }
            if !comments.is_empty() {
                out.push('\n');
                out.push_str(&comments);
            }
        }
        Ok(out)
    }

    async fn primary_key(&self, db: &str, schema: &str, table: &str) -> AppResult<Vec<String>> {
        let d = q(db);
        let (s, t) = (schema.to_string(), table.to_string());
        let rows = self
            .rows(
                &format!(
                    "SELECT c.name FROM {d}.sys.indexes i
                     JOIN {d}.sys.objects o ON o.object_id = i.object_id
                     JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id
                     JOIN {d}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                     JOIN {d}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                     WHERE i.is_primary_key = 1 AND s.name = @P1 AND o.name = @P2 ORDER BY ic.key_ordinal"
                ),
                &[&s, &t],
            )
            .await?;
        Ok(rows.iter().map(|r| vs(&r[0])).collect())
    }

    fn table_parts<'a>(&'a self, t: &'a TableRef) -> AppResult<(&'a str, &'a str, &'a str)> {
        Ok((self.db_of(t.database.as_deref())?, t.schema.as_deref().unwrap_or("dbo"), t.name.as_str()))
    }
}

/// Render a grid value for a column, turning `\x..` strings into binary literals
/// for binary columns.
fn value_sql(v: &Value, col_type: Option<&str>) -> String {
    if let (Value::String(s), Some(t)) = (v, col_type) {
        let t = t.to_lowercase();
        if t.starts_with("varbinary") || t.starts_with("binary") || t == "image" || t == "timestamp" || t == "rowversion" {
            if let Some(h) = s.strip_prefix("\\x").or_else(|| s.strip_prefix("0x")) {
                if h.chars().all(|c| c.is_ascii_hexdigit()) {
                    return format!("0x{h}");
                }
            }
        }
    }
    literal(Dialect::Mssql, v)
}

#[async_trait]
impl Driver for MssqlDriver {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Mssql
    }

    fn dialect(&self) -> Dialect {
        Dialect::Mssql
    }

    async fn server_version(&self) -> AppResult<String> {
        let r = self.rows("SELECT @@VERSION", &[]).await?;
        Ok(r.first()
            .map(|r| vs(&r[0]).lines().next().unwrap_or("").trim().to_string())
            .unwrap_or_else(|| "SQL Server".into()))
    }

    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>> {
        let node = |path: Vec<String>, label: String, kind: NodeKind, has_children: bool, detail: Option<String>, table: Option<TableRef>| TreeNode {
            path,
            label,
            kind,
            has_children,
            detail,
            table,
        };
        match path {
            [] => Ok(self
                .list_databases()
                .await?
                .into_iter()
                .map(|d| node(vec![d.clone()], d, NodeKind::Database, true, None, None))
                .collect()),
            [db] => {
                let d = q(db);
                let rows = self
                    .rows(
                        &format!(
                            "SELECT name FROM {d}.sys.schemas
                             WHERE schema_id < 16384 AND name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest')
                                OR schema_id IN (SELECT schema_id FROM {d}.sys.objects WHERE is_ms_shipped = 0)
                             ORDER BY name"
                        ),
                        &[],
                    )
                    .await?;
                Ok(rows
                    .iter()
                    .map(|r| {
                        let s = vs(&r[0]);
                        node(vec![db.clone(), s.clone()], s, NodeKind::Schema, true, None, None)
                    })
                    .collect())
            }
            [db, schema] => Ok(FOLDERS
                .iter()
                .map(|(id, label)| node(vec![db.clone(), schema.clone(), id.to_string()], label.to_string(), NodeKind::Folder, true, None, None))
                .collect()),
            [db, schema, folder] => {
                let d = q(db);
                let s = schema.clone();
                let (sql, kind, expandable) = match folder.as_str() {
                    "tables" => (
                        format!(
                            "SELECT t.name, CAST(SUM(p.rows) AS bigint) FROM {d}.sys.tables t
                             JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                             LEFT JOIN {d}.sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1)
                             WHERE s.name = @P1 GROUP BY t.name ORDER BY t.name"
                        ),
                        NodeKind::Table,
                        true,
                    ),
                    "views" => (
                        format!(
                            "SELECT v.name, NULL FROM {d}.sys.views v JOIN {d}.sys.schemas s ON s.schema_id = v.schema_id
                             WHERE s.name = @P1 ORDER BY v.name"
                        ),
                        NodeKind::View,
                        false,
                    ),
                    "procedures" => (
                        format!(
                            "SELECT p.name, NULL FROM {d}.sys.procedures p JOIN {d}.sys.schemas s ON s.schema_id = p.schema_id
                             WHERE s.name = @P1 ORDER BY p.name"
                        ),
                        NodeKind::Procedure,
                        false,
                    ),
                    "functions" => (
                        format!(
                            "SELECT o.name, CASE WHEN o.type IN ('FN', 'FS') THEN 'scalar' ELSE 'table' END
                             FROM {d}.sys.objects o JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id
                             WHERE s.name = @P1 AND o.type IN ('FN', 'IF', 'TF', 'FS', 'FT') ORDER BY o.name"
                        ),
                        NodeKind::Function,
                        false,
                    ),
                    "triggers" => (
                        format!(
                            "SELECT tr.name, OBJECT_NAME(tr.parent_id, DB_ID({})) FROM {d}.sys.triggers tr
                             JOIN {d}.sys.objects o ON o.object_id = tr.parent_id
                             JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id
                             WHERE s.name = @P1 AND tr.parent_class = 1 ORDER BY tr.name",
                            lit(db)
                        ),
                        NodeKind::Trigger,
                        false,
                    ),
                    "sequences" => (
                        format!(
                            "SELECT q.name, NULL FROM {d}.sys.sequences q JOIN {d}.sys.schemas s ON s.schema_id = q.schema_id
                             WHERE s.name = @P1 ORDER BY q.name"
                        ),
                        NodeKind::Sequence,
                        false,
                    ),
                    _ => return Ok(vec![]),
                };
                let rows = self.rows(&sql, &[&s]).await?;
                Ok(rows
                    .iter()
                    .map(|r| {
                        let name = vs(&r[0]);
                        let detail = match (&kind, &r[1]) {
                            (NodeKind::Table, v) if !v.is_null() => Some(format!("{} rows", vi(v))),
                            (_, v) => vo(v),
                        };
                        node(
                            vec![db.clone(), schema.clone(), folder.clone(), name.clone()],
                            name.clone(),
                            kind.clone(),
                            expandable,
                            detail,
                            Some(TableRef { database: Some(db.clone()), schema: Some(schema.clone()), name }),
                        )
                    })
                    .collect())
            }
            [db, schema, folder, table] if folder == "tables" => Ok([("columns", "Columns"), ("indexes", "Indexes"), ("foreign_keys", "Foreign Keys")]
                .iter()
                .map(|(id, label)| {
                    node(
                        vec![db.clone(), schema.clone(), folder.clone(), table.clone(), id.to_string()],
                        label.to_string(),
                        NodeKind::Folder,
                        true,
                        None,
                        None,
                    )
                })
                .collect()),
            [db, schema, folder, table, sub] if folder == "tables" => {
                let st = self.load_structures(db, Some(schema), Some(table)).await?;
                let Some(t) = st.tables.into_values().next() else { return Ok(vec![]) };
                let base = path.to_vec();
                let child = |name: &str| {
                    let mut p = base.clone();
                    p.push(name.to_string());
                    p
                };
                Ok(match sub.as_str() {
                    "columns" => t
                        .columns
                        .iter()
                        .map(|c| {
                            let mut detail = c.data_type.clone();
                            if c.is_primary_key {
                                detail.push_str(" PK");
                            }
                            if !c.nullable {
                                detail.push_str(" NOT NULL");
                            }
                            node(child(&c.name), c.name.clone(), NodeKind::Column, false, Some(detail), None)
                        })
                        .collect(),
                    "indexes" => t
                        .indexes
                        .iter()
                        .map(|i| {
                            let detail = format!(
                                "{}{}({})",
                                if i.primary { "PRIMARY " } else if i.unique { "UNIQUE " } else { "" },
                                i.method.as_deref().map(|m| format!("{m} ")).unwrap_or_default(),
                                i.columns.join(", ")
                            );
                            node(child(&i.name), i.name.clone(), NodeKind::Index, false, Some(detail), None)
                        })
                        .collect(),
                    "foreign_keys" => t
                        .foreign_keys
                        .iter()
                        .map(|f| {
                            let detail = format!(
                                "({}) → {}.{}({})",
                                f.columns.join(", "),
                                f.ref_schema.as_deref().unwrap_or(""),
                                f.ref_table,
                                f.ref_columns.join(", ")
                            );
                            node(child(&f.name), f.name.clone(), NodeKind::ForeignKey, false, Some(detail), None)
                        })
                        .collect(),
                    _ => vec![],
                })
            }
            _ => Ok(vec![]),
        }
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        let rows = self
            .rows("SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 AND state = 0 ORDER BY CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, name", &[])
            .await?;
        Ok(rows.iter().map(|r| vs(&r[0])).collect())
    }

    async fn execute(&self, script: &str, opts: &ExecOptions) -> AppResult<Vec<QueryResult>> {
        let max_rows = opts.max_rows.unwrap_or(1000).max(1);
        let statements = split_statements(Dialect::Mssql, script);
        if self.read_only {
            for s in &statements {
                if !is_read_only_sql(s) {
                    return Err(AppError::msg("Connection is read-only: only SELECT / WITH / SET / PRINT / DECLARE / EXEC sp_help* statements are allowed"));
                }
            }
        }
        let mut c = self.pool.get().await?;
        let spid = c.spid();
        self.running.lock().insert(spid);
        let res: Result<Vec<QueryResult>, tiberius::error::Error> = async {
            let db = opts.database.as_deref().filter(|d| !d.is_empty()).or(self.default_db.as_deref());
            if let Some(db) = db {
                c.client().simple_query(format!("USE {}", q(db))).await?.into_results().await?;
            }
            let mut out = Vec::new();
            for batch in &statements {
                match split_batch(batch) {
                    Some(pieces) => {
                        for stmt in &pieces {
                            out.extend(Self::run_batch(c.client(), stmt, max_rows).await?);
                        }
                    }
                    None => out.extend(Self::run_batch(c.client(), batch, max_rows).await?),
                }
            }
            Ok(out)
        }
        .await;
        self.running.lock().remove(&spid);
        match res {
            Ok(r) => {
                c.release();
                Ok(r)
            }
            Err(e) => {
                if recoverable(&e) {
                    // Roll back anything the failed script left open.
                    let _ = c.client().simple_query("IF @@TRANCOUNT > 0 ROLLBACK").await.map(|_| ());
                    c.release();
                }
                Err(e.into())
            }
        }
    }

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage> {
        let started = Instant::now();
        let (db, schema, name) = self.table_parts(&req.table)?;
        let full = format!("{}.{}.{}", q(db), q(schema), q(name));
        let filter = req.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
        // T-SQL needs no `;` between statements, so check the filter as a whole batch.
        if let Some(f) = filter
            && (split_statements(Dialect::Mssql, f).len() > 1 || !is_read_only_sql(&format!("SELECT 1 WHERE {f}")))
        {
            return Err(AppError::msg("The filter must be a single read-only SQL expression"));
        }
        let where_sql = filter.map(|f| format!(" WHERE ({f})")).unwrap_or_default();
        let pk = self.primary_key(db, schema, name).await.unwrap_or_default();
        let order = if !req.sort.is_empty() {
            req.sort.iter().map(|s| format!("{} {}", q(&s.column), if s.desc { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", ")
        } else if !pk.is_empty() {
            pk.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
        } else {
            "(SELECT NULL)".into()
        };
        let limit = if req.limit == 0 { 1000 } else { req.limit };
        let sql = format!("SELECT * FROM {full}{where_sql} ORDER BY {order} OFFSET {} ROWS FETCH NEXT {limit} ROWS ONLY", req.offset);

        let mut c = self.pool.get().await?;
        let res = Self::run_batch(c.client(), &sql, limit as usize).await;
        let mut results = match res {
            Ok(r) => {
                c.release();
                r
            }
            Err(e) => {
                if recoverable(&e) {
                    c.release();
                }
                return Err(e.into());
            }
        };
        drop(c);
        let mut result = results.drain(..).next().unwrap_or_default();
        result.statement = sql;
        result.elapsed_ms = started.elapsed().as_millis() as u64;

        let total = if where_sql.is_empty() {
            let (s, t) = (schema.to_string(), name.to_string());
            let d = q(db);
            let fast = self
                .rows(
                    &format!(
                        "SELECT CAST(SUM(p.rows) AS bigint) FROM {d}.sys.partitions p
                         JOIN {d}.sys.tables t ON t.object_id = p.object_id
                         JOIN {d}.sys.schemas s ON s.schema_id = t.schema_id
                         WHERE p.index_id IN (0, 1) AND s.name = @P1 AND t.name = @P2"
                    ),
                    &[&s, &t],
                )
                .await
                .ok()
                .and_then(|r| r.first().and_then(|r| r[0].as_i64()));
            match fast {
                Some(n) => Some(n as u64),
                None => self.rows(&format!("SELECT COUNT_BIG(*) FROM {full}"), &[]).await.ok().and_then(|r| r.first().and_then(|r| r[0].as_u64())),
            }
        } else {
            self.rows(&format!("SELECT COUNT_BIG(*) FROM {full}{where_sql}"), &[])
                .await
                .ok()
                .and_then(|r| r.first().and_then(|r| r[0].as_u64()))
        };
        Ok(TableDataPage { result, total, primary_key: pk })
    }

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure> {
        let (db, schema, name) = self.table_parts(table)?;
        let st = self.load_structures(db, Some(schema), Some(name)).await?;
        if let Some(t) = st.tables.into_values().next() {
            return Ok(t);
        }
        // Views: columns only.
        let d = q(db);
        let (s, n) = (schema.to_string(), name.to_string());
        let rows = self
            .rows(
                &format!(
                    "SELECT c.name, ty.name, CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int),
                            c.is_nullable, c.column_id
                     FROM {d}.sys.columns c JOIN {d}.sys.objects o ON o.object_id = c.object_id
                     JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id
                     JOIN {d}.sys.types ty ON ty.user_type_id = c.user_type_id
                     WHERE s.name = @P1 AND o.name = @P2 ORDER BY c.column_id"
                ),
                &[&s, &n],
            )
            .await?;
        if rows.is_empty() {
            return Err(AppError::msg(format!("Object {schema}.{name} not found")));
        }
        Ok(TableStructure {
            table: Some(TableRef { database: Some(db.into()), schema: Some(schema.into()), name: name.into() }),
            columns: rows
                .iter()
                .map(|r| ColumnDef {
                    name: vs(&r[0]),
                    data_type: format_type(&vs(&r[1]), vi(&r[2]), vi(&r[3]), vi(&r[4])),
                    nullable: vb(&r[5]),
                    ordinal: vi(&r[6]) as u32,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64> {
        if self.read_only {
            return Err(AppError::msg("Connection is read-only"));
        }
        if changes.is_empty() {
            return Ok(0);
        }
        let (db, schema, name) = self.table_parts(table)?;
        let full = format!("{}.{}.{}", q(db), q(schema), q(name));
        let st = self.table_structure(table).await?;
        let col_type = |c: &str| st.columns.iter().find(|x| x.name == c).map(|x| x.data_type.clone());
        let is_identity = |c: &str| st.columns.iter().any(|x| x.name == c && x.auto_increment);
        let is_computed = |c: &str| st.columns.iter().any(|x| x.name == c && x.comment.as_deref().is_some_and(|m| m.starts_with("computed: ")));
        let where_of = |key: &Map<String, Value>| -> String {
            key.iter()
                .map(|(k, v)| {
                    if v.is_null() {
                        format!("{} IS NULL", q(k))
                    } else {
                        format!("{} = {}", q(k), value_sql(v, col_type(k).as_deref()))
                    }
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        };

        let mut stmts: Vec<(String, bool)> = Vec::new(); // (sql, must affect exactly one row)
        for ch in changes {
            match ch {
                RowChange::Insert { values } => {
                    let vals: Vec<(&String, &Value)> = values
                        .iter()
                        .filter(|(k, v)| !is_computed(k) && !(is_identity(k) && v.is_null()))
                        .collect();
                    let identity_insert = vals.iter().any(|(k, _)| is_identity(k));
                    let sql = if vals.is_empty() {
                        format!("INSERT INTO {full} DEFAULT VALUES")
                    } else {
                        format!(
                            "INSERT INTO {full} ({}) VALUES ({})",
                            vals.iter().map(|(k, _)| q(k)).collect::<Vec<_>>().join(", "),
                            vals.iter().map(|(k, v)| value_sql(v, col_type(k).as_deref())).collect::<Vec<_>>().join(", ")
                        )
                    };
                    if identity_insert {
                        stmts.push((format!("SET IDENTITY_INSERT {full} ON; {sql}; SET IDENTITY_INSERT {full} OFF"), false));
                    } else {
                        stmts.push((sql, false));
                    }
                }
                RowChange::Update { key, values } => {
                    if key.is_empty() {
                        return Err(AppError::msg("Cannot update a row without a key"));
                    }
                    let sets: Vec<String> = values
                        .iter()
                        .filter(|(k, _)| !is_computed(k))
                        .map(|(k, v)| format!("{} = {}", q(k), value_sql(v, col_type(k).as_deref())))
                        .collect();
                    if sets.is_empty() {
                        continue;
                    }
                    stmts.push((format!("UPDATE {full} SET {} WHERE {}", sets.join(", "), where_of(key)), true));
                }
                RowChange::Delete { key } => {
                    if key.is_empty() {
                        return Err(AppError::msg("Cannot delete a row without a key"));
                    }
                    stmts.push((format!("DELETE FROM {full} WHERE {}", where_of(key)), true));
                }
            }
        }

        let mut c = self.pool.get().await?;
        let res: AppResult<u64> = async {
            c.client().simple_query("SET XACT_ABORT ON; BEGIN TRANSACTION").await?.into_results().await?;
            let mut total = 0u64;
            for (sql, exact) in &stmts {
                let r = c
                    .client()
                    .simple_query(format!("{sql}; SELECT CAST(@@ROWCOUNT AS bigint)"))
                    .await?
                    .into_results()
                    .await?;
                let n = r.last().and_then(|rows| rows.first()).and_then(|row| row.get::<i64, _>(0)).unwrap_or(0) as u64;
                if *exact && n != 1 {
                    return Err(AppError::msg(format!(
                        "Expected to change exactly 1 row but {n} matched; nothing was saved.\n{sql}"
                    )));
                }
                total += n;
            }
            c.client().simple_query("COMMIT TRANSACTION").await?.into_results().await?;
            Ok(total)
        }
        .await;
        match res {
            Ok(n) => {
                c.release();
                Ok(n)
            }
            Err(e) => {
                let rolled_back = match c.client().simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await {
                    Ok(s) => s.into_results().await.is_ok(),
                    Err(_) => false,
                };
                if rolled_back {
                    c.release();
                }
                Err(e)
            }
        }
    }

    async fn object_ddl(&self, table: &TableRef, kind: &NodeKind) -> AppResult<String> {
        let (db, schema, name) = self.table_parts(table)?;
        let d = q(db);
        let (s, n) = (schema.to_string(), name.to_string());
        match kind {
            NodeKind::Table => self.table_ddl(db, schema, name).await,
            NodeKind::Sequence => {
                let rows = self
                    .rows(
                        &format!(
                            "SELECT TYPE_NAME(q.user_type_id), CAST(q.start_value AS nvarchar(60)), CAST(q.increment AS nvarchar(60)),
                                    CAST(q.minimum_value AS nvarchar(60)), CAST(q.maximum_value AS nvarchar(60)),
                                    q.is_cycling, q.is_cached, q.cache_size
                             FROM {d}.sys.sequences q JOIN {d}.sys.schemas s ON s.schema_id = q.schema_id
                             WHERE s.name = @P1 AND q.name = @P2"
                        ),
                        &[&s, &n],
                    )
                    .await?;
                let r = rows.first().ok_or_else(|| AppError::msg(format!("Sequence {schema}.{name} not found")))?;
                Ok(format!(
                    "CREATE SEQUENCE {}.{} AS {}\n    START WITH {}\n    INCREMENT BY {}\n    MINVALUE {}\n    MAXVALUE {}\n    {}\n    {};\n",
                    q(schema),
                    q(name),
                    vs(&r[0]),
                    vs(&r[1]),
                    vs(&r[2]),
                    vs(&r[3]),
                    vs(&r[4]),
                    if vb(&r[5]) { "CYCLE" } else { "NO CYCLE" },
                    if !vb(&r[6]) {
                        "NO CACHE".to_string()
                    } else if r[7].is_null() {
                        "CACHE".to_string()
                    } else {
                        format!("CACHE {}", vs(&r[7]))
                    }
                ))
            }
            _ => {
                // Views, procedures, functions and triggers (a trigger's schema is its table's).
                let rows = self
                    .rows(
                        &format!(
                            "SELECT m.definition FROM {d}.sys.sql_modules m
                             JOIN {d}.sys.objects o ON o.object_id = m.object_id
                             LEFT JOIN {d}.sys.objects p ON p.object_id = o.parent_object_id
                             JOIN {d}.sys.schemas s ON s.schema_id = COALESCE(p.schema_id, o.schema_id)
                             WHERE o.name = @P2 AND (s.name = @P1 OR o.schema_id = SCHEMA_ID(@P1))"
                        ),
                        &[&s, &n],
                    )
                    .await?;
                match rows.first() {
                    Some(r) if !r[0].is_null() => Ok(vs(&r[0]).trim().to_string() + "\n"),
                    Some(_) => Ok(format!("-- The definition of {schema}.{name} is encrypted\n")),
                    None => Err(AppError::msg(format!("Object {schema}.{name} not found"))),
                }
            }
        }
    }

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot> {
        let db = self.db_of(database)?;
        let st = self.load_structures(db, schema, None).await?;
        let d = q(db);
        let filter = schema.map(|s| format!(" AND s.name = {}", lit(s))).unwrap_or_default();
        let views = self
            .rows(
                &format!(
                    "SELECT s.name, v.name, m.definition FROM {d}.sys.views v
                     JOIN {d}.sys.schemas s ON s.schema_id = v.schema_id
                     LEFT JOIN {d}.sys.sql_modules m ON m.object_id = v.object_id
                     WHERE v.is_ms_shipped = 0{filter} ORDER BY s.name, v.name"
                ),
                &[],
            )
            .await?;
        Ok(SchemaSnapshot {
            tables: st.tables.into_values().collect(),
            views: views
                .iter()
                .map(|r| (TableRef { database: Some(db.into()), schema: Some(vs(&r[0])), name: vs(&r[1]) }, vs(&r[2])))
                .collect(),
        })
    }

    /// Kills the sessions running `execute` (needs ALTER ANY CONNECTION).
    async fn cancel(&self) -> AppResult<()> {
        let spids: Vec<i32> = self.running.lock().iter().copied().collect();
        if spids.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.open().await?;
        for spid in spids.into_iter().filter(|s| *s > 0) {
            conn.client.simple_query(format!("KILL {spid}")).await?.into_results().await?;
        }
        let _ = conn.client.close().await;
        Ok(())
    }

    async fn close(&self) {
        let conns: Vec<Conn> = std::mem::take(&mut *self.pool.idle.lock());
        for c in conns {
            let _ = c.client.close().await;
        }
    }
}

/// Split a batch on top-level `;` when that cannot change its meaning, so every
/// statement gets its own result (tiberius doesn't expose per-statement DONE
/// counts). Batches using variables, control flow or module definitions are
/// batch-scoped and returned as `None` (run whole).
fn split_batch(sql: &str) -> Option<Vec<String>> {
    let c: Vec<char> = sql.chars().collect();
    let mut pieces = Vec::new();
    let mut cur = String::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut i = 0;
    let flush_word = |word: &mut String, words: &mut Vec<String>| {
        if !word.is_empty() {
            words.push(word.to_uppercase());
            word.clear();
        }
    };
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\'' | '"' | '[' => {
                flush_word(&mut word, &mut words);
                let close = if ch == '[' { ']' } else { ch };
                cur.push(ch);
                i += 1;
                while i < c.len() {
                    cur.push(c[i]);
                    if c[i] == close {
                        if c.get(i + 1) == Some(&close) {
                            cur.push(close);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            '-' if c.get(i + 1) == Some(&'-') => {
                flush_word(&mut word, &mut words);
                while i < c.len() && c[i] != '\n' {
                    cur.push(c[i]);
                    i += 1;
                }
                continue;
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                flush_word(&mut word, &mut words);
                while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                    cur.push(c[i]);
                    i += 1;
                }
                cur.push_str("*/");
                i += 2;
                continue;
            }
            '@' if c.get(i + 1) != Some(&'@') && !word.ends_with('@') => return None,
            ';' => {
                flush_word(&mut word, &mut words);
                if !cur.trim().is_empty() {
                    pieces.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => {
                if ch.is_alphanumeric() || ch == '_' || ch == '@' || ch == '#' {
                    word.push(ch);
                } else {
                    flush_word(&mut word, &mut words);
                }
                cur.push(ch);
            }
        }
        i += 1;
    }
    flush_word(&mut word, &mut words);
    if !cur.trim().is_empty() {
        pieces.push(cur.trim().to_string());
    }
    // Drop comment-only pieces.
    pieces.retain(|p| !strip_comments(p).trim().is_empty());
    if pieces.len() < 2 {
        return None;
    }
    const UNSAFE: &[&str] = &[
        "DECLARE", "IF", "ELSE", "WHILE", "GOTO", "RETURN", "BREAK", "CONTINUE", "TRY", "CATCH", "PROCEDURE", "PROC",
        "FUNCTION", "TRIGGER", "VIEW", "WAITFOR", "SCHEMA",
    ];
    for (k, w) in words.iter().enumerate() {
        if UNSAFE.contains(&w.as_str()) {
            return None;
        }
        if w == "BEGIN" && !matches!(words.get(k + 1).map(String::as_str), Some("TRAN" | "TRANSACTION" | "DISTRIBUTED")) {
            return None;
        }
    }
    Some(pieces)
}

/// Conservative read-only check for a statement/batch.
fn is_read_only_sql(sql: &str) -> bool {
    let cleaned = strip_comments(sql).to_uppercase();
    let first = cleaned.split_whitespace().next().unwrap_or("");
    if !matches!(first, "SELECT" | "WITH" | "SET" | "PRINT" | "DECLARE" | "USE" | "EXEC" | "EXECUTE" | "SP_HELP" | "SP_HELPTEXT" | "DBCC") && !first.is_empty() {
        return false;
    }
    let words: Vec<&str> = cleaned.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).collect();
    if (first == "EXEC" || first == "EXECUTE") && !words.get(1).is_some_and(|w| w.starts_with("SP_HELP")) {
        return false;
    }
    if first == "DBCC" && !words.get(1).is_some_and(|w| matches!(*w, "SHOW_STATISTICS" | "USEROPTIONS" | "SQLPERF")) {
        return false;
    }
    // dynamic SQL later in the batch (`SELECT 1 EXEC('DELETE …')`) hides its text in a string
    if words.iter().skip(1).any(|w| matches!(*w, "EXEC" | "EXECUTE" | "SP_EXECUTESQL" | "OPENROWSET" | "OPENQUERY" | "OPENDATASOURCE")) {
        return false;
    }
    const WRITES: &[&str] = &[
        "INSERT", "UPDATE", "DELETE", "MERGE", "DROP", "ALTER", "CREATE", "TRUNCATE", "GRANT", "REVOKE", "DENY", "INTO",
        "BACKUP", "RESTORE", "KILL", "SHUTDOWN", "BULK", "RECONFIGURE",
    ];
    !words.iter().any(|w| WRITES.contains(w))
}

fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let c: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '-' && c.get(i + 1) == Some(&'-') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c[i] == '\'' {
            // Drop string literal contents so keywords inside strings don't count.
            i += 1;
            while i < c.len() {
                if c[i] == '\'' {
                    if c.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push_str("''");
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_default_parens() {
        assert_eq!(strip_parens("((0))"), "0");
        assert_eq!(strip_parens("(getdate())"), "getdate()");
        assert_eq!(strip_parens("(N'a')"), "N'a'");
        assert_eq!(strip_parens("((1)+(2))"), "(1)+(2)");
    }

    #[test]
    fn read_only_detection() {
        assert!(is_read_only_sql("SELECT * FROM t WHERE a = 'DELETE'"));
        assert!(is_read_only_sql("-- drop\nWITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(!is_read_only_sql("SELECT * INTO t2 FROM t"));
        assert!(!is_read_only_sql("UPDATE t SET a = 1"));
        assert!(!is_read_only_sql("EXEC dbo.do_stuff"));
        assert!(is_read_only_sql("EXEC sp_helptext 'x'"));
        assert!(!is_read_only_sql("SELECT 1 EXEC('DELETE FROM t')"));
        assert!(!is_read_only_sql("DECLARE @s nvarchar(99) = N'DROP TABLE t'; EXEC sp_executesql @s"));
        assert!(!is_read_only_sql("SELECT * FROM OPENQUERY(srv, 'DELETE FROM t')"));
    }

    #[test]
    fn splits_simple_batches_only() {
        assert_eq!(
            split_batch("INSERT INTO t VALUES ('a;b'); -- x;y\nSELECT @@ROWCOUNT; BEGIN TRAN; UPDATE [we;ird] SET a = 1").unwrap().len(),
            4
        );
        assert!(split_batch("SELECT 1").is_none());
        assert!(split_batch("DECLARE @x int = 1; SELECT @x").is_none());
        assert!(split_batch("IF 1 = 1 SELECT 1; SELECT 2").is_none());
        assert!(split_batch("BEGIN TRY SELECT 1; END TRY BEGIN CATCH SELECT 2; END CATCH").is_none());
        assert_eq!(split_batch("SELECT CASE WHEN a = 1 THEN 2 END FROM t; SELECT 3").unwrap().len(), 2);
    }

    #[test]
    fn formats_types() {
        assert_eq!(format_type("nvarchar", 100, 0, 0), "nvarchar(50)");
        assert_eq!(format_type("varbinary", -1, 0, 0), "varbinary(max)");
        assert_eq!(format_type("decimal", 9, 18, 2), "decimal(18,2)");
        assert_eq!(fmt_frac(123_456_789, 7), ".1234567");
        assert_eq!(fmt_frac(3_000_000, 3), ".003");
        assert_eq!(fmt_frac(0, 7), "");
    }
}

/// Integration tests against the docker-compose SQL Server (`cargo test -- --ignored mssql_it`).
#[cfg(test)]
mod mssql_it {
    use super::*;

    fn cfg(db: Option<&str>) -> ConnectionConfig {
        serde_json::from_value(json!({
            "id": "t", "name": "t", "kind": "mssql", "host": "127.0.0.1", "port": 51433,
            "user": "sa", "password": "DevCat_Passw0rd!", "database": db
        }))
        .unwrap()
    }

    fn opts(db: &str) -> ExecOptions {
        ExecOptions { database: Some(db.into()), schema: None, max_rows: Some(100) }
    }

    async fn exec1(d: &MssqlDriver, db: &str, sql: &str) -> Vec<QueryResult> {
        d.execute(sql, &opts(db)).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    #[tokio::test]
    #[ignore]
    async fn mssql_it_roundtrip() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let master = MssqlDriver::connect(&cfg(None)).await.unwrap();
        assert!(master.server_version().await.unwrap().contains("SQL Server"));
        exec1(&master, "master", "IF DB_ID('devcat_it') IS NOT NULL BEGIN ALTER DATABASE devcat_it SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE devcat_it; END").await;
        exec1(&master, "master", "CREATE DATABASE devcat_it").await;
        let d = MssqlDriver::connect(&cfg(Some("devcat_it"))).await.unwrap();
        for sql in [
            "CREATE SCHEMA app",
            "CREATE TABLE app.owners (id int IDENTITY(1,1) CONSTRAINT pk_owners PRIMARY KEY, name nvarchar(50) NOT NULL CONSTRAINT uq_owner_name UNIQUE)",
            "CREATE TABLE app.items (
                id bigint IDENTITY(10,5) NOT NULL,
                owner_id int NULL,
                code varchar(20) NOT NULL CONSTRAINT df_code DEFAULT ('X'),
                title nvarchar(max) NULL,
                flag bit NOT NULL DEFAULT 0,
                qty tinyint NULL, small smallint NULL, price decimal(10,2) NULL, cost money NULL,
                ratio float NULL, r real NULL, uid uniqueidentifier NULL DEFAULT NEWID(),
                d date NULL, t time(3) NULL, dt datetime NULL, sdt smalldatetime NULL, dt2 datetime2(7) NULL,
                dto datetimeoffset NULL, bin varbinary(16) NULL, x xml NULL, v sql_variant NULL,
                total AS (qty * price) PERSISTED,
                CONSTRAINT pk_items PRIMARY KEY (id),
                CONSTRAINT fk_items_owner FOREIGN KEY (owner_id) REFERENCES app.owners(id) ON DELETE CASCADE,
                CONSTRAINT ck_qty CHECK (qty >= 0)
            )",
            "CREATE INDEX ix_items_code ON app.items (code DESC) INCLUDE (title) WHERE code IS NOT NULL",
            "EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'Item table', @level0type=N'SCHEMA', @level0name=N'app', @level1type=N'TABLE', @level1name=N'items'",
            "CREATE VIEW app.v_items AS SELECT id, code FROM app.items",
            "CREATE PROCEDURE app.p_items AS SELECT 1 AS a; SELECT 2 AS b, 3 AS c",
            "CREATE FUNCTION app.f_double(@x int) RETURNS int AS BEGIN RETURN @x * 2 END",
            "CREATE TRIGGER app.tr_items ON app.items AFTER INSERT AS BEGIN SET NOCOUNT ON; END",
            "CREATE SEQUENCE app.seq_a AS int START WITH 5 INCREMENT BY 2",
        ] {
            exec1(&d, "devcat_it", sql).await;
        }

        // execute: inserts, multiple result sets, rows affected
        let r = exec1(
            &d,
            "devcat_it",
            "INSERT INTO app.owners (name) VALUES (N'ann'), (N'bob');
             INSERT INTO app.items (owner_id, code, title, qty, small, price, cost, ratio, r, uid, d, t, dt, sdt, dt2, dto, bin, x, v)
             VALUES (1, 'A1', N'héllo', 3, -2, 12.34, 5.5, 1.25, 0.5, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '2024-01-02',
                     '10:11:12.123', '2024-01-02T03:04:05.123', '2024-01-02 03:04', '2024-01-02T03:04:05.1234567',
                     '2024-01-02T03:04:05.5+02:00', 0xDEADBEEF, '<a>1</a>', CAST(42 AS int));
             EXEC app.p_items;
             UPDATE app.items SET qty = qty WHERE 1 = 0",
        )
        .await;
        assert_eq!(r[0].rows_affected, Some(2));
        assert_eq!(r[1].rows_affected, Some(1));
        assert_eq!(r[2].columns[0].name, "a");
        assert_eq!(r[3].columns.len(), 2);
        assert_eq!(r[4].rows_affected, Some(0));

        let r = exec1(&d, "devcat_it", "SELECT * FROM app.items").await;
        let row = &r[0].rows[0];
        let col = |n: &str| r[0].columns.iter().position(|c| c.name == n).unwrap();
        assert_eq!(row[col("id")], json!(10));
        assert_eq!(row[col("title")], json!("héllo"));
        assert_eq!(row[col("flag")], json!(false));
        assert_eq!(row[col("price")], json!("12.34"));
        assert_eq!(row[col("cost")], json!("5.5000"));
        assert_eq!(row[col("ratio")], json!(1.25));
        assert_eq!(row[col("uid")], json!("A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11"));
        assert_eq!(row[col("d")], json!("2024-01-02"));
        assert_eq!(row[col("t")], json!("10:11:12.123"));
        assert_eq!(row[col("dt")], json!("2024-01-02T03:04:05.123"));
        assert_eq!(row[col("sdt")], json!("2024-01-02T03:04:00"));
        assert_eq!(row[col("dt2")], json!("2024-01-02T03:04:05.1234567"));
        assert_eq!(row[col("dto")], json!("2024-01-02T03:04:05.5+02:00"));
        assert_eq!(row[col("bin")], json!("\\xdeadbeef"));
        assert_eq!(row[col("x")], json!("<a>1</a>"));
        assert!(!row[col("v")].is_null(), "sql_variant: {}", row[col("v")]);
        assert_eq!(row[col("total")], json!("37.02"));

        // max_rows truncation + continuation
        let r = d
            .execute("SELECT TOP 500 a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b; SELECT 1 AS one", &ExecOptions { database: None, schema: None, max_rows: Some(10) })
            .await
            .unwrap();
        assert!(r[0].truncated);
        assert_eq!(r[0].rows.len(), 10);
        assert_eq!(r[1].rows[0][0], json!(1));

        // errors keep the connection usable
        assert!(d.execute("SELECT * FROM nope", &opts("devcat_it")).await.is_err());
        assert_eq!(exec1(&d, "devcat_it", "SELECT DB_NAME()").await[0].rows[0][0], json!("devcat_it"));

        // tree
        let dbs = master.children(&[]).await.unwrap();
        assert!(dbs.iter().any(|n| n.label == "devcat_it"));
        let schemas = d.children(&["devcat_it".into()]).await.unwrap();
        assert!(schemas.iter().any(|n| n.label == "app") && schemas.iter().any(|n| n.label == "dbo"));
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(d.children(&p(&["devcat_it", "app"])).await.unwrap().len(), 6);
        let tables = d.children(&p(&["devcat_it", "app", "tables"])).await.unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].detail.as_deref(), Some("1 rows"));
        for (folder, name) in [("views", "v_items"), ("procedures", "p_items"), ("functions", "f_double"), ("triggers", "tr_items"), ("sequences", "seq_a")] {
            let n = d.children(&p(&["devcat_it", "app", folder])).await.unwrap();
            assert_eq!(n.len(), 1, "{folder}");
            assert_eq!(n[0].label, name);
            let ddl = d.object_ddl(n[0].table.as_ref().unwrap(), &n[0].kind).await.unwrap();
            assert!(ddl.to_uppercase().contains("CREATE"), "{folder}: {ddl}");
        }
        let cols = d.children(&p(&["devcat_it", "app", "tables", "items", "columns"])).await.unwrap();
        assert_eq!(cols.len(), 22);
        let fks = d.children(&p(&["devcat_it", "app", "tables", "items", "foreign_keys"])).await.unwrap();
        assert_eq!(fks[0].label, "fk_items_owner");

        // structure
        let items = TableRef { database: Some("devcat_it".into()), schema: Some("app".into()), name: "items".into() };
        let st = d.table_structure(&items).await.unwrap();
        assert_eq!(st.comment.as_deref(), Some("Item table"));
        let id = &st.columns[0];
        assert!(id.is_primary_key && id.auto_increment && !id.nullable);
        let code = st.columns.iter().find(|c| c.name == "code").unwrap();
        assert_eq!(code.data_type, "varchar(20)");
        assert_eq!(code.default.as_deref(), Some("'X'"));
        assert_eq!(st.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
        assert!(st.indexes.iter().any(|i| i.name == "ix_items_code"));

        // DDL recreates the table
        let ddl = d.object_ddl(&items, &NodeKind::Table).await.unwrap();
        assert!(ddl.contains("IDENTITY(10,5)") && ddl.contains("ON DELETE CASCADE") && ddl.contains("INCLUDE ([title])"), "{ddl}");
        let ddl2 = ddl.replace("[items]", "[items_copy]").replace("N'items'", "N'items_copy'").replace("CONSTRAINT [", "CONSTRAINT [c2_").replace("INDEX [", "INDEX [c2_");
        exec1(&d, "devcat_it", &ddl2).await;

        // table_data with paging, filter, sort
        let page = d
            .table_data(&TableDataRequest { table: items.clone(), offset: 0, limit: 10, filter: Some("code = 'A1'".into()), sort: vec![] })
            .await
            .unwrap();
        assert_eq!(page.total, Some(1));
        assert_eq!(page.primary_key, vec!["id".to_string()]);
        // a filter can't smuggle in another statement
        let all = TableDataRequest { table: items.clone(), offset: 0, limit: 10, filter: None, sort: vec![] };
        let before = d.table_data(&all).await.unwrap().result.rows.len();
        assert!(before > 0);
        for f in ["1=0) DELETE FROM [app].[items] --", "1=0; DELETE FROM [app].[items]", "1=1 EXEC('DELETE FROM [app].[items]')"] {
            let req = TableDataRequest { table: items.clone(), offset: 0, limit: 10, filter: Some(f.into()), sort: vec![] };
            assert!(d.table_data(&req).await.is_err(), "{f}");
        }
        assert_eq!(d.table_data(&all).await.unwrap().result.rows.len(), before);

        // apply_changes: insert (identity skipped), update, delete in one transaction
        let mut ins = Map::new();
        ins.insert("id".into(), Value::Null);
        ins.insert("code".into(), json!("B2"));
        ins.insert("bin".into(), json!("\\x0102"));
        ins.insert("flag".into(), json!(true));
        ins.insert("total".into(), Value::Null);
        let mut key = Map::new();
        key.insert("id".into(), json!(10));
        let mut upd = Map::new();
        upd.insert("title".into(), json!("it's"));
        upd.insert("dt".into(), json!("2024-01-02T03:04:05.123"));
        let n = d
            .apply_changes(&items, &[RowChange::Insert { values: ins }, RowChange::Update { key: key.clone(), values: upd }])
            .await
            .unwrap();
        assert_eq!(n, 2);
        let r = exec1(&d, "devcat_it", "SELECT id, title, bin FROM app.items ORDER BY id").await;
        assert_eq!(r[0].rows[0][1], json!("it's"));
        assert_eq!(r[0].rows[1][0], json!(15));
        assert_eq!(r[0].rows[1][2], json!("\\x0102"));
        // failing change rolls back the whole batch
        let mut bad = Map::new();
        bad.insert("id".into(), json!(9999));
        let mut del = Map::new();
        del.insert("id".into(), json!(15));
        assert!(d.apply_changes(&items, &[RowChange::Delete { key: del }, RowChange::Delete { key: bad }]).await.is_err());
        assert_eq!(exec1(&d, "devcat_it", "SELECT COUNT(*) FROM app.items").await[0].rows[0][0], json!(2));

        // snapshot
        let snap = d.schema_snapshot(Some("devcat_it"), Some("app")).await.unwrap();
        assert_eq!(snap.tables.len(), 3);
        assert_eq!(snap.views.len(), 1);

        // read-only
        let mut ro = cfg(Some("devcat_it"));
        ro.read_only = true;
        let r = MssqlDriver::connect(&ro).await.unwrap();
        assert!(r.execute("DELETE FROM app.items", &opts("devcat_it")).await.is_err());
        assert!(r.execute("SELECT 1", &opts("devcat_it")).await.is_ok());

        // cancel a long-running query
        let d = std::sync::Arc::new(d);
        let d2 = d.clone();
        let h = tokio::spawn(async move { d2.execute("WAITFOR DELAY '00:00:30'", &opts("devcat_it")).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        d.cancel().await.unwrap();
        let res = tokio::time::timeout(Duration::from_secs(10), h).await.expect("cancel took too long").unwrap();
        assert!(res.is_err());
        // pool still works afterwards
        assert_eq!(exec1(&d, "devcat_it", "SELECT 1").await[0].rows[0][0], json!(1));

        d.close().await;
        drop(d);
        exec1(&master, "master", "ALTER DATABASE devcat_it SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE devcat_it").await;
    }

    #[tokio::test]
    #[ignore]
    async fn mssql_it_url() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut c = cfg(None);
        c.host = String::new();
        c.url = Some("server=tcp:127.0.0.1,51433;user=sa;password=DevCat_Passw0rd!;TrustServerCertificate=true".into());
        let d = MssqlDriver::connect(&c).await.unwrap();
        assert!(d.list_databases().await.unwrap().contains(&"master".to_string()));
    }
}
