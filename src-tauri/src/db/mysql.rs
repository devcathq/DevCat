//! MySQL / MariaDB driver built on sqlx.
//!
//! User SQL, grid data and catalog queries use the text protocol (`raw_sql`),
//! so every value arrives in its textual form and is converted to JSON by
//! column type in [`cell_to_json`]. Grid edits use bound parameters.
//! Query-editor executions run on a sticky session connection so `USE`,
//! session variables, temp tables and explicit transactions carry over.
use super::sql_common::{self as sc};
use super::ssh::{open_endpoint, SshTunnel};
use super::*;
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use async_trait::async_trait;
use futures::TryStreamExt;
use serde_json::Value;
use sqlx::mysql::{MySql, MySqlConnectOptions, MySqlConnection, MySqlDatabaseError, MySqlPool, MySqlPoolOptions, MySqlRow, MySqlSslMode, MySqlTypeInfo};
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Column, Either, Executor, Row, SqlSafeStr, Statement, Type, TypeInfo, ValueRef};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};

const D: Dialect = Dialect::Mysql;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_ROWS: usize = 1000;
const JS_MAX_SAFE_INT: u64 = 9_007_199_254_740_991;
const SYSTEM_DBS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

struct Session {
    conn: PoolConnection<MySql>,
    id: u64,
    /// Current default database, `None` if unknown (after a user `USE`).
    db: Option<String>,
}

impl Session {
    async fn open(pool: &MySqlPool) -> Result<Self, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        let row = sqlx::raw_sql("SELECT CONNECTION_ID(), DATABASE()").fetch_one(&mut *conn).await?;
        let id = text_at(&row, 0).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok(Session { conn, id, db: text_at(&row, 1) })
    }
}

enum Lease {
    Sticky(OwnedMutexGuard<Option<Session>>),
    Temp(Session),
}

impl Lease {
    fn session(&mut self) -> &mut Session {
        match self {
            Lease::Sticky(g) => g.as_mut().expect("session initialised"),
            Lease::Temp(s) => s,
        }
    }
    fn discard(&mut self) {
        match self {
            Lease::Sticky(g) => {
                if let Some(mut s) = g.take() {
                    s.conn.close_on_drop();
                }
            }
            Lease::Temp(s) => s.conn.close_on_drop(),
        }
    }
}

struct RunningGuard<'a> {
    map: &'a parking_lot::Mutex<HashMap<u64, u64>>,
    id: u64,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().remove(&self.id);
    }
}

pub struct MysqlDriver {
    pool: MySqlPool,
    default_db: Option<String>,
    read_only: bool,
    mariadb: bool,
    session: Arc<Mutex<Option<Session>>>,
    /// run id -> server connection id of in-flight executions.
    running: parking_lot::Mutex<HashMap<u64, u64>>,
    next_run: AtomicU64,
    cancel_gen: AtomicU64,
    _tunnel: Option<SshTunnel>,
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn ssl_mode(s: &str) -> Option<MySqlSslMode> {
    Some(match s.to_ascii_lowercase().as_str() {
        "disable" | "disabled" => MySqlSslMode::Disabled,
        "prefer" | "preferred" | "allow" => MySqlSslMode::Preferred,
        "require" | "required" => MySqlSslMode::Required,
        "verify-ca" | "verify_ca" => MySqlSslMode::VerifyCa,
        "verify-full" | "verify_full" | "verify-identity" | "verify_identity" => MySqlSslMode::VerifyIdentity,
        _ => return None,
    })
}

/// Column `i` of a text-protocol row as a (lossy UTF-8) string.
fn text_at(row: &MySqlRow, i: usize) -> Option<String> {
    let v = row.try_get_raw(i).ok()?;
    if v.is_null() {
        return None;
    }
    let b: &[u8] = row.try_get_unchecked(i).ok()?;
    Some(String::from_utf8_lossy(b).into_owned())
}

fn lit(s: &str) -> String {
    sc::string_literal(D, s)
}

fn q(s: &str) -> String {
    sc::quote_ident(D, s)
}

impl MysqlDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> AppResult<Self> {
        let url = non_empty(&cfg.url);
        let mut opts = match url {
            Some(u) => MySqlConnectOptions::from_str(u)?,
            None => MySqlConnectOptions::new().username(if cfg.user.trim().is_empty() { "root" } else { cfg.user.trim() }),
        };
        if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
            opts = opts.password(pw);
        }
        let mut ecfg = cfg.clone();
        if url.is_some() {
            ecfg.host = opts.get_host().to_string();
            ecfg.port = Some(opts.get_port());
        }
        let ep = open_endpoint(&ecfg, 3306).await?;
        // Don't alter the session the way sqlx does by default (UTC time zone,
        // PIPES_AS_CONCAT): users must see what their server really does.
        opts = opts
            .host(&ep.host)
            .port(ep.port)
            .charset("utf8mb4")
            .timezone(None)
            .pipes_as_concat(false)
            .no_engine_substitution(false)
            .statement_cache_capacity(32);
        if let Some(db) = non_empty(&cfg.database) {
            opts = opts.database(db);
        }
        if let Some(mode) = non_empty(&cfg.ssl_mode) {
            opts = opts.ssl_mode(ssl_mode(mode).ok_or_else(|| AppError::msg(format!("Unknown SSL mode \"{mode}\"")))?);
        }
        let default_db = opts.get_database().map(str::to_string);
        let read_only = cfg.read_only;
        let fut = MySqlPoolOptions::new()
            .max_connections(6)
            .min_connections(0)
            .acquire_timeout(CONNECT_TIMEOUT)
            .idle_timeout(Duration::from_secs(600))
            .after_connect(move |conn: &mut MySqlConnection, _| {
                Box::pin(async move {
                    if read_only {
                        conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                    }
                    Ok(())
                })
            })
            .connect_with(opts);
        let pool = tokio::time::timeout(CONNECT_TIMEOUT + Duration::from_secs(1), fut)
            .await
            .map_err(|_| AppError::msg("Timed out connecting to the MySQL server"))??;
        let version = sqlx::raw_sql("SELECT VERSION()").fetch_one(&pool).await?;
        let mariadb = text_at(&version, 0).unwrap_or_default().to_ascii_lowercase().contains("mariadb");
        Ok(MysqlDriver {
            pool,
            default_db,
            read_only,
            mariadb,
            session: Arc::new(Mutex::new(None)),
            running: parking_lot::Mutex::new(HashMap::new()),
            next_run: AtomicU64::new(1),
            cancel_gen: AtomicU64::new(0),
            _tunnel: ep.tunnel,
        })
    }

    fn db_of(&self, db: Option<&str>) -> AppResult<String> {
        db.map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .or_else(|| self.default_db.clone())
            .ok_or_else(|| AppError::msg("No database selected"))
    }

    /// Run a catalog query and return all cells as strings.
    async fn rows(&self, sql: String) -> AppResult<Vec<Vec<Option<String>>>> {
        let rows = sqlx::raw_sql(AssertSqlSafe(sql)).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| (0..r.len()).map(|i| text_at(r, i)).collect()).collect())
    }

    async fn strings(&self, sql: String) -> AppResult<Vec<String>> {
        Ok(self.rows(sql).await?.into_iter().filter_map(|mut r| r.swap_remove(0)).collect())
    }

    async fn lease(&self) -> AppResult<Lease> {
        match self.session.clone().try_lock_owned() {
            Ok(mut guard) => {
                if guard.is_none() {
                    *guard = Some(Session::open(&self.pool).await?);
                }
                Ok(Lease::Sticky(guard))
            }
            Err(_) => {
                let mut s = Session::open(&self.pool).await?;
                s.conn.close_on_drop();
                Ok(Lease::Temp(s))
            }
        }
    }

    /// Run one statement; a statement such as `CALL` may yield several result sets.
    ///
    /// `early` = (pool, connection id of `conn`): once more than `max_rows` rows
    /// arrived, a side-effect free query is stopped with `KILL QUERY` so the rest
    /// of a huge result isn't transferred. (A killed statement does not roll back
    /// an open transaction in MySQL.)
    async fn run_statement(conn: &mut MySqlConnection, sql: &str, max_rows: usize, early: Option<(&MySqlPool, u64)>) -> Result<Vec<QueryResult>, sqlx::Error> {
        let started = Instant::now();
        let mut out: Vec<QueryResult> = Vec::new();
        let mut columns: Option<Vec<(String, MySqlTypeInfo)>> = None;
        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut truncated = false;
        let mut affected = 0u64;
        let mut last_id = 0u64;
        let finish = |cols: Vec<(String, MySqlTypeInfo)>, rows: Vec<Vec<Value>>, truncated: bool| QueryResult {
            statement: sql.to_string(),
            columns: cols.iter().map(|(n, t)| ColumnInfo { name: n.clone(), type_name: display_type(t) }).collect(),
            rows,
            rows_affected: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
            truncated,
            message: None,
        };
        {
            let mut stream = sqlx::raw_sql(AssertSqlSafe(sql.to_string())).fetch_many(&mut *conn);
            let mut kinds: Vec<Kind> = Vec::new();
            let mut early = early.filter(|_| sc::can_stop_early(D, sql));
            let mut stopped = false;
            loop {
                let item = match stream.try_next().await {
                    Ok(Some(item)) => item,
                    Ok(None) => break,
                    // ER_QUERY_INTERRUPTED caused by our own KILL QUERY after truncation.
                    Err(e) if stopped && is_interrupted(&e) => break,
                    Err(e) => return Err(e),
                };
                match item {
                    Either::Left(done) => {
                        if let Some(cols) = columns.take() {
                            out.push(finish(cols, std::mem::take(&mut rows), truncated));
                            truncated = false;
                        } else {
                            affected += done.rows_affected();
                            if done.last_insert_id() > 0 {
                                last_id = done.last_insert_id();
                            }
                        }
                    }
                    Either::Right(row) => {
                        if columns.is_none() {
                            let cols: Vec<(String, MySqlTypeInfo)> =
                                row.columns().iter().map(|c| (c.name().to_string(), c.type_info().clone())).collect();
                            kinds = cols.iter().map(|(_, t)| Kind::of(t)).collect();
                            columns = Some(cols);
                        }
                        if rows.len() < max_rows {
                            rows.push(convert_row(&row, &kinds));
                        } else {
                            truncated = true;
                            if let Some((pool, id)) = early.take() {
                                stopped = sqlx::raw_sql(AssertSqlSafe(format!("KILL QUERY {id}"))).execute(pool).await.is_ok();
                            }
                        }
                    }
                }
            }
            if let Some(cols) = columns.take() {
                out.push(finish(cols, std::mem::take(&mut rows), truncated));
            }
        }
        if out.is_empty() && sc::may_return_rows(D, sql) && !sql.trim_start().to_ascii_uppercase().starts_with("CALL") {
            if let Ok(stmt) = (&mut *conn).prepare(AssertSqlSafe(sql.to_string()).into_sql_str()).await {
                if !stmt.columns().is_empty() {
                    let cols = stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().clone())).collect();
                    out.push(finish(cols, vec![], false));
                }
            }
        }
        if out.is_empty() {
            out.push(QueryResult {
                statement: sql.to_string(),
                rows_affected: Some(affected),
                elapsed_ms: started.elapsed().as_millis() as u64,
                message: (last_id > 0).then(|| format!("Last insert id: {last_id}")),
                ..Default::default()
            });
        }
        Ok(out)
    }

    async fn run_script(&self, lease: &mut Lease, stmts: &[sc::ScriptStatement], opts: &ExecOptions, cancel_gen: u64) -> Result<Vec<QueryResult>, (usize, sqlx::Error)> {
        let session = lease.session();
        let want = opts.database.as_deref().filter(|d| !d.is_empty()).or(self.default_db.as_deref());
        if let Some(db) = want {
            if session.db.as_deref() != Some(db) {
                sqlx::raw_sql(AssertSqlSafe(format!("USE {}", q(db)))).execute(&mut *session.conn).await.map_err(|e| (0, e))?;
                session.db = Some(db.to_string());
            }
        }
        let max_rows = opts.max_rows.filter(|m| *m > 0).unwrap_or(DEFAULT_MAX_ROWS);
        let mut out = Vec::with_capacity(stmts.len());
        for (i, st) in stmts.iter().enumerate() {
            if self.cancel_gen.load(Ordering::SeqCst) != cancel_gen {
                return Err((i, sqlx::Error::Protocol("Query execution was interrupted (cancelled)".into())));
            }
            if sc::first_keyword(D, &st.sql) == "USE" {
                session.db = None;
            }
            match Self::run_statement(&mut session.conn, &st.sql, max_rows, Some((&self.pool, session.id))).await {
                Ok(r) => out.extend(r),
                Err(e) => return Err((i, e)),
            }
        }
        Ok(out)
    }

    async fn primary_key(&self, db: &str, table: &str) -> AppResult<Vec<String>> {
        self.strings(format!(
            "SELECT COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX",
            lit(db),
            lit(table)
        ))
        .await
    }

    /// Bulk structure query for one table or every table of a database.
    async fn structures(&self, db: &str, table: Option<&str>) -> AppResult<Vec<TableStructure>> {
        let tfilter = |col: &str| table.map(|t| format!(" AND {col} = {}", lit(t))).unwrap_or_default();
        let tables = self
            .rows(format!(
                "SELECT TABLE_NAME, TABLE_COMMENT, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {}{} {} ORDER BY TABLE_NAME",
                lit(db),
                tfilter("TABLE_NAME"),
                if table.is_some() { "" } else { "AND TABLE_TYPE <> 'VIEW'" }
            ))
            .await?;
        let mut out: Vec<TableStructure> = Vec::with_capacity(tables.len());
        let mut index: HashMap<String, usize> = HashMap::new();
        for r in tables {
            let name = r[0].clone().unwrap_or_default();
            index.insert(name.clone(), out.len());
            let comment = r[1].clone().filter(|c| !c.is_empty() && r[2].as_deref() != Some("VIEW"));
            out.push(TableStructure {
                table: Some(TableRef { database: Some(db.to_string()), schema: None, name }),
                comment,
                ..Default::default()
            });
        }
        if out.is_empty() {
            return Ok(out);
        }
        let cols = self
            .rows(format!(
                "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, EXTRA, COLUMN_COMMENT, \
                        GENERATION_EXPRESSION, DATA_TYPE \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {}{} ORDER BY TABLE_NAME, ORDINAL_POSITION",
                lit(db),
                tfilter("TABLE_NAME")
            ))
            .await?;
        for r in cols {
            let Some(&ix) = r[0].as_ref().and_then(|t| index.get(t)) else { continue };
            let ts = &mut out[ix];
            let extra = r[6].clone().unwrap_or_default();
            let extra_l = extra.to_ascii_lowercase();
            let data_type = r[9].clone().unwrap_or_default().to_ascii_lowercase();
            let generation = r[8].clone().filter(|g| !g.is_empty());
            let default = if let Some(g) = generation.filter(|_| extra_l.contains("generated") && !extra_l.contains("default_generated")) {
                Some(format!("GENERATED ALWAYS AS ({g}) {}", if extra_l.contains("stored") { "STORED" } else { "VIRTUAL" }))
            } else {
                self.default_expr(r[4].clone(), &data_type, &extra_l)
            };
            ts.columns.push(ColumnDef {
                name: r[1].clone().unwrap_or_default(),
                data_type: r[2].clone().unwrap_or_default(),
                nullable: r[3].as_deref() == Some("YES"),
                default,
                is_primary_key: r[5].as_deref() == Some("PRI"),
                auto_increment: extra_l.contains("auto_increment"),
                comment: r[7].clone().filter(|c| !c.is_empty()),
                ordinal: ts.columns.len() as u32 + 1,
            });
        }
        let idx = self
            .rows(format!(
                "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME, SUB_PART, INDEX_TYPE \
                 FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {}{} \
                 ORDER BY TABLE_NAME, INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
                lit(db),
                tfilter("TABLE_NAME")
            ))
            .await?;
        for r in idx {
            let Some(&ix) = r[0].as_ref().and_then(|t| index.get(t)) else { continue };
            let ts = &mut out[ix];
            let name = r[1].clone().unwrap_or_default();
            let mut col = r[3].clone().unwrap_or_else(|| "(expression)".into());
            if let Some(sub) = &r[4] {
                col = format!("{col}({sub})");
            }
            match ts.indexes.iter_mut().find(|i| i.name == name) {
                Some(i) => i.columns.push(col),
                None => ts.indexes.push(IndexDef {
                    primary: name == "PRIMARY",
                    unique: r[2].as_deref() == Some("0"),
                    method: r[5].clone().map(|m| m.to_ascii_lowercase()),
                    columns: vec![col],
                    name,
                }),
            }
        }
        let fks = self
            .rows(format!(
                "SELECT k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, \
                        k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE \
                 FROM information_schema.KEY_COLUMN_USAGE k \
                 JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
                   ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
                 WHERE k.TABLE_SCHEMA = {}{} AND k.REFERENCED_TABLE_NAME IS NOT NULL \
                 ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
                lit(db),
                tfilter("k.TABLE_NAME")
            ))
            .await?;
        for r in fks {
            let Some(&ix) = r[0].as_ref().and_then(|t| index.get(t)) else { continue };
            let ts = &mut out[ix];
            let name = r[1].clone().unwrap_or_default();
            let (c, rc) = (r[2].clone().unwrap_or_default(), r[5].clone().unwrap_or_default());
            match ts.foreign_keys.iter_mut().find(|f| f.name == name) {
                Some(f) => {
                    f.columns.push(c);
                    f.ref_columns.push(rc);
                }
                None => ts.foreign_keys.push(ForeignKeyDef {
                    name,
                    columns: vec![c],
                    ref_schema: r[3].clone(),
                    ref_table: r[4].clone().unwrap_or_default(),
                    ref_columns: vec![rc],
                    on_update: r[6].clone(),
                    on_delete: r[7].clone(),
                }),
            }
        }
        Ok(out)
    }

    /// Render `COLUMN_DEFAULT` as a SQL expression.
    fn default_expr(&self, raw: Option<String>, data_type: &str, extra: &str) -> Option<String> {
        let raw = raw?;
        if self.mariadb {
            // MariaDB already returns SQL (quoted literals, NULL as "NULL").
            return (raw != "NULL").then_some(raw);
        }
        if extra.contains("default_generated") {
            let is_fn = raw.eq_ignore_ascii_case("CURRENT_TIMESTAMP") || raw.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP(");
            return Some(if is_fn || raw.starts_with('(') { raw } else { format!("({raw})") });
        }
        let numeric = matches!(
            data_type,
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "decimal" | "numeric" | "float" | "double" | "real" | "year"
        );
        Some(if numeric || data_type == "bit" && raw.starts_with("b'") { raw } else { lit(&raw) })
    }

    async fn show_create(&self, sql: String, col: &str) -> AppResult<String> {
        let row = sqlx::raw_sql(AssertSqlSafe(sql)).fetch_one(&self.pool).await?;
        let i = row
            .columns()
            .iter()
            .position(|c| c.name().eq_ignore_ascii_case(col))
            .ok_or_else(|| AppError::msg(format!("Unexpected SHOW CREATE output (no \"{col}\" column)")))?;
        let ddl = text_at(&row, i).ok_or_else(|| AppError::msg("The definition is not available (insufficient privileges?)"))?;
        Ok(format!("{};\n", ddl.trim_end().trim_end_matches(';')))
    }

    async fn column_types(&self, db: &str, table: &str) -> AppResult<HashMap<String, String>> {
        Ok(self
            .rows(format!(
                "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {}",
                lit(db),
                lit(table)
            ))
            .await?
            .into_iter()
            .filter_map(|r| Some((r[0].clone()?, r[1].clone()?.to_ascii_lowercase())))
            .collect())
    }
}

fn is_interrupted(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.try_downcast_ref::<MySqlDatabaseError>().is_some_and(|m| m.number() == 1317))
}

fn is_connection_error(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed)
}

fn my_error_text(e: &sqlx::Error) -> String {
    if let sqlx::Error::Database(db) = e {
        if let Some(my) = db.try_downcast_ref::<MySqlDatabaseError>() {
            return match my.code() {
                Some(state) => format!("ERROR {} ({state}): {}", my.number(), my.message()),
                None => format!("ERROR {}: {}", my.number(), my.message()),
            };
        }
    }
    e.to_string()
}

// ---------------------------------------------------------------------------
// Value conversion (text protocol)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    UInt,
    Float,
    Decimal,
    Bit,
    Json,
    Binary,
    /// strings, enum, set, dates and times
    Text,
}

impl Kind {
    fn of(ti: &MySqlTypeInfo) -> Self {
        if <str as Type<MySql>>::compatible(ti) {
            return Kind::Text;
        }
        let name = ti.name();
        if name.ends_with(" UNSIGNED") {
            return Kind::UInt;
        }
        match name {
            "TINYINT" | "SMALLINT" | "INT" | "MEDIUMINT" | "BIGINT" | "BOOLEAN" | "YEAR" => Kind::Int,
            "FLOAT" | "DOUBLE" => Kind::Float,
            "DECIMAL" => Kind::Decimal,
            "BIT" => Kind::Bit,
            "JSON" => Kind::Json,
            "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "GEOMETRY" => Kind::Binary,
            _ => Kind::Text,
        }
    }
}

/// Type name for display; sqlx names text columns with a `_bin` collation as BLOB/BINARY.
fn display_type(ti: &MySqlTypeInfo) -> String {
    let name = ti.name();
    if <str as Type<MySql>>::compatible(ti) {
        return match name {
            "BLOB" => "TEXT",
            "TINYBLOB" => "TINYTEXT",
            "MEDIUMBLOB" => "MEDIUMTEXT",
            "LONGBLOB" => "LONGTEXT",
            "VARBINARY" => "VARCHAR",
            "BINARY" => "CHAR",
            other => other,
        }
        .to_string();
    }
    name.to_string()
}

fn convert_row(row: &MySqlRow, kinds: &[Kind]) -> Vec<Value> {
    (0..row.len())
        .map(|i| {
            let null = row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true);
            if null {
                return Value::Null;
            }
            match row.try_get_unchecked::<&[u8], _>(i) {
                Ok(b) => cell_to_json(kinds.get(i).copied().unwrap_or(Kind::Text), b),
                Err(_) => Value::Null,
            }
        })
        .collect()
}

fn cell_to_json(kind: Kind, b: &[u8]) -> Value {
    let text = || std::str::from_utf8(b).ok();
    let as_string = || match std::str::from_utf8(b) {
        Ok(s) => Value::String(s.to_string()),
        Err(_) => sc::binary_to_value(b),
    };
    match kind {
        Kind::Int => match text().and_then(|s| s.parse::<i64>().ok()) {
            Some(n) if n.unsigned_abs() <= JS_MAX_SAFE_INT => Value::from(n),
            _ => as_string(),
        },
        Kind::UInt => match text().and_then(|s| s.parse::<u64>().ok()) {
            Some(n) if n <= JS_MAX_SAFE_INT => Value::from(n),
            _ => as_string(),
        },
        Kind::Float => text()
            .and_then(|s| s.parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(as_string),
        Kind::Bit => {
            // BIT(n) is sent as big-endian bytes.
            if b.len() <= 8 {
                let n = b.iter().fold(0u64, |acc, x| (acc << 8) | u64::from(*x));
                if n <= JS_MAX_SAFE_INT { Value::from(n) } else { Value::String(n.to_string()) }
            } else {
                sc::binary_to_value(b)
            }
        }
        Kind::Json => serde_json::from_slice(b).unwrap_or_else(|_| as_string()),
        Kind::Binary => sc::binary_to_value(b),
        Kind::Decimal | Kind::Text => as_string(),
    }
}

fn is_binary_type(data_type: &str) -> bool {
    matches!(data_type, "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "geometry" | "point" | "linestring" | "polygon")
}

fn folder(base: &[String], id: &str, label: &str) -> TreeNode {
    let mut path = base.to_vec();
    path.push(id.into());
    TreeNode { path, label: label.into(), kind: NodeKind::Folder, has_children: true, detail: None, table: None }
}

fn leaf(base: &[String], name: &str, kind: NodeKind, detail: Option<String>, table: Option<TableRef>) -> TreeNode {
    let mut path = base.to_vec();
    path.push(name.into());
    TreeNode { path, label: name.into(), kind, has_children: false, detail, table }
}

fn tref(db: &str, name: &str) -> TableRef {
    TableRef { database: Some(db.into()), schema: None, name: name.into() }
}

#[async_trait]
impl Driver for MysqlDriver {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Mysql
    }
    fn dialect(&self) -> Dialect {
        Dialect::Mysql
    }

    async fn server_version(&self) -> AppResult<String> {
        let v = self.strings("SELECT VERSION()".into()).await?.into_iter().next().unwrap_or_default();
        if v.to_ascii_lowercase().contains("mariadb") {
            // e.g. "10.11.6-MariaDB-1:10.11.6+maria~ubu2204" or "5.5.5-10.11.6-MariaDB"
            let v = v.trim_start_matches("5.5.5-");
            Ok(format!("MariaDB {}", v.split('-').next().unwrap_or(v)))
        } else {
            Ok(format!("MySQL {}", v.split('-').next().unwrap_or(&v)))
        }
    }

    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>> {
        match path {
            [] => Ok(self
                .list_databases()
                .await?
                .into_iter()
                .map(|db| TreeNode {
                    path: vec![db.clone()],
                    detail: SYSTEM_DBS.contains(&db.as_str()).then(|| "system".to_string()),
                    label: db,
                    kind: NodeKind::Database,
                    has_children: true,
                    table: None,
                })
                .collect()),
            [_] => Ok(vec![
                folder(path, "tables", "Tables"),
                folder(path, "views", "Views"),
                folder(path, "functions", "Functions"),
                folder(path, "procedures", "Procedures"),
                folder(path, "triggers", "Triggers"),
                folder(path, "events", "Events"),
            ]),
            [db, f] => match f.as_str() {
                "tables" => {
                    let rows = self
                        .rows(format!(
                            "SELECT TABLE_NAME, TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} \
                             AND TABLE_TYPE <> 'VIEW' ORDER BY TABLE_NAME",
                            lit(db)
                        ))
                        .await?;
                    Ok(rows
                        .into_iter()
                        .filter_map(|r| {
                            let name = r[0].clone()?;
                            let detail = r[1].as_ref().map(|n| if n == "1" { "1 row".to_string() } else { format!("{n} rows") });
                            Some(TreeNode {
                                path: [path, &[name.clone()]].concat(),
                                label: name.clone(),
                                kind: NodeKind::Table,
                                has_children: true,
                                detail,
                                table: Some(tref(db, &name)),
                            })
                        })
                        .collect())
                }
                "views" => {
                    let names = self
                        .strings(format!(
                            "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} AND TABLE_TYPE = 'VIEW' ORDER BY TABLE_NAME",
                            lit(db)
                        ))
                        .await?;
                    Ok(names.iter().map(|n| leaf(path, n, NodeKind::View, None, Some(tref(db, n)))).collect())
                }
                "functions" | "procedures" => {
                    let (ty, kind) = if f == "functions" { ("FUNCTION", NodeKind::Function) } else { ("PROCEDURE", NodeKind::Procedure) };
                    let rows = self
                        .rows(format!(
                            "SELECT ROUTINE_NAME, DTD_IDENTIFIER FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {} \
                             AND ROUTINE_TYPE = '{ty}' ORDER BY ROUTINE_NAME",
                            lit(db)
                        ))
                        .await?;
                    Ok(rows
                        .into_iter()
                        .filter_map(|r| {
                            let n = r[0].clone()?;
                            Some(leaf(path, &n, kind.clone(), r[1].clone(), Some(tref(db, &n))))
                        })
                        .collect())
                }
                "triggers" => {
                    let rows = self
                        .rows(format!(
                            "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, EVENT_OBJECT_TABLE FROM information_schema.TRIGGERS \
                             WHERE TRIGGER_SCHEMA = {} ORDER BY TRIGGER_NAME",
                            lit(db)
                        ))
                        .await?;
                    Ok(rows
                        .into_iter()
                        .filter_map(|r| {
                            let n = r[0].clone()?;
                            let d = format!(
                                "{} {} ON {}",
                                r[1].clone().unwrap_or_default(),
                                r[2].clone().unwrap_or_default(),
                                r[3].clone().unwrap_or_default()
                            );
                            Some(leaf(path, &n, NodeKind::Trigger, Some(d), Some(tref(db, &n))))
                        })
                        .collect())
                }
                "events" => {
                    let rows = self
                        .rows(format!(
                            "SELECT EVENT_NAME, STATUS FROM information_schema.EVENTS WHERE EVENT_SCHEMA = {} ORDER BY EVENT_NAME",
                            lit(db)
                        ))
                        .await?;
                    Ok(rows
                        .into_iter()
                        .filter_map(|r| {
                            let n = r[0].clone()?;
                            Some(leaf(path, &n, NodeKind::Event, r[1].clone(), Some(tref(db, &n))))
                        })
                        .collect())
                }
                other => Err(AppError::msg(format!("Unknown folder \"{other}\""))),
            },
            [_, f, _] if f == "tables" => Ok(vec![
                folder(path, "columns", "Columns"),
                folder(path, "indexes", "Indexes"),
                folder(path, "foreign_keys", "Foreign Keys"),
                folder(path, "triggers", "Triggers"),
            ]),
            [db, f, table, sub] if f == "tables" => {
                if sub == "triggers" {
                    let names = self
                        .strings(format!(
                            "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA = {} \
                             AND EVENT_OBJECT_TABLE = {} ORDER BY TRIGGER_NAME",
                            lit(db),
                            lit(table)
                        ))
                        .await?;
                    return Ok(names.iter().map(|n| leaf(path, n, NodeKind::Trigger, None, Some(tref(db, n)))).collect());
                }
                let st = self.structures(db, Some(table)).await?.into_iter().next().unwrap_or_default();
                Ok(match sub.as_str() {
                    "columns" => st
                        .columns
                        .iter()
                        .map(|c| {
                            let mut d = c.data_type.clone();
                            if c.is_primary_key {
                                d.push_str(" PK");
                            }
                            if !c.nullable {
                                d.push_str(" NOT NULL");
                            }
                            leaf(path, &c.name, NodeKind::Column, Some(d), None)
                        })
                        .collect(),
                    "indexes" => st
                        .indexes
                        .iter()
                        .map(|i| {
                            let d = format!(
                                "{}({})",
                                if i.primary { "PRIMARY " } else if i.unique { "UNIQUE " } else { "" },
                                i.columns.join(", ")
                            );
                            leaf(path, &i.name, NodeKind::Index, Some(d), Some(tref(db, table)))
                        })
                        .collect(),
                    "foreign_keys" => st
                        .foreign_keys
                        .iter()
                        .map(|f| {
                            let d = format!("({}) → {}({})", f.columns.join(", "), f.ref_table, f.ref_columns.join(", "));
                            leaf(path, &f.name, NodeKind::ForeignKey, Some(d), None)
                        })
                        .collect(),
                    other => return Err(AppError::msg(format!("Unknown folder \"{other}\""))),
                })
            }
            _ => Ok(vec![]),
        }
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        self.strings(
            "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
             ORDER BY SCHEMA_NAME IN ('information_schema', 'mysql', 'performance_schema', 'sys'), SCHEMA_NAME"
                .into(),
        )
        .await
    }

    async fn execute(&self, script: &str, opts: &ExecOptions) -> AppResult<Vec<QueryResult>> {
        let stmts = sc::split_script(D, script);
        if stmts.is_empty() {
            return Ok(vec![]);
        }
        if self.read_only {
            if let Some(i) = stmts.iter().position(|s| !sc::is_read_only_statement(D, &s.sql)) {
                return Err(sc::read_only_error(i + 1));
            }
        }
        let cancel_gen = self.cancel_gen.load(Ordering::SeqCst);
        let mut attempt = 0;
        loop {
            let mut lease = self.lease().await?;
            let id = self.next_run.fetch_add(1, Ordering::SeqCst);
            self.running.lock().insert(id, lease.session().id);
            let _guard = RunningGuard { map: &self.running, id };
            match self.run_script(&mut lease, &stmts, opts, cancel_gen).await {
                Ok(r) => return Ok(r),
                Err((i, e)) => {
                    if is_connection_error(&e) {
                        lease.discard();
                        if i == 0 && attempt == 0 {
                            attempt += 1;
                            continue;
                        }
                    }
                    let st = &stmts[i];
                    return Err(sc::statement_error(i + 1, stmts.len(), st.line(script), &st.sql, my_error_text(&e)));
                }
            }
        }
    }

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage> {
        let db = self.db_of(req.table.database.as_deref())?;
        let table = tref(&db, &req.table.name);
        let filter = req.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
        if let Some(f) = filter {
            if sc::split_statements(D, f).len() > 1 {
                return Err(AppError::msg("The filter must be a single SQL expression"));
            }
        }
        let pk = self.primary_key(&db, &req.table.name).await?;
        let sql = sc::select_page_sql(D, &table, filter, &req.sort, &pk, req.limit.max(1), req.offset);
        let mut conn = self.pool.acquire().await?;
        let mut results = Self::run_statement(&mut conn, &sql, usize::MAX, None).await.map_err(|e| AppError::msg(my_error_text(&e)))?;
        let result = results.swap_remove(0);
        let estimate: Option<u64> = self
            .strings(format!(
                "SELECT TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {}",
                lit(&db),
                lit(&req.table.name)
            ))
            .await?
            .first()
            .and_then(|s| s.parse().ok());
        let total = match estimate {
            Some(e) if filter.is_none() && e > 1_000_000 => Some(e),
            _ => {
                let row = sqlx::raw_sql(AssertSqlSafe(sc::count_sql(D, &table, filter))).fetch_one(&mut *conn).await?;
                text_at(&row, 0).and_then(|s| s.parse().ok())
            }
        };
        Ok(TableDataPage { result, total, primary_key: pk })
    }

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure> {
        let db = self.db_of(table.database.as_deref())?;
        self.structures(&db, Some(&table.name))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::msg(format!("Table `{db}`.`{}` not found", table.name)))
    }

    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64> {
        if self.read_only {
            return Err(AppError::msg("This connection is read-only"));
        }
        if changes.is_empty() {
            return Ok(0);
        }
        let db = self.db_of(table.database.as_deref())?;
        let t = tref(&db, &table.name);
        let types = self.column_types(&db, &table.name).await?;
        let mut tx = self.pool.begin().await?;
        let mut total = 0u64;
        for (n, change) in changes.iter().enumerate() {
            let stmt = sc::build_change(D, &t, change, &|_, p| p)?;
            let mut qy = sqlx::query(AssertSqlSafe(stmt.sql.clone()));
            for (v, col) in stmt.params.iter().zip(&stmt.param_columns) {
                let ty = types.get(col).map(String::as_str).unwrap_or("");
                qy = match v {
                    Value::Null => qy.bind(None::<String>),
                    Value::Bool(b) => qy.bind(*b),
                    Value::Number(num) => {
                        if let Some(i) = num.as_i64() {
                            qy.bind(i)
                        } else if let Some(u) = num.as_u64() {
                            qy.bind(u)
                        } else {
                            qy.bind(num.as_f64().unwrap_or_default())
                        }
                    }
                    Value::String(s) if is_binary_type(ty) => match sc::parse_hex_binary(s) {
                        Some(bytes) => qy.bind(bytes),
                        None => qy.bind(s.clone()),
                    },
                    Value::String(s) => qy.bind(s.clone()),
                    other => qy.bind(other.to_string()),
                };
            }
            let affected = match qy.execute(&mut *tx).await {
                Ok(r) => r.rows_affected(),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(AppError::msg(format!("Change {}: {}", n + 1, my_error_text(&e))));
                }
            };
            // sqlx sets CLIENT_FOUND_ROWS, so this is the number of matched rows.
            if stmt.expect_one && affected != 1 {
                let _ = tx.rollback().await;
                return Err(sc::row_count_error(n + 1, &stmt.sql, affected));
            }
            total += affected;
        }
        tx.commit().await?;
        Ok(total)
    }

    async fn object_ddl(&self, table: &TableRef, kind: &NodeKind) -> AppResult<String> {
        let db = self.db_of(table.database.as_deref())?;
        let qn = format!("{}.{}", q(&db), q(&table.name));
        match kind {
            NodeKind::Table => self.show_create(format!("SHOW CREATE TABLE {qn}"), "Create Table").await,
            NodeKind::View => self.show_create(format!("SHOW CREATE VIEW {qn}"), "Create View").await,
            NodeKind::Procedure => self.show_create(format!("SHOW CREATE PROCEDURE {qn}"), "Create Procedure").await,
            NodeKind::Function => self.show_create(format!("SHOW CREATE FUNCTION {qn}"), "Create Function").await,
            NodeKind::Trigger => self.show_create(format!("SHOW CREATE TRIGGER {qn}"), "SQL Original Statement").await,
            NodeKind::Event => self.show_create(format!("SHOW CREATE EVENT {qn}"), "Create Event").await,
            NodeKind::Database => self.show_create(format!("SHOW CREATE DATABASE {}", q(&table.name)), "Create Database").await,
            other => Err(AppError::Unsupported(format!("DDL for {other:?}"))),
        }
    }

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot> {
        let db = self.db_of(database.or(schema))?;
        let tables = self.structures(&db, None).await?;
        let views = self
            .rows(format!(
                "SELECT TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                lit(&db)
            ))
            .await?
            .into_iter()
            .filter_map(|r| Some((tref(&db, r[0].as_deref()?), r[1].clone().unwrap_or_default())))
            .collect();
        Ok(SchemaSnapshot { tables, views })
    }

    async fn cancel(&self) -> AppResult<()> {
        self.cancel_gen.fetch_add(1, Ordering::SeqCst);
        let ids: Vec<u64> = self.running.lock().values().copied().collect();
        for id in ids {
            sqlx::raw_sql(AssertSqlSafe(format!("KILL QUERY {id}"))).execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn close(&self) {
        if let Ok(mut g) = self.session.try_lock() {
            g.take();
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), self.pool.close()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cells() {
        assert_eq!(cell_to_json(Kind::Int, b"-12"), json!(-12));
        assert_eq!(cell_to_json(Kind::UInt, b"18446744073709551615"), json!("18446744073709551615"));
        assert_eq!(cell_to_json(Kind::Float, b"1.5"), json!(1.5));
        assert_eq!(cell_to_json(Kind::Decimal, b"10.20"), json!("10.20"));
        assert_eq!(cell_to_json(Kind::Bit, &[0x01, 0x02]), json!(258));
        assert_eq!(cell_to_json(Kind::Json, br#"{"a":1}"#), json!({"a": 1}));
        assert_eq!(cell_to_json(Kind::Binary, &[0xde, 0xad]), json!("\\xdead"));
        assert_eq!(cell_to_json(Kind::Text, &[0xff, 0x00]), json!("\\xff00"));
        assert_eq!(cell_to_json(Kind::Text, b"0000-00-00 00:00:00"), json!("0000-00-00 00:00:00"));
    }
}

/// Integration tests against the docker-compose MySQL (`localhost:53306`).
/// Run with `cargo test --lib -- --ignored mysql::integration`.
#[cfg(test)]
mod integration {
    use super::*;
    use serde_json::{json, Map};

    fn cfg() -> ConnectionConfig {
        ConnectionConfig {
            id: "my-it".into(),
            name: "mysql".into(),
            kind: ConnectionKind::Mysql,
            host: "127.0.0.1".into(),
            port: Some(53306),
            user: "root".into(),
            password: Some("devcat".into()),
            database: Some("devcat".into()),
            file_path: None,
            url: None,
            ssl_mode: None,
            ssh: None,
            color: None,
            group: None,
            project_id: None,
            options: Default::default(),
            read_only: false,
            mcp_access: None,
        }
    }

    fn opts() -> ExecOptions {
        ExecOptions { database: Some("devcat_it".into()), schema: None, max_rows: None }
    }

    const SETUP: &str = r#"
        DROP DATABASE IF EXISTS devcat_it;
        CREATE DATABASE devcat_it;
        USE devcat_it;
        CREATE TABLE authors (
            id INT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL COMMENT 'Full name',
            code VARCHAR(20) COLLATE utf8mb4_bin,
            born DATE,
            UNIQUE KEY uq_name (name(50))
        ) COMMENT 'Book authors';
        CREATE TABLE books (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            author_id INT UNSIGNED,
            title VARCHAR(200) NOT NULL DEFAULT 'untitled',
            price DECIMAL(10,2),
            meta JSON,
            cover BLOB,
            uid BINARY(16),
            published DATETIME(3),
            created TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            ok TINYINT(1),
            score DOUBLE,
            big BIGINT UNSIGNED,
            flags BIT(10),
            yr YEAR,
            kind ENUM('a','b'),
            tags SET('x','y'),
            pt POINT,
            body TEXT,
            title_len INT GENERATED ALWAYS AS (CHAR_LENGTH(title)) STORED,
            KEY idx_title (title),
            CONSTRAINT fk_author FOREIGN KEY (author_id) REFERENCES authors(id) ON DELETE CASCADE
        );
        INSERT INTO authors (name, code, born) VALUES ('Ann', 'A1', '1970-01-02'), ('Bob', NULL, NULL), ('Cid', 'c', '1990-12-31');
        INSERT INTO books (author_id, title, price, meta, cover, uid, published, ok, score, big, flags, yr, kind, tags, pt, body)
        VALUES (1, 'A; tricky ''title'' \\ x', 9.99, '{"a": [1, 2]}', X'00FF', UNHEX('a0eebc999c0b4ef8bb6d6bb9bd380a11'),
                '2024-01-02 03:04:05.123', 1, 1.5, 18446744073709551615, b'1000000001', 2024, 'b', 'x,y', ST_GeomFromText('POINT(1 2)'), 'long text'),
               (2, 'Second', NULL, NULL, NULL, NULL, NULL, 0, NULL, 5, NULL, NULL, NULL, '', NULL, NULL);
        CREATE VIEW cheap_books AS SELECT id, title FROM books WHERE price < 10;
        CREATE FUNCTION add_one(i INT) RETURNS INT DETERMINISTIC RETURN i + 1;
        CREATE PROCEDURE two_sets(IN x INT)
        BEGIN
            IF x > 0 THEN
                SELECT 1 AS a;
            END IF;
            SELECT 'b' AS b, 2 AS c;
        END;
        CREATE TRIGGER books_bi BEFORE INSERT ON books FOR EACH ROW SET NEW.title = TRIM(NEW.title);
        CREATE EVENT ev_noop ON SCHEDULE EVERY 1 DAY DISABLE DO SELECT 1;
    "#;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[tokio::test]
    #[ignore]
    async fn mysql_end_to_end() {
        let d = MysqlDriver::connect(&cfg()).await.expect("connect");
        d.execute(SETUP, &ExecOptions { database: None, schema: None, max_rows: None }).await.expect("setup");
        let v = d.server_version().await.unwrap();
        assert!(v.starts_with("MySQL 8") || v.starts_with("MariaDB"), "{v}");

        // --- tree
        let dbs = d.children(&[]).await.unwrap();
        assert!(dbs.iter().any(|n| n.label == "devcat_it"));
        assert_eq!(dbs.last().unwrap().detail.as_deref(), Some("system"));
        let tables = d.children(&s(&["devcat_it", "tables"])).await.unwrap();
        assert_eq!(tables.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), vec!["authors", "books"]);
        assert_eq!(d.children(&s(&["devcat_it", "views"])).await.unwrap()[0].label, "cheap_books");
        assert_eq!(d.children(&s(&["devcat_it", "functions"])).await.unwrap()[0].label, "add_one");
        assert_eq!(d.children(&s(&["devcat_it", "procedures"])).await.unwrap()[0].label, "two_sets");
        assert_eq!(d.children(&s(&["devcat_it", "triggers"])).await.unwrap()[0].label, "books_bi");
        assert_eq!(d.children(&s(&["devcat_it", "events"])).await.unwrap()[0].label, "ev_noop");
        let cols = d.children(&s(&["devcat_it", "tables", "books", "columns"])).await.unwrap();
        assert_eq!(cols.len(), 19);
        let idx = d.children(&s(&["devcat_it", "tables", "authors", "indexes"])).await.unwrap();
        assert!(idx.iter().any(|i| i.label == "uq_name" && i.detail.as_deref() == Some("UNIQUE (name(50))")), "{idx:?}");
        assert_eq!(d.children(&s(&["devcat_it", "tables", "books", "foreign_keys"])).await.unwrap().len(), 1);
        assert_eq!(d.children(&s(&["devcat_it", "tables", "books", "triggers"])).await.unwrap().len(), 1);

        // --- execute & value conversion
        let r = d.execute("select * from books order by id; set @x = 5; select @x as x", &opts()).await.unwrap();
        assert_eq!(r.len(), 3);
        let row = &r[0].rows[0];
        let col = |n: &str| r[0].columns.iter().position(|c| c.name == n).unwrap();
        assert_eq!(row[col("id")], json!(1));
        assert_eq!(row[col("title")], json!("A; tricky 'title' \\ x"));
        assert_eq!(row[col("price")], json!("9.99"));
        assert_eq!(row[col("meta")], json!({"a": [1, 2]}));
        assert_eq!(row[col("cover")], json!("\\x00ff"));
        assert_eq!(row[col("uid")], json!("\\xa0eebc999c0b4ef8bb6d6bb9bd380a11"));
        assert_eq!(row[col("published")], json!("2024-01-02 03:04:05.123"));
        assert_eq!(row[col("ok")], json!(1));
        assert_eq!(row[col("score")], json!(1.5));
        assert_eq!(row[col("big")], json!("18446744073709551615"));
        assert_eq!(row[col("flags")], json!(513));
        assert_eq!(row[col("yr")], json!(2024));
        assert_eq!(row[col("kind")], json!("b"));
        assert_eq!(row[col("tags")], json!("x,y"));
        assert!(row[col("pt")].as_str().unwrap().starts_with("\\x"));
        assert_eq!(row[col("body")], json!("long text"));
        assert_eq!(r[0].columns[col("body")].type_name, "TEXT");
        assert_eq!(r[0].rows[1][col("big")], json!(5));
        assert_eq!(r[1].rows_affected, Some(0));
        assert_eq!(r[2].rows[0][0], json!(5));
        // session variables persist in the sticky session
        assert_eq!(d.execute("select @x", &opts()).await.unwrap()[0].rows[0][0], json!(5));
        // utf8mb4_bin text is still text
        let r = d.execute("select code from authors where id = 1", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0][0], json!("A1"));
        // empty result keeps columns
        let r = d.execute("select id, title from books where 1 = 0", &opts()).await.unwrap();
        assert_eq!(r[0].columns.len(), 2);
        // multiple result sets from one CALL
        let r = d.execute("call two_sets(1)", &opts()).await.unwrap();
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r[1].rows[0], vec![json!("b"), json!(2)]);
        // DELIMITER scripts
        let r = d
            .execute("DELIMITER $$\nCREATE PROCEDURE p2() BEGIN SELECT 7; END$$\nDELIMITER ;\nCALL p2();", &opts())
            .await
            .unwrap();
        assert_eq!(r.last().unwrap().rows[0][0], json!(7));
        // insert reports last insert id
        let r = d.execute("insert into authors(name) values ('Tmp')", &opts()).await.unwrap();
        assert_eq!(r[0].rows_affected, Some(1));
        assert!(r[0].message.as_deref().unwrap().starts_with("Last insert id"));
        // transaction across executions
        d.execute("start transaction; delete from authors where name = 'Tmp'", &opts()).await.unwrap();
        d.execute("rollback", &opts()).await.unwrap();
        assert_eq!(d.execute("select count(*) from authors where name = 'Tmp'", &opts()).await.unwrap()[0].rows[0][0], json!(1));
        // errors
        let err = d.execute("select 1;\nselect * from nope", &opts()).await.unwrap_err().to_string();
        assert!(err.contains("statement 2 of 2") && err.contains("1146"), "{err}");
        // max rows
        let r = d
            .execute("select * from information_schema.COLUMNS", &ExecOptions { max_rows: Some(5), ..opts() })
            .await
            .unwrap();
        assert_eq!(r[0].rows.len(), 5);
        assert!(r[0].truncated);

        // --- table data
        let authors = TableRef { database: Some("devcat_it".into()), schema: None, name: "authors".into() };
        let page = d
            .table_data(&TableDataRequest {
                table: authors.clone(),
                offset: 0,
                limit: 2,
                filter: Some("born IS NOT NULL OR name = 'Bob'".into()),
                sort: vec![SortSpec { column: "name".into(), desc: true }],
            })
            .await
            .unwrap();
        assert_eq!(page.total, Some(3));
        assert_eq!(page.primary_key, vec!["id"]);
        assert_eq!(page.result.rows[0][1], json!("Cid"));

        // --- structure
        let books = TableRef { name: "books".into(), ..authors.clone() };
        let st = d.table_structure(&books).await.unwrap();
        assert!(st.columns[0].is_primary_key && st.columns[0].auto_increment);
        let title = st.columns.iter().find(|c| c.name == "title").unwrap();
        assert_eq!(title.default.as_deref(), Some("'untitled'"));
        assert_eq!(title.data_type, "varchar(200)");
        let created = st.columns.iter().find(|c| c.name == "created").unwrap();
        assert_eq!(created.default.as_deref(), Some("CURRENT_TIMESTAMP"));
        let gen_ = st.columns.iter().find(|c| c.name == "title_len").unwrap();
        assert!(gen_.default.as_deref().unwrap().starts_with("GENERATED ALWAYS AS"), "{gen_:?}");
        assert_eq!(st.foreign_keys[0].ref_table, "authors");
        assert_eq!(st.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
        let at = d.table_structure(&authors).await.unwrap();
        assert_eq!(at.comment.as_deref(), Some("Book authors"));
        assert_eq!(at.columns[1].comment.as_deref(), Some("Full name"));

        // --- apply changes
        let mut ins = Map::new();
        ins.insert("author_id".into(), json!(3));
        ins.insert("title".into(), json!("  New  "));
        ins.insert("meta".into(), json!({"k": true}));
        ins.insert("cover".into(), json!("\\x0102"));
        ins.insert("ok".into(), json!(true));
        ins.insert("price".into(), json!("1.25"));
        assert_eq!(d.apply_changes(&books, &[RowChange::Insert { values: ins }]).await.unwrap(), 1);
        let r = d.execute("select title, meta, cover, ok, price from books where author_id = 3", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0], vec![json!("New"), json!({"k": true}), json!("\\x0102"), json!(1), json!("1.25")]);
        let key: Map<String, Value> = [("id".to_string(), json!(2))].into_iter().collect();
        // updating to the same value still counts as one matched row
        let vals: Map<String, Value> = [("name".to_string(), json!("Bob"))].into_iter().collect();
        assert_eq!(d.apply_changes(&authors, &[RowChange::Update { key: key.clone(), values: vals }]).await.unwrap(), 1);
        let missing: Map<String, Value> = [("id".to_string(), json!(999))].into_iter().collect();
        let err = d
            .apply_changes(&authors, &[RowChange::Delete { key: key.clone() }, RowChange::Delete { key: missing }])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("affected 0 rows"), "{err}");
        assert_eq!(d.execute("select count(*) from authors where id = 2", &opts()).await.unwrap()[0].rows[0][0], json!(1));

        // --- DDL
        let ddl = d.object_ddl(&books, &NodeKind::Table).await.unwrap();
        assert!(ddl.starts_with("CREATE TABLE `books`") && ddl.ends_with(";\n"), "{ddl}");
        let v = d.object_ddl(&TableRef { name: "cheap_books".into(), ..authors.clone() }, &NodeKind::View).await.unwrap();
        assert!(v.contains("VIEW `devcat_it`.`cheap_books`"), "{v}");
        let p = d.object_ddl(&TableRef { name: "two_sets".into(), ..authors.clone() }, &NodeKind::Procedure).await.unwrap();
        assert!(p.contains("PROCEDURE `two_sets`"), "{p}");
        let f = d.object_ddl(&TableRef { name: "add_one".into(), ..authors.clone() }, &NodeKind::Function).await.unwrap();
        assert!(f.contains("FUNCTION `add_one`"), "{f}");
        let t = d.object_ddl(&TableRef { name: "books_bi".into(), ..authors.clone() }, &NodeKind::Trigger).await.unwrap();
        assert!(t.contains("TRIGGER `books_bi`"), "{t}");
        let e = d.object_ddl(&TableRef { name: "ev_noop".into(), ..authors.clone() }, &NodeKind::Event).await.unwrap();
        assert!(e.contains("EVENT `ev_noop`"), "{e}");
        // procedure DDL can be replayed without DELIMITER
        d.execute(&format!("DROP PROCEDURE two_sets; {p}"), &opts()).await.expect("replay procedure");

        // --- snapshot
        let snap = d.schema_snapshot(Some("devcat_it"), None).await.unwrap();
        assert_eq!(snap.tables.len(), 2);
        assert_eq!(snap.views.len(), 1);
        assert_eq!(snap.tables.iter().find(|t| t.table.as_ref().unwrap().name == "books").unwrap().columns.len(), 19);

        // --- cancel
        let d = Arc::new(d);
        let d2 = d.clone();
        let h = tokio::spawn(async move { d2.execute("select sleep(20); select 1", &opts()).await });
        tokio::time::sleep(Duration::from_millis(700)).await;
        d.cancel().await.unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel took too long").unwrap();
        // SLEEP() returns 1 when interrupted rather than failing; the script still stops.
        match res {
            Ok(r) => panic!("expected cancellation, got {r:?}"),
            Err(e) => assert!(e.to_string().to_lowercase().contains("interrupt") || e.to_string().contains("cancel"), "{e}"),
        }
        d.execute("DROP DATABASE devcat_it", &opts()).await.unwrap();
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn mysql_snapshot_many_tables() {
        let d = MysqlDriver::connect(&cfg()).await.unwrap();
        let o = ExecOptions { database: None, schema: None, max_rows: None };
        let mut script = String::from("DROP DATABASE IF EXISTS devcat_many; CREATE DATABASE devcat_many; USE devcat_many;\n");
        for i in 1..=600 {
            script.push_str(&format!(
                "CREATE TABLE t{i} (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(50) NOT NULL, parent INT, \
                 created TIMESTAMP DEFAULT CURRENT_TIMESTAMP, KEY (name), FOREIGN KEY (parent) REFERENCES t1(id));\n"
            ));
        }
        d.execute(&script, &o).await.unwrap();
        let started = Instant::now();
        let snap = d.schema_snapshot(Some("devcat_many"), None).await.unwrap();
        let took = started.elapsed();
        assert_eq!(snap.tables.len(), 600);
        assert!(snap.tables.iter().all(|t| t.columns.len() == 4 && t.foreign_keys.len() == 1), "{:?}", snap.tables[1]);
        assert!(took < Duration::from_secs(5), "snapshot took {took:?}");
        eprintln!("mysql snapshot of 600 tables: {took:?}");
        d.execute("DROP DATABASE devcat_many", &o).await.unwrap();
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn mysql_truncation_stops_early() {
        let d = MysqlDriver::connect(&cfg()).await.unwrap();
        let o = ExecOptions { database: None, schema: None, max_rows: Some(1000) };
        // 10^7 rows from a cross join of a 10-row derived table
        let big = "WITH d AS (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 \
                   UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) \
                   SELECT a.n + 10 * b.n + 100 * c.n + 1000 * e.n + 10000 * f.n + 100000 * g.n + 1000000 * h.n AS v \
                   FROM d a, d b, d c, d e, d f, d g, d h";
        let started = Instant::now();
        let r = d.execute(big, &o).await.unwrap();
        let took = started.elapsed();
        eprintln!("mysql 10M-row select truncated in {took:?}");
        assert!(took < Duration::from_millis(1500), "took {took:?}");
        assert!(r[0].truncated);
        assert_eq!(r[0].rows.len(), 1000);
        assert_eq!(d.execute("select 42", &o).await.unwrap()[0].rows[0][0], json!(42));
        let r = d.execute(&format!("{big}; select 'next'"), &o).await.unwrap();
        assert!(r[0].truncated);
        assert_eq!(r[1].rows[0][0], json!("next"));
        // KILL QUERY doesn't roll back an open transaction
        d.execute("create temporary table early_tx(a int) engine=innodb; start transaction; insert into early_tx values (1)", &o).await.unwrap();
        let r = d.execute(big, &o).await.unwrap();
        assert!(r[0].truncated);
        assert_eq!(d.execute("select count(*) from early_tx", &o).await.unwrap()[0].rows[0][0], json!(1));
        d.execute("rollback", &o).await.unwrap();
        assert_eq!(d.execute("select count(*) from early_tx", &o).await.unwrap()[0].rows[0][0], json!(0));
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn mysql_read_only() {
        let mut c = cfg();
        c.read_only = true;
        let d = MysqlDriver::connect(&c).await.unwrap();
        assert!(d.execute("select 1", &ExecOptions { database: None, schema: None, max_rows: None }).await.is_ok());
        let err = d.execute("delete from mysql.user", &ExecOptions { database: None, schema: None, max_rows: None }).await.unwrap_err();
        assert!(err.to_string().contains("read-only"));
        d.close().await;
    }
}
