//! Database tools beyond `db_schema` / `db_query` (in `mod.rs`): an object's DDL, exporting a
//! table or query to a file (CSV, TSV, JSON, JSON lines, SQL inserts, Excel), importing a CSV /
//! JSON file into a table, and dumping / restoring a database (a .sql file; MongoDB a folder of
//! .jsonl files). The same code as the app's Export / Import / Backup dialogs. Files are on this
//! computer; relative paths start in the agent's folder. Writing a file asks like an edit (inside
//! the folder or outside it); importing and restoring change data, so they need the
//! connection's AI access at Full over MCP, and the DevCat Agent asks first.
use super::*;
use crate::commands::{db as dbc, transfer};
use crate::db::NodeKind;
use std::path::Component;

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>) {
    out.push(Arc::new(DbDdl(s.clone())));
    out.push(Arc::new(DbExport(s.clone())));
    out.push(Arc::new(DbImport(s.clone())));
    out.push(Arc::new(DbBackup(s.clone())));
    out.push(Arc::new(DbRestore(s.clone())));
}

fn db_conn(scope: &Scope, input: &Value) -> Result<ConnectionConfig, ToolOutput> {
    let c = find(scope, input)?;
    if !is_db(c.kind) {
        return Err(ToolOutput::err(format!("{} is a {} connection, not a database.", c.name, kind_name(c.kind))));
    }
    network::usable(scope, &c)?;
    Ok(c)
}

fn table_ref(c: &ConnectionConfig, input: &Value) -> Option<TableRef> {
    Some(TableRef { database: arg(input, "database").or(c.database.clone().filter(|d| !d.is_empty())), schema: arg(input, "schema"), name: arg(input, "table")? })
}

/// Writing `path` asks like an edit inside the agent's folder, else like one outside it.
fn file_kind(ctx: &ToolCtx, path: Option<String>) -> Kind {
    let Some(p) = path else { return Kind::EditOutside };
    let full = PathBuf::from(local_path(ctx, &p));
    let inside = !ctx.cwd.as_os_str().is_empty() && full.starts_with(&ctx.cwd) && !full.components().any(|c| c == Component::ParentDir);
    if inside { Kind::Edit } else { Kind::EditOutside }
}

/// Options the model passed, as the transfer code reads them.
fn options(input: &Value) -> Value {
    input.get("options").filter(|o| o.is_object()).cloned().unwrap_or_else(|| json!({}))
}

// ---------- db_ddl

struct DbDdl(Arc<Scope>);

#[async_trait]
impl Tool for DbDdl {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_ddl".into(),
            description: "The CREATE statement (DDL) of a table, view, materialized view, function, procedure, trigger or sequence in a DevCat database connection, as the database reports it.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "table": { "type": "string", "description": "The object's name" },
                    "object": { "type": "string", "enum": ["table", "view", "materializedView", "function", "procedure", "trigger", "sequence"], "description": "Default table" },
                    "database": { "type": "string" },
                    "schema": { "type": "string" }
                },
                "required": ["connection", "table"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Read
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {}", conn_summary(input), arg(input, "table").unwrap_or_default())
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(t) = table_ref(&c, &input) else { return ToolOutput::err("Missing `table`.") };
        let object = arg(&input, "object").unwrap_or_else(|| "table".into());
        let Ok(kind) = serde_json::from_value::<NodeKind>(json!(object)) else { return ToolOutput::err(format!("Unknown `object` \"{object}\".")) };
        match dbc::db_object_ddl(St::get(), c.id.clone(), t, kind).await {
            Ok(ddl) => ToolOutput::ok(ddl),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- db_export

struct DbExport(Arc<Scope>);

#[async_trait]
impl Tool for DbExport {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_export".into(),
            description: "Export a table (`table`) or a query's result (`query`) from a DevCat database connection to a file on this computer, without the rows going through you: `format` csv, tsv, json, jsonl, sql (INSERT statements) or xlsx. `options` as DevCat's Export dialog takes them, e.g. { \"filter\": \"created_at > now() - interval '1 day'\" } for a table, { \"delimiter\": \";\", \"header\": true }, { \"targetDialect\": \"mysql\", \"includeCreate\": true } for sql, { \"maxRows\": 50000 }. Returns how many rows were written.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "table": { "type": "string" },
                    "query": { "type": "string" },
                    "database": { "type": "string" },
                    "schema": { "type": "string" },
                    "format": { "type": "string", "enum": ["csv", "tsv", "json", "jsonl", "sql", "xlsx"] },
                    "path": { "type": "string", "description": "File to write" },
                    "options": { "type": "object" }
                },
                "required": ["connection", "format", "path"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        file_kind(ctx, arg(input, "path"))
    }
    fn summary(&self, input: &Value) -> String {
        let what = arg(input, "table").or_else(|| arg(input, "query")).unwrap_or_default();
        format!("{}: {what} → {}", conn_summary(input), arg(input, "path").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(600)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let (Some(format), Some(path)) = (arg(&input, "format"), arg(&input, "path")) else { return ToolOutput::err("Missing `format` or `path`.") };
        let (table, query) = (table_ref(&c, &input), arg(&input, "query"));
        if table.is_some() == query.is_some() {
            return ToolOutput::err("Give either `table` or `query`.");
        }
        if let Some(q) = &query
            && !reads_only(&c, q)
        {
            return ToolOutput::err("An export runs a query that only reads (SELECT ...).");
        }
        let path = local_path(ctx, &path);
        let mut opts = options(&input);
        if let (Some(s), Some(m)) = (arg(&input, "schema"), opts.as_object_mut()) {
            m.entry("schema").or_insert(json!(s));
        }
        match transfer::export_data(St::get(), c.id.clone(), table, query, arg(&input, "database"), format, path.clone(), opts).await {
            Ok(n) => ToolOutput::ok(format!("Wrote {n} row{} to {path}.", if n == 1 { "" } else { "s" })),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- db_import

struct DbImport(Arc<Scope>);

#[async_trait]
impl Tool for DbImport {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_import".into(),
            description: "Import a CSV, TSV, JSON or JSON-lines file on this computer into an existing table of a DevCat database connection (values are converted to the column types). `options` as DevCat's Import dialog takes them: { \"mapping\": { \"file column\": \"table column\" } }, { \"mode\": \"truncate\" } to empty the table first (default append), { \"delimiter\": \";\", \"header\": true, \"skipRows\": 1 }, { \"nullText\": \"NULL\" }. Changes data: only when the user asks.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "table": { "type": "string" },
                    "database": { "type": "string" },
                    "schema": { "type": "string" },
                    "format": { "type": "string", "enum": ["csv", "tsv", "json", "jsonl"] },
                    "path": { "type": "string", "description": "File to read" },
                    "options": { "type": "object" }
                },
                "required": ["connection", "table", "format", "path"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{}: {} → {}", conn_summary(input), arg(input, "path").unwrap_or_default(), arg(input, "table").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(600)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let (Some(t), Some(format), Some(path)) = (table_ref(&c, &input), arg(&input, "format"), arg(&input, "path")) else { return ToolOutput::err("Missing `table`, `format` or `path`.") };
        match transfer::import_data(St::get(), c.id.clone(), t.clone(), format, local_path(ctx, &path), options(&input)).await {
            Ok(n) => ToolOutput::ok(format!("Imported {n} row{} into {}.", if n == 1 { "" } else { "s" }, qualified(&t))),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- db_backup

struct DbBackup(Arc<Scope>);

#[async_trait]
impl Tool for DbBackup {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_backup".into(),
            description: "Dump a database (or one `schema`) of a DevCat connection to a .sql file on this computer: CREATE statements and the data as INSERTs (for MongoDB `path` is a folder that gets one .jsonl file per collection). `options` e.g. { \"tables\": [\"users\", \"orders\"] }, { \"structureOnly\": true }, { \"includeData\": false }, { \"dropIfExists\": false }. Returns what was written.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string" },
                    "schema": { "type": "string" },
                    "path": { "type": "string", "description": "File (or folder for MongoDB) to write" },
                    "options": { "type": "object" }
                },
                "required": ["connection", "path"]
            }),
        }
    }
    fn kind(&self, input: &Value, ctx: &ToolCtx) -> Kind {
        file_kind(ctx, arg(input, "path"))
    }
    fn summary(&self, input: &Value) -> String {
        format!("{} → {}", conn_summary(input), arg(input, "path").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(1800)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(path) = arg(&input, "path") else { return ToolOutput::err("Missing `path`.") };
        let database = arg(&input, "database").or(c.database.clone().filter(|d| !d.is_empty()));
        match transfer::backup_database(St::get(), c.id.clone(), database, arg(&input, "schema"), local_path(ctx, &path), options(&input)).await {
            Ok(summary) => ToolOutput::ok(summary),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

// ---------- db_restore

struct DbRestore(Arc<Scope>);

#[async_trait]
impl Tool for DbRestore {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_restore".into(),
            description: "Run a .sql file on this computer against a database of a DevCat connection, statement by statement (for MongoDB, a folder of <collection>.jsonl files as `db_backup` writes them). Stops at the first failing statement unless `options` has { \"continueOnError\": true }; { \"dropExisting\": true } empties MongoDB collections first. Changes data, often a lot: only when the user asks, and never on a production database without them saying so.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string" },
                    "path": { "type": "string", "description": "The .sql file (or folder for MongoDB)" },
                    "options": { "type": "object" }
                },
                "required": ["connection", "path"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Exec
    }
    fn summary(&self, input: &Value) -> String {
        format!("{} ← {}", conn_summary(input), arg(input, "path").unwrap_or_default())
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(1800)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let c = match db_conn(&self.0, &input).and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(path) = arg(&input, "path") else { return ToolOutput::err("Missing `path`.") };
        let database = arg(&input, "database").or(c.database.clone().filter(|d| !d.is_empty()));
        match transfer::restore_database(St::get(), c.id.clone(), database, local_path(ctx, &path), options(&input)).await {
            Ok(summary) => ToolOutput::ok(summary),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_inside_the_folder_ask_like_edits() {
        let ctx = ToolCtx::at(std::env::temp_dir().join("proj"));
        assert_eq!(file_kind(&ctx, Some("out/users.csv".into())), Kind::Edit);
        assert_eq!(file_kind(&ctx, Some("../elsewhere.csv".into())), Kind::EditOutside);
        let outside = std::env::temp_dir().join("other").join("x.csv");
        assert_eq!(file_kind(&ctx, Some(outside.to_string_lossy().into_owned())), Kind::EditOutside);
        assert_eq!(file_kind(&ctx, None), Kind::EditOutside);
    }
}
