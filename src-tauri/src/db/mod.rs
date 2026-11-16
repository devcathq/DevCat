//! Database driver abstraction shared by every database backend.
//!
//! Tree browsing is path based: the frontend asks for the children of a path
//! (e.g. `[]`, `["mydb"]`, `["mydb", "public"]`, `["mydb", "public", "tables"]`)
//! and each driver decides what lives at each level.

pub mod jdbc;
pub mod mongo;
pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod redis;
pub mod sql_common;
pub mod sqlite;
pub mod ssh;
pub mod ddl;
pub mod transfer;

use crate::error::{AppError, AppResult};
use crate::models::{ConnectionConfig, ConnectionKind};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Database,
    Schema,
    /// Grouping folder: "Tables", "Views", "Functions", ...
    Folder,
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Index,
    ForeignKey,
    Column,
    Event,
    Collection,
    /// Redis key or key-prefix group
    Key,
    KeyGroup,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// Full path of this node; pass it back to `children` to expand it.
    pub path: Vec<String>,
    pub label: String,
    pub kind: NodeKind,
    pub has_children: bool,
    /// Optional extra info (row count, key type, column type, ...).
    #[serde(default)]
    pub detail: Option<String>,
    /// Set for openable objects (tables, views, collections, keys).
    #[serde(default)]
    pub table: Option<TableRef>,
}

/// Fully qualified reference to a table-like object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub statement: String,
    pub columns: Vec<ColumnInfo>,
    /// Cell values as JSON. Binary => base64 string prefixed with "\\x" hex if small,
    /// dates/times => ISO strings, decimals => strings, NULL => null.
    pub rows: Vec<Vec<Value>>,
    pub rows_affected: Option<u64>,
    pub elapsed_ms: u64,
    /// True when rows were capped by `ExecOptions::max_rows`.
    pub truncated: bool,
    /// Free text output (notices, Redis replies, Mongo command output).
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOptions {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub max_rows: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub column: String,
    pub desc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataRequest {
    pub table: TableRef,
    pub offset: u64,
    pub limit: u64,
    /// SQL: raw WHERE expression (without the WHERE keyword).
    /// MongoDB: JSON filter document.
    pub filter: Option<String>,
    #[serde(default)]
    pub sort: Vec<SortSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataPage {
    pub result: QueryResult,
    /// Total row count if cheaply known.
    pub total: Option<u64>,
    /// Primary key columns (used by the grid for edits). For Mongo: ["_id"].
    pub primary_key: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub auto_increment: bool,
    pub comment: Option<String>,
    pub ordinal: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// btree, hash, gin, ... if known
    pub method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDef {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub table: Option<TableRef>,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKeyDef>,
    pub comment: Option<String>,
}

/// Whole-schema snapshot used by the ER diagram and schema compare.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSnapshot {
    pub tables: Vec<TableStructure>,
    /// (view ref, definition SQL)
    pub views: Vec<(TableRef, String)>,
}

/// A pending grid edit. Keys/values are column name -> JSON value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RowChange {
    Insert { values: Map<String, Value> },
    Update { key: Map<String, Value>, values: Map<String, Value> },
    Delete { key: Map<String, Value> },
}

/// SQL dialect information used by generic helpers (transfer, DDL generation).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
    Mssql,
    /// Non SQL stores (MongoDB, Redis)
    None,
    /// Standard SQL, for databases reached through JDBC: `"…"` identifiers, `'…'` strings, `?`
    /// parameters, `--` and `/* */` comments. The driver quotes names its own way.
    Generic,
}

#[async_trait]
pub trait Driver: Send + Sync {
    fn kind(&self) -> ConnectionKind;
    fn dialect(&self) -> Dialect;

    /// Verify the connection is alive and return a server version string.
    async fn server_version(&self) -> AppResult<String>;

    /// Browse the object tree. `path == []` returns top-level nodes (usually databases).
    async fn children(&self, path: &[String]) -> AppResult<Vec<TreeNode>>;

    async fn list_databases(&self) -> AppResult<Vec<String>>;

    /// Execute one or more statements (SQL script, Mongo shell command, Redis command line).
    /// Returns one result per statement.
    async fn execute(&self, script: &str, opts: &ExecOptions) -> AppResult<Vec<QueryResult>>;

    async fn table_data(&self, req: &TableDataRequest) -> AppResult<TableDataPage>;

    async fn table_structure(&self, table: &TableRef) -> AppResult<TableStructure>;

    /// Apply grid edits in a single transaction where supported. Returns affected rows.
    async fn apply_changes(&self, table: &TableRef, changes: &[RowChange]) -> AppResult<u64>;

    /// CREATE statement for a table/view.
    async fn object_ddl(&self, table: &TableRef, kind: &NodeKind) -> AppResult<String>;

    async fn schema_snapshot(&self, database: Option<&str>, schema: Option<&str>) -> AppResult<SchemaSnapshot>;

    /// Key/value stores (Redis): read a key with its type, TTL and value.
    /// Returns `{ "key", "type", "ttl", "value", "length" }`.
    async fn get_key(&self, _database: Option<&str>, _key: &str) -> AppResult<Value> {
        Err(AppError::Unsupported("get_key".into()))
    }

    /// Key/value stores (Redis): write a key. `value` uses the same shape as `get_key`.
    async fn set_key(&self, _database: Option<&str>, _value: &Value) -> AppResult<()> {
        Err(AppError::Unsupported("set_key".into()))
    }

    async fn delete_key(&self, _database: Option<&str>, _key: &str) -> AppResult<()> {
        Err(AppError::Unsupported("delete_key".into()))
    }

    /// Cancel running queries if supported.
    async fn cancel(&self) -> AppResult<()> {
        Err(AppError::Unsupported("cancel".into()))
    }

    async fn close(&self);
}

pub type DynDriver = Arc<dyn Driver>;

/// Create and connect a driver for a database connection profile.
pub async fn connect(cfg: &ConnectionConfig) -> AppResult<DynDriver> {
    Ok(match cfg.kind {
        ConnectionKind::Postgres => Arc::new(postgres::PostgresDriver::connect(cfg).await?),
        ConnectionKind::Mysql => Arc::new(mysql::MysqlDriver::connect(cfg).await?),
        ConnectionKind::Sqlite => Arc::new(sqlite::SqliteDriver::connect(cfg).await?),
        ConnectionKind::Mssql => Arc::new(mssql::MssqlDriver::connect(cfg).await?),
        ConnectionKind::Mongodb => Arc::new(mongo::MongoDriver::connect(cfg).await?),
        ConnectionKind::Redis => Arc::new(redis::RedisDriver::connect(cfg).await?),
        ConnectionKind::Jdbc => Arc::new(jdbc::JdbcDriver::connect(cfg).await?),
        k => return Err(AppError::msg(format!("{k:?} is not a database connection"))),
    })
}
