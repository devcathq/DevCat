//! PostgreSQL driver built on sqlx.
//!
//! A Postgres connection is bound to one database, so the driver keeps a lazily
//! created pool per database. Query-editor executions run on a "sticky" session
//! connection per database so `SET`, temp tables and explicit transactions carry
//! over between executions (like a psql session).
//!
//! User SQL and grid data are fetched with the simple query protocol, which
//! returns every value in its text form; [`text_to_json`] then converts the
//! text to JSON based on the column type, so any type (including extension and
//! user-defined types) can be displayed.
use super::sql_common::{self as sc};
use super::ssh::{open_endpoint, SshTunnel};
use super::*;
use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use async_trait::async_trait;
use futures::TryStreamExt;
use serde_json::Value;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{
    PgConnectOptions, PgConnection, PgDatabaseError, PgErrorPosition, PgPool, PgPoolOptions, PgRow, PgSslMode, PgTypeInfo,
    PgTypeKind, PgValueFormat, Postgres,
};
use sqlx::{AssertSqlSafe, Column, Either, Executor, Row, SqlSafeStr, Statement, TypeInfo, ValueRef};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

const D: Dialect = Dialect::Postgres;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_ROWS: usize = 1000;
/// Largest integer a JavaScript number represents exactly.
const JS_MAX_SAFE_INT: i64 = 9_007_199_254_740_991;
const SYSTEM_SCHEMAS: [&str; 2] = ["pg_catalog", "information_schema"];

/// A dedicated connection used by the query editor.
struct Session {
    conn: PoolConnection<Postgres>,
    pid: i32,
    search_path: Option<String>,
}

impl Session {
    async fn open(pool: &PgPool) -> Result<Self, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *conn).await?;
        Ok(Session { conn, pid, search_path: None })
    }
}

/// Either the sticky session of a database or a temporary connection used
/// while the sticky one is busy (closed afterwards so no state leaks).
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
    /// Drop a broken sticky session so the next execution reconnects.
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

/// Registry entry for an in-flight execution (for `cancel`).
struct RunningGuard<'a> {
    map: &'a parking_lot::Mutex<HashMap<u64, (String, i32)>>,
    id: u64,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().remove(&self.id);
    }
}

pub struct PostgresDriver {
    opts: PgConnectOptions,
    default_db: String,
    read_only: bool,
    version_num: i32,
    pools: RwLock<HashMap<String, PgPool>>,
    sessions: parking_lot::Mutex<HashMap<String, Arc<Mutex<Option<Session>>>>>,
    running: parking_lot::Mutex<HashMap<u64, (String, i32)>>,
    next_run: AtomicU64,
    /// Bumped by `cancel` so multi-statement scripts stop between statements.
    cancel_gen: AtomicU64,
    /// Types sqlx does not know (extensions, enums, domains...) by OID.
    type_names: parking_lot::RwLock<HashMap<u32, (String, ValueKind)>>,
    _tunnel: Option<SshTunnel>,
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn ssl_mode(s: &str) -> Option<PgSslMode> {
    Some(match s.to_ascii_lowercase().as_str() {
        "disable" | "disabled" => PgSslMode::Disable,
        "allow" => PgSslMode::Allow,
        "prefer" | "preferred" => PgSslMode::Prefer,
        "require" | "required" => PgSslMode::Require,
        "verify-ca" | "verify_ca" => PgSslMode::VerifyCa,
        "verify-full" | "verify_full" | "verify-identity" => PgSslMode::VerifyFull,
        _ => return None,
    })
}

impl PostgresDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> AppResult<Self> {
        let url = non_empty(&cfg.url);
        let mut opts = match url {
            Some(u) => PgConnectOptions::from_str(u)?,
            None => PgConnectOptions::new_without_pgpass()
                .username(if cfg.user.trim().is_empty() { "postgres" } else { cfg.user.trim() }),
        };
        if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
            opts = opts.password(pw);
        }
        let mut ecfg = cfg.clone();
        if url.is_some() {
            ecfg.host = opts.get_host().to_string();
            ecfg.port = Some(opts.get_port());
        }
        let ep = open_endpoint(&ecfg, 5432).await?;
        opts = opts.host(&ep.host).port(ep.port).application_name("DevCat").statement_cache_capacity(64);
        if let Some(mode) = non_empty(&cfg.ssl_mode) {
            opts = opts.ssl_mode(ssl_mode(mode).ok_or_else(|| AppError::msg(format!("Unknown SSL mode \"{mode}\"")))?);
        }
        if cfg.read_only {
            opts = opts.options([("default_transaction_read_only", "on")]);
        }
        let default_db = non_empty(&cfg.database)
            .map(str::to_string)
            .or_else(|| opts.get_database().map(str::to_string))
            .unwrap_or_else(|| "postgres".into());

        let mut drv = PostgresDriver {
            opts,
            default_db: default_db.clone(),
            read_only: cfg.read_only,
            version_num: 0,
            pools: RwLock::new(HashMap::new()),
            sessions: parking_lot::Mutex::new(HashMap::new()),
            running: parking_lot::Mutex::new(HashMap::new()),
            next_run: AtomicU64::new(1),
            cancel_gen: AtomicU64::new(0),
            type_names: parking_lot::RwLock::new(HashMap::new()),
            _tunnel: ep.tunnel,
        };
        let pool = drv.pool(&default_db).await?;
        let num: String = sqlx::query_scalar("SHOW server_version_num").fetch_one(&pool).await?;
        drv.version_num = num.trim().parse().unwrap_or(0);
        Ok(drv)
    }

    fn db<'a>(&'a self, db: Option<&'a str>) -> &'a str {
        db.map(str::trim).filter(|d| !d.is_empty()).unwrap_or(&self.default_db)
    }

    async fn pool(&self, db: &str) -> AppResult<PgPool> {
        if let Some(p) = self.pools.read().await.get(db) {
            return Ok(p.clone());
        }
        let mut pools = self.pools.write().await;
        if let Some(p) = pools.get(db) {
            return Ok(p.clone());
        }
        let fut = PgPoolOptions::new()
            .max_connections(6)
            .min_connections(0)
            .acquire_timeout(CONNECT_TIMEOUT)
            .idle_timeout(Duration::from_secs(600))
            .connect_with(self.opts.clone().database(db));
        let pool = tokio::time::timeout(CONNECT_TIMEOUT + Duration::from_secs(1), fut)
            .await
            .map_err(|_| AppError::msg(format!("Timed out connecting to database \"{db}\"")))??;
        pools.insert(db.to_string(), pool.clone());
        Ok(pool)
    }

    async fn lease(&self, db: &str) -> AppResult<Lease> {
        let slot = self.sessions.lock().entry(db.to_string()).or_default().clone();
        match slot.try_lock_owned() {
            Ok(mut guard) => {
                if guard.is_none() {
                    *guard = Some(Session::open(&self.pool(db).await?).await?);
                }
                Ok(Lease::Sticky(guard))
            }
            Err(_) => {
                let mut s = Session::open(&self.pool(db).await?).await?;
                s.conn.close_on_drop();
                Ok(Lease::Temp(s))
            }
        }
    }

    /// Resolve name and value kind of type OIDs sqlx doesn't know about.
    async fn resolve_types(&self, conn: &mut PgConnection, oids: Vec<u32>) {
        let missing: Vec<i64> = {
            let known = self.type_names.read();
            oids.into_iter().filter(|o| !known.contains_key(o)).map(i64::from).collect()
        };
        if missing.is_empty() {
            return;
        }
        let rows: Result<Vec<(i64, String, String, Option<String>)>, _> = sqlx::query_as(
            "SELECT t.oid::int8, format_type(t.oid, NULL), \
                    COALESCE(bt.typname, t.typname)::text, \
                    CASE WHEN t.typcategory = 'A' THEN COALESCE(ebt.typname, et.typname)::text END \
             FROM pg_type t \
             LEFT JOIN pg_type bt ON bt.oid = t.typbasetype AND t.typtype = 'd' \
             LEFT JOIN pg_type et ON et.oid = t.typelem \
             LEFT JOIN pg_type ebt ON ebt.oid = et.typbasetype AND et.typtype = 'd' \
             WHERE t.oid = ANY($1::int8[]::oid[])",
        )
        .bind(&missing)
        .fetch_all(&mut *conn)
        .await;
        if let Ok(rows) = rows {
            let mut known = self.type_names.write();
            for (oid, name, base, elem) in rows {
                let kind = match elem {
                    Some(e) => ValueKind::Array(scalar_from_name(&e), if e == "box" { ';' } else { ',' }),
                    None => ValueKind::Scalar(scalar_from_name(&base)),
                };
                known.insert(oid as u32, (name, kind));
            }
        }
    }

    /// Display name and value kind of a column type.
    fn type_meta(&self, ti: &PgTypeInfo) -> (String, ValueKind) {
        if ti.name() == "?" {
            // Unresolved OID: never call `kind()` on these (sqlx panics).
            let oid = ti.oid().map(|o| o.0).unwrap_or(0);
            return self
                .type_names
                .read()
                .get(&oid)
                .cloned()
                .unwrap_or_else(|| (format!("oid:{oid}"), ValueKind::Scalar(Scalar::Text)));
        }
        (ti.name().to_string(), ValueKind::of(ti))
    }

    /// Run one statement on `conn`, collecting at most `max_rows` rows.
    ///
    /// `early` = (database, backend pid) of `conn`: once more than `max_rows`
    /// rows arrived, a side-effect free query is cancelled server-side so the
    /// rest of a huge result isn't transferred (see [`Self::stop_early`]).
    async fn run_statement(&self, conn: &mut PgConnection, sql: &str, max_rows: usize, early: Option<(&str, i32)>) -> Result<QueryResult, sqlx::Error> {
        let started = Instant::now();
        let mut columns: Option<Vec<(String, PgTypeInfo)>> = None;
        let mut raw_rows: Vec<Vec<Cell>> = Vec::new();
        let mut truncated = false;
        let mut affected = 0u64;
        let mut early = early.filter(|_| sc::can_stop_early(D, sql));
        let mut stopped = false;
        {
            let mut stream = sqlx::raw_sql(AssertSqlSafe(sql.to_string())).fetch_many(&mut *conn);
            loop {
                let item = match stream.try_next().await {
                    Ok(Some(item)) => item,
                    Ok(None) => break,
                    // Our own cancellation after truncation: the result is complete for us.
                    Err(e) if stopped && is_query_canceled(&e) => break,
                    Err(e) => return Err(e),
                };
                match item {
                    Either::Left(done) => affected += done.rows_affected(),
                    Either::Right(row) => {
                        if columns.is_none() {
                            columns = Some(row.columns().iter().map(|c| (c.name().to_string(), c.type_info().clone())).collect());
                        }
                        if raw_rows.len() < max_rows {
                            raw_rows.push(raw_row(&row));
                        } else {
                            truncated = true;
                            if let Some((db, pid)) = early.take() {
                                stopped = self.stop_early(db, pid).await;
                            }
                        }
                    }
                }
            }
        }
        // No rows: ask the server for the result shape of row-returning statements.
        if columns.is_none() && sc::may_return_rows(D, sql) {
            if let Ok(stmt) = (&mut *conn).prepare(AssertSqlSafe(sql.to_string()).into_sql_str()).await {
                if !stmt.columns().is_empty() {
                    columns = Some(stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().clone())).collect());
                }
            }
        }
        let has_result_set = columns.is_some();
        let columns = columns.unwrap_or_default();
        let unknown: Vec<u32> = columns.iter().filter(|(_, t)| t.name() == "?").filter_map(|(_, t)| t.oid().map(|o| o.0)).collect();
        if !unknown.is_empty() {
            self.resolve_types(conn, unknown).await;
        }
        let metas: Vec<(String, ValueKind)> = columns.iter().map(|(_, t)| self.type_meta(t)).collect();
        let kinds: Vec<ValueKind> = metas.iter().map(|(_, k)| *k).collect();
        Ok(QueryResult {
            statement: sql.to_string(),
            columns: columns
                .iter()
                .zip(metas)
                .map(|((n, _), (type_name, _))| ColumnInfo { name: n.clone(), type_name })
                .collect(),
            rows: raw_rows.into_iter().map(|r| convert_row(r, &kinds)).collect(),
            rows_affected: if has_result_set { None } else { Some(affected) },
            elapsed_ms: started.elapsed().as_millis() as u64,
            truncated,
            message: None,
        })
    }

    /// Cancel the query running on backend `pid` unless it runs inside an
    /// explicit transaction block (a cancel would abort the user's transaction;
    /// then the remaining rows are drained instead). In an implicit transaction
    /// `xact_start` equals `query_start`.
    async fn stop_early(&self, db: &str, pid: i32) -> bool {
        let Ok(pool) = self.pool(db).await else { return false };
        let implicit: Option<Option<bool>> = sqlx::query_scalar("SELECT xact_start = query_start FROM pg_stat_activity WHERE pid = $1")
            .bind(pid)
            .fetch_optional(&pool)
            .await
            .unwrap_or(None);
        if implicit.flatten() != Some(true) {
            return false;
        }
        sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)").bind(pid).fetch_one(&pool).await.unwrap_or(false)
    }

    async fn run_script(
        &self,
        db: &str,
        lease: &mut Lease,
        stmts: &[sc::ScriptStatement],
        opts: &ExecOptions,
        cancel_gen: u64,
    ) -> Result<Vec<QueryResult>, (usize, sqlx::Error)> {
        let session = lease.session();
        if let Some(schema) = opts.schema.as_deref().filter(|s| !s.is_empty()) {
            if session.search_path.as_deref() != Some(schema) {
                let path = if schema == "public" { sc::quote_ident(D, schema) } else { format!("{}, public", sc::quote_ident(D, schema)) };
                // Ignore failures (e.g. inside an aborted transaction) so ROLLBACK still runs.
                if sqlx::raw_sql(AssertSqlSafe(format!("SET search_path TO {path}"))).execute(&mut *session.conn).await.is_ok() {
                    session.search_path = Some(schema.to_string());
                }
            }
        }
        let max_rows = opts.max_rows.filter(|m| *m > 0).unwrap_or(DEFAULT_MAX_ROWS);
        let mut out = Vec::with_capacity(stmts.len());
        for (i, st) in stmts.iter().enumerate() {
            if self.cancel_gen.load(Ordering::SeqCst) != cancel_gen {
                return Err((i, sqlx::Error::Protocol("canceling statement due to user request".into())));
            }
            match self.run_statement(&mut session.conn, &st.sql, max_rows, Some((db, session.pid))).await {
                Ok(r) => out.push(r),
                Err(e) => return Err((i, e)),
            }
        }
        Ok(out)
    }

    async fn primary_key(&self, pool: &PgPool, schema: &str, table: &str) -> AppResult<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT a.attname::text FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum, ord) ON true \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE i.indisprimary AND n.nspname = $1 AND c.relname = $2 ORDER BY k.ord",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?)
    }

    fn schema_of<'a>(&self, t: &'a TableRef) -> &'a str {
        t.schema.as_deref().filter(|s| !s.is_empty()).unwrap_or("public")
    }

    /// Bulk structure query for one table (`table = Some`) or all tables of a schema.
    async fn structures(&self, pool: &PgPool, db: &str, schema: Option<&str>, table: Option<&str>) -> AppResult<Vec<TableStructure>> {
        let schema_filter = "(($1::text IS NULL AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%') OR n.nspname = $1)";
        let identity = if self.version_num >= 100000 { "a.attidentity::text" } else { "''::text" };
        let generated = if self.version_num >= 120000 { "a.attgenerated::text" } else { "''::text" };
        let nkey = if self.version_num >= 110000 { "i.indnkeyatts" } else { "i.indnatts" };

        // Tables (with comments) define the result order.
        let tables: Vec<(String, String, Option<String>)> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT n.nspname::text, c.relname::text, obj_description(c.oid, 'pg_class') \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE {schema_filter} AND ($2::text IS NULL OR c.relname = $2) \
             AND (c.relkind IN ('r', 'p', 'f') OR ($2::text IS NOT NULL AND c.relkind IN ('v', 'm'))) \
             ORDER BY 1, 2"
        )))
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?;

        let mut out: Vec<TableStructure> = Vec::with_capacity(tables.len());
        let mut index: HashMap<(String, String), usize> = HashMap::new();
        for (s, t, comment) in tables {
            index.insert((s.clone(), t.clone()), out.len());
            out.push(TableStructure {
                table: Some(TableRef { database: Some(db.to_string()), schema: Some(s), name: t }),
                comment,
                ..Default::default()
            });
        }
        if out.is_empty() {
            return Ok(out);
        }

        type ColRow = (String, String, String, String, bool, Option<String>, String, String, Option<String>);
        let cols: Vec<ColRow> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT n.nspname::text, c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
                    pg_get_expr(d.adbin, d.adrelid), {identity}, {generated}, col_description(c.oid, a.attnum) \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE {schema_filter} AND ($2::text IS NULL OR c.relname = $2) \
             AND c.relkind IN ('r', 'p', 'f', 'v', 'm') AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY 1, 2, a.attnum"
        )))
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?;
        for (s, t, name, ty, notnull, default, ident, gen_, comment) in cols {
            let Some(&ix) = index.get(&(s, t)) else { continue };
            let ts = &mut out[ix];
            let serial = default.as_deref().is_some_and(|d| d.starts_with("nextval("));
            let default = match (ident.as_str(), gen_.as_str()) {
                ("a", _) => Some("GENERATED ALWAYS AS IDENTITY".to_string()),
                ("d", _) => Some("GENERATED BY DEFAULT AS IDENTITY".to_string()),
                (_, "s") => default.map(|e| format!("GENERATED ALWAYS AS ({e}) STORED")),
                (_, "v") => default.map(|e| format!("GENERATED ALWAYS AS ({e}) VIRTUAL")),
                _ => default,
            };
            ts.columns.push(ColumnDef {
                name,
                data_type: ty,
                nullable: !notnull,
                default,
                is_primary_key: false,
                auto_increment: serial || ident == "a" || ident == "d",
                comment,
                ordinal: ts.columns.len() as u32 + 1,
            });
        }

        type IdxRow = (String, String, String, bool, bool, String, Vec<String>);
        let idx: Vec<IdxRow> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT n.nspname::text, t.relname::text, ic.relname::text, i.indisunique, i.indisprimary, am.amname::text, \
                    ARRAY(SELECT CASE WHEN i.indkey[k] = 0 THEN pg_get_indexdef(i.indexrelid, k + 1, true) \
                                      ELSE (SELECT attname::text FROM pg_attribute WHERE attrelid = i.indrelid AND attnum = i.indkey[k]) END \
                          FROM generate_series(0, {nkey} - 1) k ORDER BY k)::text[] \
             FROM pg_index i \
             JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_class t ON t.oid = i.indrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace JOIN pg_am am ON am.oid = ic.relam \
             WHERE {schema_filter} AND ($2::text IS NULL OR t.relname = $2) \
             ORDER BY 1, 2, i.indisprimary DESC, 3"
        )))
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?;
        for (s, t, name, unique, primary, method, columns) in idx {
            let Some(&ix) = index.get(&(s, t)) else { continue };
            let ts = &mut out[ix];
            if primary {
                for c in ts.columns.iter_mut() {
                    if columns.contains(&c.name) {
                        c.is_primary_key = true;
                    }
                }
            }
            ts.indexes.push(IndexDef { name, columns, unique, primary, method: Some(method) });
        }

        type FkRow = (String, String, String, Vec<String>, String, String, Vec<String>, String, String);
        let fks: Vec<FkRow> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT n.nspname::text, c.relname::text, con.conname::text, \
                    ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord) \
                          JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum ORDER BY k.ord)::text[], \
                    rn.nspname::text, rc.relname::text, \
                    ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord) \
                          JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum ORDER BY k.ord)::text[], \
                    con.confupdtype::text, con.confdeltype::text \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_class rc ON rc.oid = con.confrelid JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
             WHERE con.contype = 'f' AND {schema_filter} AND ($2::text IS NULL OR c.relname = $2) \
             ORDER BY 1, 2, 3"
        )))
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?;
        for (s, t, name, columns, rs, rt, ref_columns, upd, del) in fks {
            let Some(&ix) = index.get(&(s, t)) else { continue };
            out[ix].foreign_keys.push(ForeignKeyDef {
                name,
                columns,
                ref_schema: Some(rs),
                ref_table: rt,
                ref_columns,
                on_update: Some(fk_action(&upd).into()),
                on_delete: Some(fk_action(&del).into()),
            });
        }
        Ok(out)
    }

    /// `CREATE SEQUENCE` for `qn` plus, if the sequence is owned by a column,
    /// `(owner table oid, "ALTER SEQUENCE ... OWNED BY ...;")`.
    async fn sequence_ddl(&self, pool: &PgPool, qn: &str, if_not_exists: bool) -> AppResult<(String, Option<(i64, String)>)> {
        let (ty, start, inc, min, max, cache, cycle): (String, i64, i64, i64, i64, i64, bool) = sqlx::query_as(
            "SELECT format_type(seqtypid, NULL), seqstart, seqincrement, seqmin, seqmax, seqcache, seqcycle \
             FROM pg_sequence WHERE seqrelid = $1::regclass",
        )
        .bind(qn)
        .fetch_one(pool)
        .await?;
        let owner: Option<(i64, String)> = sqlx::query_as(
            "SELECT t.oid::int8, quote_ident(tn.nspname) || '.' || quote_ident(t.relname) || '.' || quote_ident(a.attname) \
             FROM pg_depend d JOIN pg_class t ON t.oid = d.refobjid JOIN pg_namespace tn ON tn.oid = t.relnamespace \
             JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid \
             WHERE d.objid = $1::regclass AND d.classid = 'pg_class'::regclass AND d.refclassid = 'pg_class'::regclass \
             AND d.deptype = 'a' LIMIT 1",
        )
        .bind(qn)
        .fetch_optional(pool)
        .await?;
        let create = format!(
            "CREATE SEQUENCE {}{qn}\n    AS {ty}\n    INCREMENT BY {inc}\n    MINVALUE {min}\n    MAXVALUE {max}\n    START WITH {start}\n    CACHE {cache}{};\n",
            if if_not_exists { "IF NOT EXISTS " } else { "" },
            if cycle { "\n    CYCLE" } else { "" }
        );
        Ok((create, owner.map(|(oid, col)| (oid, format!("ALTER SEQUENCE {qn} OWNED BY {col};\n")))))
    }

    async fn table_ddl(&self, pool: &PgPool, db: &str, schema: &str, name: &str) -> AppResult<String> {
        let qn = format!("{}.{}", sc::quote_ident(D, schema), sc::quote_ident(D, name));
        let st = self
            .structures(pool, db, Some(schema), Some(name))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::msg(format!("Table {qn} not found")))?;
        let (relkind, partkey): (String, Option<String>) = sqlx::query_as(if self.version_num >= 100000 {
            "SELECT relkind::text, CASE WHEN relkind = 'p' THEN pg_get_partkeydef(oid) END FROM pg_class WHERE oid = $1::regclass"
        } else {
            "SELECT relkind::text, NULL::text FROM pg_class WHERE oid = $1::regclass"
        })
        .bind(&qn)
        .fetch_one(pool)
        .await?;

        let mut lines: Vec<String> = st
            .columns
            .iter()
            .map(|c| {
                let mut l = format!("    {} {}", sc::quote_ident(D, &c.name), c.data_type);
                if let Some(d) = &c.default {
                    if d.starts_with("GENERATED ") {
                        l.push(' ');
                        l.push_str(d);
                    } else {
                        l.push_str(" DEFAULT ");
                        l.push_str(d);
                    }
                }
                if !c.nullable {
                    l.push_str(" NOT NULL");
                }
                l
            })
            .collect();
        let cons: Vec<(String, String)> = sqlx::query_as(
            "SELECT conname::text, pg_get_constraintdef(oid, true) FROM pg_constraint \
             WHERE conrelid = $1::regclass AND contype IN ('p', 'u', 'c', 'f', 'x') \
             ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'c' THEN 2 WHEN 'f' THEN 3 ELSE 4 END, conname",
        )
        .bind(&qn)
        .fetch_all(pool)
        .await?;
        for (cname, def) in cons {
            lines.push(format!("    CONSTRAINT {} {}", sc::quote_ident(D, &cname), def));
        }
        // Sequences used by column defaults (serial columns, nextval(...)) so the DDL is self-contained.
        let (table_oid, seqs): (i64, Vec<String>) = (
            sqlx::query_scalar("SELECT $1::regclass::oid::int8").bind(&qn).fetch_one(pool).await?,
            sqlx::query_scalar(
                "SELECT DISTINCT quote_ident(n.nspname) || '.' || quote_ident(c.relname) \
                 FROM pg_attrdef ad \
                 JOIN pg_depend d ON d.classid = 'pg_attrdef'::regclass AND d.objid = ad.oid AND d.refclassid = 'pg_class'::regclass \
                 JOIN pg_class c ON c.oid = d.refobjid AND c.relkind = 'S' JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE ad.adrelid = $1::regclass ORDER BY 1",
            )
            .bind(&qn)
            .fetch_all(pool)
            .await?,
        );
        let mut ddl = String::new();
        let mut owned_by = String::new();
        for seq in &seqs {
            let (create, owner) = self.sequence_ddl(pool, seq, true).await?;
            ddl.push_str(&create);
            if let Some((_, stmt)) = owner.filter(|(oid, _)| *oid == table_oid) {
                owned_by.push_str(&stmt);
            }
        }
        if !ddl.is_empty() {
            ddl.push('\n');
        }
        ddl.push_str(&format!("CREATE {}TABLE {qn} (\n{}\n)", if relkind == "f" { "FOREIGN " } else { "" }, lines.join(",\n")));
        if let Some(pk) = partkey {
            ddl.push_str(&format!(" PARTITION BY {pk}"));
        }
        ddl.push_str(";\n");
        if !owned_by.is_empty() {
            ddl.push('\n');
            ddl.push_str(&owned_by);
        }

        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i WHERE i.indrelid = $1::regclass \
             AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid AND c.conrelid = i.indrelid AND c.contype IN ('p', 'u', 'x')) \
             ORDER BY 1",
        )
        .bind(&qn)
        .fetch_all(pool)
        .await?;
        if !indexes.is_empty() {
            ddl.push('\n');
            for i in indexes {
                ddl.push_str(&i);
                ddl.push_str(";\n");
            }
        }
        let triggers: Vec<String> = sqlx::query_scalar(
            "SELECT pg_get_triggerdef(oid, true) FROM pg_trigger WHERE tgrelid = $1::regclass AND NOT tgisinternal ORDER BY tgname",
        )
        .bind(&qn)
        .fetch_all(pool)
        .await?;
        if !triggers.is_empty() {
            ddl.push('\n');
            for t in triggers {
                ddl.push_str(&t);
                ddl.push_str(";\n");
            }
        }
        let mut comments = String::new();
        if let Some(c) = &st.comment {
            comments.push_str(&format!("COMMENT ON TABLE {qn} IS {};\n", sc::string_literal(D, c)));
        }
        for col in &st.columns {
            if let Some(c) = &col.comment {
                comments.push_str(&format!("COMMENT ON COLUMN {qn}.{} IS {};\n", sc::quote_ident(D, &col.name), sc::string_literal(D, c)));
            }
        }
        if !comments.is_empty() {
            ddl.push('\n');
            ddl.push_str(&comments);
        }
        Ok(ddl)
    }
}

fn fk_action(c: &str) -> &'static str {
    match c {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
}

fn is_query_canceled(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("57014"))
}

fn is_connection_error(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed)
        || matches!(e, sqlx::Error::Protocol(m) if !m.contains("user request"))
}

/// Human readable Postgres error including detail/hint and the error position.
/// `script`/`offset` locate the failed statement (byte offset) so positions are
/// reported relative to the whole script.
fn pg_error_text(e: &sqlx::Error, script: &str, offset: usize) -> String {
    if let sqlx::Error::Database(db) = e {
        if let Some(pg) = db.try_downcast_ref::<PgDatabaseError>() {
            let mut msg = format!("ERROR: {}", pg.message());
            if let Some(PgErrorPosition::Original(p)) = pg.position() {
                let before = script.get(..offset).map(|s| s.chars().count()).unwrap_or(0);
                msg.push_str(&format!(" (at {})", sc::describe_position(script, before + p)));
            }
            if let Some(d) = pg.detail() {
                msg.push_str(&format!("\nDETAIL: {d}"));
            }
            if let Some(h) = pg.hint() {
                msg.push_str(&format!("\nHINT: {h}"));
            }
            if let Some(PgErrorPosition::Internal { position, query }) = pg.position() {
                msg.push_str(&format!("\nQUERY: {query} (position {position})"));
            }
            if let Some(w) = pg.r#where() {
                msg.push_str(&format!("\nCONTEXT: {w}"));
            }
            msg.push_str(&format!("\nSQLSTATE: {}", pg.code()));
            return msg;
        }
    }
    if let sqlx::Error::Protocol(m) = e {
        if m.contains("user request") {
            return "Query cancelled".into();
        }
    }
    e.to_string()
}

// ---------------------------------------------------------------------------
// Value conversion (text protocol)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Int,
    Float,
    Bool,
    Json,
    Bytea,
    /// timestamptz / timetz: offsets normalised to `+HH:MM`
    TimeTz,
    /// numeric, money, dates, uuid, inet, geometry, enums, ... => string
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Scalar(Scalar),
    /// Array with the element kind; `delim` is the element delimiter (`;` for box).
    Array(Scalar, char),
}

impl ValueKind {
    fn of(ti: &PgTypeInfo) -> Self {
        if ti.name() == "?" {
            return ValueKind::Scalar(Scalar::Text);
        }
        match ti.kind() {
            PgTypeKind::Array(elem) => {
                let (s, delim) = scalar_of(elem);
                ValueKind::Array(s, delim)
            }
            _ => {
                let name = ti.name();
                if let Some(elem) = name.strip_suffix("[]") {
                    ValueKind::Array(scalar_from_name(elem), if elem.eq_ignore_ascii_case("box") { ';' } else { ',' })
                } else {
                    ValueKind::Scalar(scalar_of(ti).0)
                }
            }
        }
    }
}

fn scalar_of(ti: &PgTypeInfo) -> (Scalar, char) {
    if ti.name() == "?" {
        return (Scalar::Text, ',');
    }
    match ti.kind() {
        PgTypeKind::Domain(base) => scalar_of(base),
        _ => {
            let n = ti.name();
            (scalar_from_name(n), if n.eq_ignore_ascii_case("box") { ';' } else { ',' })
        }
    }
}

fn scalar_from_name(name: &str) -> Scalar {
    match name.to_ascii_uppercase().as_str() {
        "INT2" | "INT4" | "INT8" | "OID" | "SMALLINT" | "INTEGER" | "BIGINT" | "XID" | "CID" | "REGPROC" => Scalar::Int,
        "FLOAT4" | "FLOAT8" | "REAL" | "DOUBLE PRECISION" => Scalar::Float,
        "BOOL" | "BOOLEAN" => Scalar::Bool,
        "JSON" | "JSONB" => Scalar::Json,
        "BYTEA" => Scalar::Bytea,
        "TIMESTAMPTZ" | "TIMETZ" | "TIMESTAMP WITH TIME ZONE" | "TIME WITH TIME ZONE" => Scalar::TimeTz,
        _ => Scalar::Text,
    }
}

/// A cell as received from the server, before type-aware conversion.
enum Cell {
    Null,
    Text(String),
    Binary(Vec<u8>),
}

fn raw_row(row: &PgRow) -> Vec<Cell> {
    (0..row.len())
        .map(|i| match row.try_get_raw(i) {
            Ok(v) if !v.is_null() => match v.format() {
                PgValueFormat::Text => match v.as_str() {
                    Ok(s) => Cell::Text(s.to_string()),
                    Err(_) => Cell::Binary(v.as_bytes().map(<[u8]>::to_vec).unwrap_or_default()),
                },
                PgValueFormat::Binary => Cell::Binary(v.as_bytes().map(<[u8]>::to_vec).unwrap_or_default()),
            },
            _ => Cell::Null,
        })
        .collect()
}

fn convert_row(cells: Vec<Cell>, kinds: &[ValueKind]) -> Vec<Value> {
    cells
        .into_iter()
        .enumerate()
        .map(|(i, c)| match c {
            Cell::Null => Value::Null,
            Cell::Binary(b) => sc::binary_to_value(&b),
            Cell::Text(s) => match kinds.get(i).copied().unwrap_or(ValueKind::Scalar(Scalar::Text)) {
                ValueKind::Scalar(k) => text_to_json(k, &s),
                ValueKind::Array(k, delim) => parse_pg_array(&s, k, delim).unwrap_or(Value::String(s)),
            },
        })
        .collect()
}

/// Convert the Postgres text representation of a value to JSON.
fn text_to_json(kind: Scalar, s: &str) -> Value {
    match kind {
        Scalar::Int => match s.parse::<i64>() {
            Ok(n) if n.abs() <= JS_MAX_SAFE_INT => Value::from(n),
            _ => Value::String(s.to_string()),
        },
        Scalar::Float => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(s.to_string())),
        Scalar::Bool => match s {
            "t" | "true" => Value::Bool(true),
            "f" | "false" => Value::Bool(false),
            _ => Value::String(s.to_string()),
        },
        Scalar::Json => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string())),
        Scalar::Bytea => {
            // Text form is already "\x<hex>"; just cap the size.
            let max = 2 + sc::MAX_BINARY_DISPLAY * 2;
            if s.len() > max && s.is_char_boundary(max) {
                Value::String(format!("{}…", &s[..max]))
            } else {
                Value::String(s.to_string())
            }
        }
        Scalar::TimeTz => Value::String(full_offset(s)),
        Scalar::Text => Value::String(s.to_string()),
    }
}

/// Postgres prints whole-hour offsets as `+00`; ISO 8601 consumers prefer `+00:00`.
fn full_offset(s: &str) -> String {
    let b = s.as_bytes();
    let n = b.len();
    if n >= 4 && matches!(b[n - 3], b'+' | b'-') && b[n - 2].is_ascii_digit() && b[n - 1].is_ascii_digit() && b[n - 4].is_ascii_digit() {
        format!("{s}:00")
    } else {
        s.to_string()
    }
}

/// Parse a Postgres array literal (`{1,2,"a b",NULL,{3}}`) into JSON.
/// Returns `None` for forms we don't handle (explicit bounds etc.).
fn parse_pg_array(s: &str, elem: Scalar, delim: char) -> Option<Value> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let v = parse_array_level(&chars, &mut i, elem, delim)?;
    if i == chars.len() { Some(v) } else { None }
}

fn parse_array_level(c: &[char], i: &mut usize, elem: Scalar, delim: char) -> Option<Value> {
    if c.get(*i) != Some(&'{') {
        return None;
    }
    *i += 1;
    let mut items = Vec::new();
    if c.get(*i) == Some(&'}') {
        *i += 1;
        return Some(Value::Array(items));
    }
    loop {
        match c.get(*i)? {
            '{' => items.push(parse_array_level(c, i, elem, delim)?),
            '"' => {
                *i += 1;
                let mut s = String::new();
                loop {
                    match c.get(*i)? {
                        '\\' => {
                            s.push(*c.get(*i + 1)?);
                            *i += 2;
                        }
                        '"' => {
                            *i += 1;
                            break;
                        }
                        ch => {
                            s.push(*ch);
                            *i += 1;
                        }
                    }
                }
                items.push(text_to_json(elem, &s));
            }
            _ => {
                let st = *i;
                while let Some(&ch) = c.get(*i) {
                    if ch == delim || ch == '}' {
                        break;
                    }
                    *i += 1;
                }
                let raw: String = c[st..*i].iter().collect();
                let raw = raw.trim();
                items.push(if raw.eq_ignore_ascii_case("NULL") { Value::Null } else { text_to_json(elem, raw) });
            }
        }
        match c.get(*i)? {
            ch if *ch == delim => *i += 1,
            '}' => {
                *i += 1;
                return Some(Value::Array(items));
            }
            _ => return None,
        }
    }
}

/// JSON value -> text parameter for a column of type `ty` (bound as text and cast server-side).
fn json_to_param(v: &Value, ty: Option<&str>) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(_) if ty.is_some_and(|t| t.ends_with("[]")) => Some(json_to_pg_array(v)),
        other => Some(other.to_string()),
    }
}

fn json_to_pg_array(v: &Value) -> String {
    match v {
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|x| match x {
                    Value::Null => "NULL".to_string(),
                    Value::Array(_) => json_to_pg_array(x),
                    Value::String(s) => quote_array_elem(s),
                    other => quote_array_elem(&other.to_string()),
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn quote_array_elem(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ---------------------------------------------------------------------------
// Tree helpers
// ---------------------------------------------------------------------------

fn path_of(base: &[String], extra: &str) -> Vec<String> {
    let mut p = base.to_vec();
    p.push(extra.to_string());
    p
}

fn folder(base: &[String], id: &str, label: &str) -> TreeNode {
    TreeNode { path: path_of(base, id), label: label.into(), kind: NodeKind::Folder, has_children: true, detail: None, table: None }
}

fn leaf(base: &[String], name: &str, kind: NodeKind, detail: Option<String>, table: Option<TableRef>) -> TreeNode {
    TreeNode { path: path_of(base, name), label: name.into(), kind, has_children: false, detail, table }
}

fn tref(db: &str, schema: &str, name: &str) -> TableRef {
    TableRef { database: Some(db.into()), schema: Some(schema.into()), name: name.into() }
}

fn format_count(n: f64) -> String {
    let n = n.max(0.0) as u64;
    if n == 1 { "1 row".into() } else { format!("{n} rows") }
}

#[async_trait]
impl Driver for PostgresDriver {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Postgres
    }
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    async fn server_version(&self) -> AppResult<String> {
        let pool = self.pool(&self.default_db).await?;
        let v: String = sqlx::query_scalar("SHOW server_version").fetch_one(&pool).await?;
        Ok(format!("PostgreSQL {}", v.split_whitespace().next().unwrap_or(&v)))
    }

    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>> {
        match path {
            [] => Ok(self
                .list_databases()
                .await?
                .into_iter()
                .map(|db| TreeNode {
                    path: vec![db.clone()],
                    label: db,
                    kind: NodeKind::Database,
                    has_children: true,
                    detail: None,
                    table: None,
                })
                .collect()),
            [db] => {
                let pool = self.pool(db).await?;
                let schemas: Vec<String> = sqlx::query_scalar(
                    "SELECT nspname::text FROM pg_namespace \
                     WHERE nspname NOT LIKE 'pg\\_toast%' AND nspname NOT LIKE 'pg\\_temp\\_%' \
                     ORDER BY nspname IN ('pg_catalog', 'information_schema'), nspname",
                )
                .fetch_all(&pool)
                .await?;
                Ok(schemas
                    .into_iter()
                    .map(|s| TreeNode {
                        path: path_of(path, &s),
                        detail: SYSTEM_SCHEMAS.contains(&s.as_str()).then(|| "system".to_string()),
                        label: s,
                        kind: NodeKind::Schema,
                        has_children: true,
                        table: None,
                    })
                    .collect())
            }
            [_, _] => Ok(vec![
                folder(path, "tables", "Tables"),
                folder(path, "views", "Views"),
                folder(path, "materialized_views", "Materialized Views"),
                folder(path, "functions", "Functions"),
                folder(path, "sequences", "Sequences"),
            ]),
            [db, schema, f] => {
                let pool = self.pool(db).await?;
                match f.as_str() {
                    "tables" => {
                        let rows: Vec<(String, f64, Option<String>)> = sqlx::query_as(
                            "SELECT c.relname::text, c.reltuples::float8, obj_description(c.oid, 'pg_class') \
                             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'f') ORDER BY 1",
                        )
                        .bind(schema)
                        .fetch_all(&pool)
                        .await?;
                        Ok(rows
                            .into_iter()
                            .map(|(name, est, _comment)| TreeNode {
                                path: path_of(path, &name),
                                label: name.clone(),
                                kind: NodeKind::Table,
                                has_children: true,
                                detail: (est >= 0.0).then(|| format_count(est)),
                                table: Some(tref(db, schema, &name)),
                            })
                            .collect())
                    }
                    "views" | "materialized_views" | "sequences" => {
                        let (relkind, kind) = match f.as_str() {
                            "views" => ("v", NodeKind::View),
                            "materialized_views" => ("m", NodeKind::MaterializedView),
                            _ => ("S", NodeKind::Sequence),
                        };
                        let names: Vec<String> = sqlx::query_scalar(
                            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                             WHERE n.nspname = $1 AND c.relkind::text = $2 ORDER BY 1",
                        )
                        .bind(schema)
                        .bind(relkind)
                        .fetch_all(&pool)
                        .await?;
                        Ok(names.iter().map(|n| leaf(path, n, kind.clone(), None, Some(tref(db, schema, n)))).collect())
                    }
                    "functions" => {
                        let prokind = if self.version_num >= 110000 { "p.prokind::text" } else { "CASE WHEN p.proisagg THEN 'a' ELSE 'f' END" };
                        let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(AssertSqlSafe(format!(
                            "SELECT p.proname::text, p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')', {prokind}, \
                                    pg_get_function_result(p.oid) \
                             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 ORDER BY 1, 2"
                        )))
                        .bind(schema)
                        .fetch_all(&pool)
                        .await?;
                        Ok(rows
                            .into_iter()
                            .map(|(_, sig, k, result)| {
                                let kind = if k == "p" { NodeKind::Procedure } else { NodeKind::Function };
                                leaf(path, &sig, kind, result, Some(tref(db, schema, &sig)))
                            })
                            .collect())
                    }
                    other => Err(AppError::msg(format!("Unknown folder \"{other}\""))),
                }
            }
            [_, _, f, _] if f == "tables" => Ok(vec![
                folder(path, "columns", "Columns"),
                folder(path, "indexes", "Indexes"),
                folder(path, "foreign_keys", "Foreign Keys"),
                folder(path, "triggers", "Triggers"),
            ]),
            [db, schema, f, table, sub] if f == "tables" => {
                let pool = self.pool(db).await?;
                if sub == "triggers" {
                    let names: Vec<String> = sqlx::query_scalar(
                        "SELECT t.tgname::text FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
                         JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = $1 AND c.relname = $2 AND NOT t.tgisinternal ORDER BY 1",
                    )
                    .bind(schema)
                    .bind(table)
                    .fetch_all(&pool)
                    .await?;
                    return Ok(names.iter().map(|n| leaf(path, n, NodeKind::Trigger, None, Some(tref(db, schema, n)))).collect());
                }
                let st = self.structures(&pool, db, Some(schema), Some(table)).await?.into_iter().next().unwrap_or_default();
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
                                "{}({}){}",
                                if i.primary { "PRIMARY " } else if i.unique { "UNIQUE " } else { "" },
                                i.columns.join(", "),
                                i.method.as_deref().filter(|m| *m != "btree").map(|m| format!(" {m}")).unwrap_or_default()
                            );
                            leaf(path, &i.name, NodeKind::Index, Some(d), Some(tref(db, schema, &i.name)))
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
        let pool = self.pool(&self.default_db).await?;
        Ok(sqlx::query_scalar("SELECT datname::text FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1")
            .fetch_all(&pool)
            .await?)
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
        let db = self.db(opts.database.as_deref()).to_string();
        let cancel_gen = self.cancel_gen.load(Ordering::SeqCst);
        let mut attempt = 0;
        loop {
            let mut lease = self.lease(&db).await?;
            let id = self.next_run.fetch_add(1, Ordering::SeqCst);
            self.running.lock().insert(id, (db.clone(), lease.session().pid));
            let _guard = RunningGuard { map: &self.running, id };
            match self.run_script(&db, &mut lease, &stmts, opts, cancel_gen).await {
                Ok(results) => return Ok(results),
                Err((i, e)) => {
                    if is_connection_error(&e) {
                        lease.discard();
                        // A stale sticky connection (server restart, idle timeout): retry once
                        // if nothing has run yet.
                        if i == 0 && attempt == 0 {
                            attempt += 1;
                            continue;
                        }
                    }
                    let st = &stmts[i];
                    return Err(sc::statement_error(i + 1, stmts.len(), st.line(script), &st.sql, pg_error_text(&e, script, st.offset)));
                }
            }
        }
    }

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage> {
        let db = self.db(req.table.database.as_deref()).to_string();
        let schema = self.schema_of(&req.table).to_string();
        let table = TableRef { database: None, schema: Some(schema.clone()), name: req.table.name.clone() };
        let filter = req.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
        if let Some(f) = filter {
            if sc::split_statements(D, f).len() > 1 {
                return Err(AppError::msg("The filter must be a single SQL expression"));
            }
        }
        let pool = self.pool(&db).await?;
        let pk = self.primary_key(&pool, &schema, &req.table.name).await?;
        let sql = sc::select_page_sql(D, &table, filter, &req.sort, &pk, req.limit.max(1), req.offset);
        let mut conn = pool.acquire().await?;
        let result = self.run_statement(&mut conn, &sql, usize::MAX, None).await.map_err(|e| AppError::msg(pg_error_text(&e, &sql, 0)))?;
        let estimate: Option<f64> = sqlx::query_scalar(
            "SELECT c.reltuples::float8 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(&schema)
        .bind(&req.table.name)
        .fetch_optional(&mut *conn)
        .await?;
        // Exact counts on huge tables are slow; use the planner estimate there.
        let total = match estimate {
            Some(e) if filter.is_none() && e > 1_000_000.0 => Some(e as u64),
            _ => {
                let count: i64 = sqlx::query_scalar(AssertSqlSafe(sc::count_sql(D, &table, filter))).fetch_one(&mut *conn).await?;
                Some(count.max(0) as u64)
            }
        };
        Ok(TableDataPage { result, total, primary_key: pk })
    }

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure> {
        let db = self.db(table.database.as_deref()).to_string();
        let pool = self.pool(&db).await?;
        let schema = self.schema_of(table).to_string();
        self.structures(&pool, &db, Some(&schema), Some(&table.name))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::msg(format!("Table \"{schema}\".\"{}\" not found", table.name)))
    }

    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64> {
        if self.read_only {
            return Err(AppError::msg("This connection is read-only"));
        }
        if changes.is_empty() {
            return Ok(0);
        }
        let db = self.db(table.database.as_deref()).to_string();
        let schema = self.schema_of(table).to_string();
        let t = TableRef { database: None, schema: Some(schema.clone()), name: table.name.clone() };
        let pool = self.pool(&db).await?;
        let types: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) FROM pg_attribute a \
             WHERE a.attrelid = $1::regclass AND a.attnum > 0 AND NOT a.attisdropped",
        )
        .bind(sc::qualified_name(D, &t))
        .fetch_all(&pool)
        .await?
        .into_iter()
        .collect();
        let mut tx = pool.begin().await?;
        let mut total = 0u64;
        for (n, change) in changes.iter().enumerate() {
            let stmt = sc::build_change(D, &t, change, &|col, p| match types.get(col) {
                Some(ty) => format!("CAST({p} AS {ty})"),
                None => p,
            })?;
            let mut q = sqlx::query(AssertSqlSafe(stmt.sql.clone()));
            for (v, col) in stmt.params.iter().zip(&stmt.param_columns) {
                q = q.bind(json_to_param(v, types.get(col).map(String::as_str)));
            }
            let affected = match q.execute(&mut *tx).await {
                Ok(r) => r.rows_affected(),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(AppError::msg(format!("Change {}: {}", n + 1, pg_error_text(&e, &stmt.sql, 0))));
                }
            };
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
        let db = self.db(table.database.as_deref()).to_string();
        let pool = self.pool(&db).await?;
        let schema = self.schema_of(table).to_string();
        let qn = format!("{}.{}", sc::quote_ident(D, &schema), sc::quote_ident(D, &table.name));
        match kind {
            NodeKind::Table => self.table_ddl(&pool, &db, &schema, &table.name).await,
            NodeKind::View | NodeKind::MaterializedView => {
                let (relkind, def): (String, String) =
                    sqlx::query_as("SELECT relkind::text, pg_get_viewdef(oid, true) FROM pg_class WHERE oid = $1::regclass")
                        .bind(&qn)
                        .fetch_one(&pool)
                        .await?;
                let def = def.trim().trim_end_matches(';');
                if relkind == "m" {
                    let mut ddl = format!("CREATE MATERIALIZED VIEW {qn} AS\n{def}\nWITH DATA;\n");
                    let idx: Vec<String> = sqlx::query_scalar("SELECT pg_get_indexdef(indexrelid) FROM pg_index WHERE indrelid = $1::regclass ORDER BY 1")
                        .bind(&qn)
                        .fetch_all(&pool)
                        .await?;
                    for i in idx {
                        ddl.push_str(&format!("\n{i};"));
                    }
                    Ok(ddl)
                } else {
                    Ok(format!("CREATE OR REPLACE VIEW {qn} AS\n{def};\n"))
                }
            }
            NodeKind::Function | NodeKind::Procedure => {
                let rows: Vec<(String, String)> = sqlx::query_as(
                    "SELECT p.oid::regprocedure::text, CASE WHEN p.prokind = 'a' THEN '-- aggregate function: ' || p.oid::regprocedure::text \
                                                           ELSE pg_get_functiondef(p.oid) END \
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname = $1 AND (p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2 OR p.proname = $2) \
                     ORDER BY 1",
                )
                .bind(&schema)
                .bind(&table.name)
                .fetch_all(&pool)
                .await?;
                if rows.is_empty() {
                    return Err(AppError::msg(format!("Function {} not found", table.name)));
                }
                Ok(rows.into_iter().map(|(_, d)| format!("{};\n", d.trim_end())).collect::<Vec<_>>().join("\n"))
            }
            NodeKind::Sequence => {
                let (create, owner) = self.sequence_ddl(&pool, &qn, false).await?;
                Ok(match owner {
                    Some((_, owned_by)) => format!("{create}\n{owned_by}"),
                    None => create,
                })
            }
            NodeKind::Trigger => {
                let defs: Vec<String> = sqlx::query_scalar(
                    "SELECT pg_get_triggerdef(t.oid, true) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND t.tgname = $2 AND NOT t.tgisinternal ORDER BY 1",
                )
                .bind(&schema)
                .bind(&table.name)
                .fetch_all(&pool)
                .await?;
                if defs.is_empty() {
                    return Err(AppError::msg(format!("Trigger {} not found", table.name)));
                }
                Ok(defs.into_iter().map(|d| format!("{d};\n")).collect())
            }
            NodeKind::Index => {
                let def: String = sqlx::query_scalar("SELECT pg_get_indexdef($1::regclass)").bind(&qn).fetch_one(&pool).await?;
                Ok(format!("{def};\n"))
            }
            NodeKind::Schema => Ok(format!("CREATE SCHEMA {};\n", sc::quote_ident(D, &table.name))),
            NodeKind::Database => Ok(format!("CREATE DATABASE {};\n", sc::quote_ident(D, &table.name))),
            other => Err(AppError::Unsupported(format!("DDL for {other:?}"))),
        }
    }

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot> {
        let db = self.db(database).to_string();
        let pool = self.pool(&db).await?;
        let schema = schema.filter(|s| !s.is_empty());
        let tables = self.structures(&pool, &db, schema, None).await?;
        let views: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT n.nspname::text, c.relname::text, pg_get_viewdef(c.oid, true) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('v', 'm') \
             AND (($1::text IS NULL AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%') OR n.nspname = $1) \
             ORDER BY 1, 2",
        )
        .bind(schema)
        .fetch_all(&pool)
        .await?;
        Ok(SchemaSnapshot {
            tables,
            views: views.into_iter().map(|(s, n, def)| (tref(&db, &s, &n), def.trim().to_string())).collect(),
        })
    }

    async fn cancel(&self) -> AppResult<()> {
        self.cancel_gen.fetch_add(1, Ordering::SeqCst);
        let running: Vec<(String, i32)> = self.running.lock().values().cloned().collect();
        for (db, pid) in running {
            let pool = self.pool(&db).await?;
            let _: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)").bind(pid).fetch_one(&pool).await?;
        }
        Ok(())
    }

    async fn close(&self) {
        let slots: Vec<_> = self.sessions.lock().drain().map(|(_, s)| s).collect();
        for slot in slots {
            if let Ok(mut g) = slot.try_lock() {
                g.take();
            }
        }
        let pools: Vec<PgPool> = self.pools.write().await.drain().map(|(_, p)| p).collect();
        for p in pools {
            let _ = tokio::time::timeout(Duration::from_secs(3), p.close()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn array_parsing() {
        assert_eq!(parse_pg_array("{1,2,NULL}", Scalar::Int, ','), Some(json!([1, 2, null])));
        assert_eq!(parse_pg_array("{}", Scalar::Int, ','), Some(json!([])));
        assert_eq!(parse_pg_array(r#"{"a b","c\"d",e,"NULL"}"#, Scalar::Text, ','), Some(json!(["a b", "c\"d", "e", "NULL"])));
        assert_eq!(parse_pg_array("{{1,2},{3,4}}", Scalar::Int, ','), Some(json!([[1, 2], [3, 4]])));
        assert_eq!(parse_pg_array("{t,f}", Scalar::Bool, ','), Some(json!([true, false])));
        assert_eq!(parse_pg_array("[0:1]={1,2}", Scalar::Int, ','), None);
        assert_eq!(parse_pg_array("{(1,1),(0,0);(2,2),(1,1)}", Scalar::Text, ';'), Some(json!(["(1,1),(0,0)", "(2,2),(1,1)"])));
    }

    #[test]
    fn text_conversion() {
        assert_eq!(text_to_json(Scalar::Int, "42"), json!(42));
        assert_eq!(text_to_json(Scalar::Int, "9223372036854775807"), json!("9223372036854775807"));
        assert_eq!(text_to_json(Scalar::Float, "NaN"), json!("NaN"));
        assert_eq!(text_to_json(Scalar::Float, "1.5"), json!(1.5));
        assert_eq!(text_to_json(Scalar::Bool, "t"), json!(true));
        assert_eq!(text_to_json(Scalar::Json, r#"{"a":1}"#), json!({"a": 1}));
        assert_eq!(text_to_json(Scalar::TimeTz, "2024-01-02 03:04:05.5+00"), json!("2024-01-02 03:04:05.5+00:00"));
        assert_eq!(text_to_json(Scalar::TimeTz, "2024-01-02 03:04:05-03:30"), json!("2024-01-02 03:04:05-03:30"));
        assert_eq!(text_to_json(Scalar::TimeTz, "12:00:00+05"), json!("12:00:00+05:00"));
        assert_eq!(text_to_json(Scalar::TimeTz, "infinity"), json!("infinity"));
        let big = format!("\\x{}", "ab".repeat(5000));
        assert!(text_to_json(Scalar::Bytea, &big).as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn array_params() {
        assert_eq!(json_to_pg_array(&json!([1, null, "a\"b", [2]])), r#"{"1",NULL,"a\"b",{"2"}}"#);
    }
}

/// Integration tests against the docker-compose Postgres (`localhost:55432`).
/// Run with `cargo test --lib -- --ignored postgres::integration`.
#[cfg(test)]
mod integration {
    use super::*;
    use serde_json::json;

    fn cfg() -> ConnectionConfig {
        ConnectionConfig {
            id: "pg-it".into(),
            name: "pg".into(),
            kind: ConnectionKind::Postgres,
            host: "localhost".into(),
            port: Some(55432),
            user: "postgres".into(),
            password: Some("devcat".into()),
            database: Some("devcat".into()),
            file_path: None,
            url: None,
            ssl_mode: Some("disable".into()),
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
        ExecOptions { database: None, schema: Some("devcat_it".into()), max_rows: None }
    }

    const SETUP: &str = r#"
        DROP SCHEMA IF EXISTS devcat_it CASCADE;
        DROP SCHEMA IF EXISTS devcat_it2 CASCADE;
        CREATE SCHEMA devcat_it;
        SET search_path TO devcat_it;
        CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
        CREATE TABLE authors (
            id serial PRIMARY KEY,
            name text NOT NULL UNIQUE,
            born date
        );
        COMMENT ON TABLE authors IS 'Book authors';
        COMMENT ON COLUMN authors.name IS 'Full name';
        CREATE TABLE books (
            id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            author_id int REFERENCES authors(id) ON DELETE CASCADE,
            title varchar(200) NOT NULL DEFAULT 'untitled',
            price numeric(10,2) CHECK (price >= 0),
            tags text[],
            meta jsonb,
            cover bytea,
            uid uuid,
            published timestamptz,
            took interval,
            ok boolean,
            score double precision,
            m mood,
            ip inet,
            pt point,
            bits bit varying(8),
            cash money
        );
        CREATE INDEX books_title_idx ON books (lower(title));
        CREATE INDEX books_tags_idx ON books USING gin (tags);
        INSERT INTO authors (name, born) VALUES ('Ann', '1970-01-02'), ('Bob', NULL), ('Cid', '1990-12-31');
        INSERT INTO books (author_id, title, price, tags, meta, cover, uid, published, took, ok, score, m, ip, pt, bits, cash)
        VALUES (1, 'A; tricky ''title''', 9.99, ARRAY['x','y z',NULL], '{"a": [1, 2]}', '\x00ff', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
                '2024-01-02 03:04:05+00', '1 day 02:00:00', true, 1.5, 'happy', '10.0.0.1/24', '(1,2)', B'101', 12.5),
               (2, 'Second', NULL, '{}', 'null', NULL, NULL, NULL, NULL, false, 'NaN', NULL, NULL, NULL, NULL, NULL);
        CREATE VIEW cheap_books AS SELECT id, title FROM books WHERE price < 10;
        CREATE MATERIALIZED VIEW book_counts AS SELECT author_id, count(*) AS n FROM books GROUP BY author_id;
        CREATE SEQUENCE invoice_seq START 100 INCREMENT 5;
        CREATE FUNCTION add_one(i int) RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN i + 1; END; $$;
        CREATE FUNCTION add_one(i bigint) RETURNS bigint LANGUAGE sql AS 'SELECT i + 1';
        CREATE PROCEDURE noop() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
        CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.title := trim(NEW.title); RETURN NEW; END $$;
        CREATE TRIGGER books_touch BEFORE INSERT OR UPDATE ON books FOR EACH ROW EXECUTE FUNCTION touch();
    "#;

    async fn setup() -> PostgresDriver {
        let d = PostgresDriver::connect(&cfg()).await.expect("connect");
        d.execute(SETUP, &opts()).await.expect("setup");
        d
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_end_to_end() {
        let d = setup().await;
        assert!(d.server_version().await.unwrap().starts_with("PostgreSQL "));
        assert!(d.list_databases().await.unwrap().contains(&"devcat".to_string()));

        // --- tree
        let schemas = d.children(&s(&["devcat"])).await.unwrap();
        assert!(schemas.iter().any(|n| n.label == "devcat_it"));
        assert_eq!(schemas.last().unwrap().detail.as_deref(), Some("system"));
        let folders = d.children(&s(&["devcat", "devcat_it"])).await.unwrap();
        assert_eq!(folders[0].path, s(&["devcat", "devcat_it", "tables"]));
        let tables = d.children(&s(&["devcat", "devcat_it", "tables"])).await.unwrap();
        assert_eq!(tables.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), vec!["authors", "books"]);
        assert_eq!(tables[1].table.as_ref().unwrap().schema.as_deref(), Some("devcat_it"));
        let cols = d.children(&s(&["devcat", "devcat_it", "tables", "books", "columns"])).await.unwrap();
        assert_eq!(cols.len(), 17);
        assert!(cols[0].detail.as_deref().unwrap().contains("PK"));
        let idx = d.children(&s(&["devcat", "devcat_it", "tables", "books", "indexes"])).await.unwrap();
        assert!(idx.iter().any(|i| i.label == "books_tags_idx" && i.detail.as_deref().unwrap().contains("gin")));
        let fks = d.children(&s(&["devcat", "devcat_it", "tables", "books", "foreign_keys"])).await.unwrap();
        assert_eq!(fks.len(), 1);
        let trg = d.children(&s(&["devcat", "devcat_it", "tables", "books", "triggers"])).await.unwrap();
        assert_eq!(trg[0].label, "books_touch");
        let funcs = d.children(&s(&["devcat", "devcat_it", "functions"])).await.unwrap();
        let labels: Vec<_> = funcs.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.contains(&"add_one(i integer)") && labels.contains(&"add_one(i bigint)"), "{labels:?}");
        assert!(funcs.iter().any(|f| f.kind == NodeKind::Procedure));
        assert_eq!(d.children(&s(&["devcat", "devcat_it", "views"])).await.unwrap()[0].label, "cheap_books");
        assert_eq!(d.children(&s(&["devcat", "devcat_it", "materialized_views"])).await.unwrap()[0].label, "book_counts");
        assert_eq!(d.children(&s(&["devcat", "devcat_it", "sequences"])).await.unwrap().len(), 3);

        // --- execute: multi statement, value conversion, session state
        let r = d
            .execute("select * from books order by id; create temp table tmp_x(a int); insert into tmp_x values (1),(2)", &opts())
            .await
            .unwrap();
        assert_eq!(r.len(), 3);
        let row = &r[0].rows[0];
        let col = |n: &str| r[0].columns.iter().position(|c| c.name == n).unwrap();
        assert_eq!(row[col("id")], json!(1));
        assert_eq!(row[col("title")], json!("A; tricky 'title'"));
        assert_eq!(row[col("price")], json!("9.99"));
        assert_eq!(row[col("tags")], json!(["x", "y z", null]));
        assert_eq!(row[col("meta")], json!({"a": [1, 2]}));
        assert_eq!(row[col("cover")], json!("\\x00ff"));
        assert_eq!(row[col("uid")], json!("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"));
        assert_eq!(row[col("published")], json!("2024-01-02 03:04:05+00:00"));
        assert_eq!(row[col("ok")], json!(true));
        assert_eq!(row[col("score")], json!(1.5));
        assert_eq!(row[col("m")], json!("happy"));
        assert_eq!(row[col("ip")], json!("10.0.0.1/24"));
        assert_eq!(row[col("pt")], json!("(1,2)"));
        assert_eq!(row[col("bits")], json!("101"));
        assert_eq!(r[0].rows[1][col("score")], json!("NaN"));
        assert_eq!(r[0].rows[1][col("meta")], json!(null));
        assert!(r[0].columns[col("m")].type_name.ends_with("mood"));
        assert_eq!(r[2].rows_affected, Some(2));
        // temp table persists in the sticky session
        let r = d.execute("select count(*) as n from tmp_x", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0][0], json!(2));
        // empty result keeps its columns
        let r = d.execute("select id, title from books where false", &opts()).await.unwrap();
        assert_eq!(r[0].columns.len(), 2);
        assert!(r[0].rows.is_empty());
        // max_rows truncation
        let r = d
            .execute("select generate_series(1, 50)", &ExecOptions { database: None, schema: None, max_rows: Some(10) })
            .await
            .unwrap();
        assert_eq!(r[0].rows.len(), 10);
        assert!(r[0].truncated);
        // explicit transaction across executions
        d.execute("begin; insert into authors(name) values ('Tx')", &opts()).await.unwrap();
        d.execute("rollback", &opts()).await.unwrap();
        let r = d.execute("select count(*) from authors where name = 'Tx'", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0][0], json!(0));
        // error names the statement and position
        let err = d.execute("select 1;\nselect * from nope", &opts()).await.unwrap_err().to_string();
        assert!(err.contains("statement 2 of 2") && err.contains("line 2") && err.contains("nope") && err.contains("column 15"), "{err}");
        // session still usable after an error
        assert_eq!(d.execute("select 1", &opts()).await.unwrap()[0].rows[0][0], json!(1));

        // --- table data
        let books = TableRef { database: Some("devcat".into()), schema: Some("devcat_it".into()), name: "authors".into() };
        let page = d
            .table_data(&TableDataRequest {
                table: books.clone(),
                offset: 0,
                limit: 2,
                filter: Some("born IS NOT NULL OR name = 'Bob'".into()),
                sort: vec![SortSpec { column: "name".into(), desc: true }],
            })
            .await
            .unwrap();
        assert_eq!(page.total, Some(3));
        assert_eq!(page.primary_key, vec!["id"]);
        assert_eq!(page.result.rows.len(), 2);
        assert_eq!(page.result.rows[0][1], json!("Cid"));
        let err = d
            .table_data(&TableDataRequest { table: books.clone(), offset: 0, limit: 2, filter: Some("true; drop table x".into()), sort: vec![] })
            .await;
        assert!(err.is_err());

        // --- structure
        let bt = TableRef { name: "books".into(), ..books.clone() };
        let st = d.table_structure(&bt).await.unwrap();
        let id = &st.columns[0];
        assert!(id.is_primary_key && id.auto_increment && !id.nullable);
        assert_eq!(id.default.as_deref(), Some("GENERATED ALWAYS AS IDENTITY"));
        let title = st.columns.iter().find(|c| c.name == "title").unwrap();
        assert_eq!(title.data_type, "character varying(200)");
        assert_eq!(title.default.as_deref(), Some("'untitled'::character varying"));
        assert_eq!(st.foreign_keys[0].ref_table, "authors");
        assert_eq!(st.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
        assert!(st.indexes.iter().any(|i| i.name == "books_title_idx" && i.columns == vec!["lower(title::text)"]));
        let at = d.table_structure(&books).await.unwrap();
        assert_eq!(at.comment.as_deref(), Some("Book authors"));
        assert!(at.columns[0].auto_increment);
        assert!(at.indexes.iter().any(|i| i.unique && !i.primary && i.columns == vec!["name"]));

        // --- apply changes
        let mut ins = Map::new();
        ins.insert("author_id".into(), json!(3));
        ins.insert("title".into(), json!("  New  "));
        ins.insert("price".into(), json!("1.25"));
        ins.insert("tags".into(), json!(["a", "b\"c"]));
        ins.insert("meta".into(), json!({"k": true}));
        ins.insert("ok".into(), json!(true));
        ins.insert("m".into(), json!("sad"));
        ins.insert("cover".into(), json!("\\x0102"));
        let n = d.apply_changes(&bt, &[RowChange::Insert { values: ins }]).await.unwrap();
        assert_eq!(n, 1);
        let r = d.execute("select title, tags, meta, cover, m from books where author_id = 3", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0], vec![json!("New"), json!(["a", "b\"c"]), json!({"k": true}), json!("\\x0102"), json!("sad")]);
        let key: Map<String, Value> = [("id".to_string(), json!(2))].into_iter().collect();
        let vals: Map<String, Value> = [("name".to_string(), json!("Bobby"))].into_iter().collect();
        assert_eq!(d.apply_changes(&books, &[RowChange::Update { key: key.clone(), values: vals }]).await.unwrap(), 1);
        // a failing change rolls back the whole batch
        let missing: Map<String, Value> = [("id".to_string(), json!(999))].into_iter().collect();
        let err = d
            .apply_changes(&books, &[RowChange::Delete { key: key.clone() }, RowChange::Delete { key: missing }])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("affected 0 rows"), "{err}");
        let r = d.execute("select name from authors where id = 2", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0][0], json!("Bobby"));

        // --- DDL
        let ddl = d.object_ddl(&bt, &NodeKind::Table).await.unwrap();
        assert!(ddl.contains("CREATE TABLE \"devcat_it\".\"books\""), "{ddl}");
        assert!(ddl.contains("GENERATED ALWAYS AS IDENTITY"));
        assert!(ddl.contains("FOREIGN KEY (author_id) REFERENCES"));
        assert!(ddl.contains("CREATE INDEX books_title_idx"));
        assert!(ddl.contains("CREATE TRIGGER books_touch"));
        // the generated DDL is valid SQL: replay it into another schema
        let replay = ddl.replace("\"devcat_it\".", "devcat_it2.").replace("devcat_it.books", "devcat_it2.books");
        d.execute(&format!("CREATE SCHEMA devcat_it2; {replay}"), &opts())
            .await
            .expect("replay ddl");
        let adl = d.object_ddl(&books, &NodeKind::Table).await.unwrap();
        assert!(adl.starts_with("CREATE SEQUENCE IF NOT EXISTS devcat_it.authors_id_seq"), "{adl}");
        assert!(adl.contains("ALTER SEQUENCE devcat_it.authors_id_seq OWNED BY devcat_it.authors.id;"), "{adl}");
        // self-contained: replays into an empty schema (sequence + table + owned by)
        let replay = adl.replace("\"devcat_it\".", "devcat_it2.").replace("devcat_it.", "devcat_it2.");
        d.execute(&replay, &opts()).await.expect("replay authors ddl");
        let r = d.execute("insert into devcat_it2.authors(name) values ('z') returning id", &opts()).await.unwrap();
        assert_eq!(r[0].rows[0][0], json!(1));
        assert!(adl.contains("COMMENT ON TABLE \"devcat_it\".\"authors\" IS 'Book authors'"), "{adl}");
        let v = d.object_ddl(&TableRef { name: "cheap_books".into(), ..books.clone() }, &NodeKind::View).await.unwrap();
        assert!(v.starts_with("CREATE OR REPLACE VIEW \"devcat_it\".\"cheap_books\" AS"), "{v}");
        let mv = d.object_ddl(&TableRef { name: "book_counts".into(), ..books.clone() }, &NodeKind::MaterializedView).await.unwrap();
        assert!(mv.contains("CREATE MATERIALIZED VIEW"));
        let f = d.object_ddl(&TableRef { name: "add_one(i integer)".into(), ..books.clone() }, &NodeKind::Function).await.unwrap();
        assert!(f.contains("CREATE OR REPLACE FUNCTION devcat_it.add_one(i integer)") && !f.contains("bigint"), "{f}");
        let seq = d.object_ddl(&TableRef { name: "invoice_seq".into(), ..books.clone() }, &NodeKind::Sequence).await.unwrap();
        assert!(seq.contains("START WITH 100") && seq.contains("INCREMENT BY 5"), "{seq}");
        let serial = d.object_ddl(&TableRef { name: "authors_id_seq".into(), ..books.clone() }, &NodeKind::Sequence).await.unwrap();
        assert!(serial.contains("OWNED BY devcat_it.authors.id"), "{serial}");
        let t = d.object_ddl(&TableRef { name: "books_touch".into(), ..books.clone() }, &NodeKind::Trigger).await.unwrap();
        assert!(t.starts_with("CREATE TRIGGER books_touch"));

        // --- snapshot
        let snap = d.schema_snapshot(Some("devcat"), Some("devcat_it")).await.unwrap();
        assert_eq!(snap.tables.len(), 2);
        assert_eq!(snap.views.len(), 2);
        let b = snap.tables.iter().find(|t| t.table.as_ref().unwrap().name == "books").unwrap();
        assert_eq!(b.columns.len(), 17);
        assert_eq!(b.foreign_keys.len(), 1);

        // --- cancel
        let d = Arc::new(d);
        let d2 = d.clone();
        let h = tokio::spawn(async move { d2.execute("select pg_sleep(20); select 1", &opts()).await });
        tokio::time::sleep(Duration::from_millis(700)).await;
        d.cancel().await.unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel took too long").unwrap();
        let err = res.unwrap_err().to_string();
        assert!(err.contains("cancel"), "{err}");
        d.execute("DROP SCHEMA devcat_it CASCADE; DROP SCHEMA devcat_it2 CASCADE", &opts()).await.unwrap();
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_snapshot_many_tables() {
        let d = PostgresDriver::connect(&cfg()).await.unwrap();
        let o = ExecOptions { database: None, schema: None, max_rows: None };
        d.execute(
            "DROP SCHEMA IF EXISTS devcat_many CASCADE; CREATE SCHEMA devcat_many;
             DO $$ BEGIN FOR i IN 1..600 LOOP
               EXECUTE format('CREATE TABLE devcat_many.t%s (id serial PRIMARY KEY, name text NOT NULL, parent int REFERENCES devcat_many.t1(id), created timestamptz DEFAULT now())', i);
               EXECUTE format('CREATE INDEX ON devcat_many.t%s (name)', i);
             END LOOP; END $$",
            &o,
        )
        .await
        .unwrap();
        let started = Instant::now();
        let snap = d.schema_snapshot(None, Some("devcat_many")).await.unwrap();
        let took = started.elapsed();
        assert_eq!(snap.tables.len(), 600);
        assert!(snap.tables.iter().all(|t| t.columns.len() == 4 && t.indexes.len() == 2 && t.foreign_keys.len() == 1));
        assert!(took < Duration::from_secs(3), "snapshot took {took:?}");
        eprintln!("postgres snapshot of 600 tables: {took:?}");
        d.execute("DROP SCHEMA devcat_many CASCADE", &o).await.unwrap();
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_truncation_stops_early() {
        let d = PostgresDriver::connect(&cfg()).await.unwrap();
        let o = ExecOptions { database: None, schema: None, max_rows: Some(1000) };
        let started = Instant::now();
        let r = d.execute("select g from generate_series(1, 5000000) g", &o).await.unwrap();
        let took = started.elapsed();
        eprintln!("postgres 5M-row select truncated in {took:?}");
        assert!(took < Duration::from_millis(1500), "took {took:?}");
        assert!(r[0].truncated);
        assert_eq!(r[0].rows.len(), 1000);
        assert_eq!(r[0].rows[999][0], json!(1000));
        // the sticky session is clean afterwards
        assert_eq!(d.execute("select 42", &o).await.unwrap()[0].rows[0][0], json!(42));
        // multi-statement scripts continue after a truncated statement
        let r = d.execute("select g from generate_series(1, 3000000) g; select 'next'", &o).await.unwrap();
        assert!(r[0].truncated);
        assert_eq!(r[1].rows[0][0], json!("next"));
        // inside an explicit transaction the query is drained, not cancelled, so the transaction survives
        d.execute("begin; create temp table early_tx(a int); insert into early_tx values (1)", &o).await.unwrap();
        let r = d.execute("select g from generate_series(1, 200000) g", &o).await.unwrap();
        assert!(r[0].truncated);
        assert_eq!(d.execute("select count(*) from early_tx", &o).await.unwrap()[0].rows[0][0], json!(1));
        d.execute("rollback", &o).await.unwrap();
        // data-modifying statements are never cancelled
        d.execute("create temp table early_ins(a int)", &o).await.unwrap();
        let r = d.execute("insert into early_ins select g from generate_series(1, 5000) g returning a", &ExecOptions { max_rows: Some(10), ..o.clone() }).await.unwrap();
        assert!(r[0].truncated);
        assert_eq!(d.execute("select count(*) from early_ins", &o).await.unwrap()[0].rows[0][0], json!(5000));
        d.close().await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_read_only() {
        let mut c = cfg();
        c.read_only = true;
        let d = PostgresDriver::connect(&c).await.unwrap();
        assert!(d.execute("select 1", &opts()).await.is_ok());
        let err = d.execute("select 1; create table ro_x (a int)", &opts()).await.unwrap_err().to_string();
        assert!(err.contains("read-only"), "{err}");
        // server side enforcement for things the keyword check lets through
        let err = d.execute("select * into ro_y from pg_class", &opts()).await.unwrap_err().to_string();
        assert!(err.contains("read-only"), "{err}");
        d.close().await;
    }
}
