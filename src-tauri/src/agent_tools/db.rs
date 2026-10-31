//! Database tools beyond `db_schema` / `db_query` / `db_dump` / `db_export` (in `mod.rs`): an
//! object's DDL, importing a CSV / JSON file into a table, and restoring a database from a dump
//! (a .sql file; MongoDB a folder of .jsonl files). The same code as the app's Import and Restore
//! dialogs. Files are on this computer; relative paths start in the agent's folder. Importing
//! changes data, so it needs the connection's AI access at Full over MCP (the DevCat Agent asks
//! first); emptying the table first can't be undone, and neither can a restore (dumps drop and
//! recreate tables), so those are the DevCat Agent's only, confirmed by typing the name
//! (`Tool::confirm`).
use super::*;
use crate::commands::{db as dbc, transfer};
use crate::db::NodeKind;

pub(super) fn add(out: &mut Vec<Arc<dyn Tool>>, s: &Arc<Scope>) {
    out.push(Arc::new(DbDdl(s.clone())));
    out.push(Arc::new(DbImport(s.clone())));
    if !s.mcp {
        out.push(Arc::new(DbRestore(s.clone())));
    }
}

fn table_ref(c: &ConnectionConfig, input: &Value) -> Option<TableRef> {
    Some(TableRef { database: database_of(c, input), schema: arg(input, "schema"), name: arg(input, "table")? })
}

/// Options the model passed, as the transfer code reads them.
fn options(input: &Value) -> Value {
    input.get("options").filter(|o| o.is_object()).cloned().unwrap_or_else(|| json!({}))
}

/// Whether an import empties the table first (`options.mode` = truncate).
fn truncates(input: &Value) -> bool {
    options(input).get("mode").and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case("truncate"))
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
        let c = match db_conn(&self.0, &input, "db_ddl") {
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

// ---------- db_import

struct DbImport(Arc<Scope>);

#[async_trait]
impl Tool for DbImport {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_import".into(),
            description: "Import a CSV, TSV, JSON or JSON-lines file on this computer into an existing table of a DevCat database connection (values are converted to the column types); the rows are added to the table. `options` as DevCat's Import dialog takes them: { \"mapping\": { \"file column\": \"table column\" } }, { \"delimiter\": \";\", \"header\": true, \"skipRows\": 1 }, { \"nullText\": \"NULL\" }. { \"mode\": \"truncate\" } empties the table first, which can't be undone: DevCat Agent only, and the user confirms it by typing the table's name. Changes data: only when the user asks.".into(),
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
        let empty = if truncates(input) { " (emptying it first)" } else { "" };
        format!("{}: {} → {}{empty}", conn_summary(input), arg(input, "path").unwrap_or_default(), arg(input, "table").unwrap_or_default())
    }
    fn confirm(&self, input: &Value) -> Option<String> {
        truncates(input).then(|| arg(input, "table").unwrap_or_else(|| "(no table)".into()))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(600)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        if self.0.mcp && truncates(&input) {
            return ToolOutput::err("Over MCP an import only adds rows: emptying the table first is for the DevCat Agent in DevCat, where the user confirms it.");
        }
        let c = match db_conn(&self.0, &input, "db_import").and_then(|c| may_change(&self.0, &c).map(|_| c)) {
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

// ---------- db_restore

struct DbRestore(Arc<Scope>);

/// What the user types to allow a restore: the database it runs on.
fn restore_target(scope: &Scope, input: &Value) -> String {
    find(scope, input).ok().and_then(|c| database_of(&c, input)).or_else(|| arg(input, "database")).unwrap_or_else(|| "(no database)".into())
}

#[async_trait]
impl Tool for DbRestore {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "db_restore".into(),
            description: "Run a dump (a .sql file on this computer, e.g. from `db_dump`; for MongoDB a folder of <collection>.jsonl files) against a database of a DevCat connection, statement by statement. Dumps usually drop and recreate their tables, so what's there now is replaced: it can't be undone. Only when the user asked to restore into that database; suggest a `db_dump` of it first. DevCat asks the user to confirm by typing the database's name. Stops at the first failing statement unless `options` has { \"continueOnError\": true }; { \"dropExisting\": true } empties MongoDB collections first.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name or id" },
                    "database": { "type": "string", "description": "Database, if not the connection's default" },
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
    fn confirm(&self, input: &Value) -> Option<String> {
        Some(restore_target(&self.0, input))
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(60 * 60)
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        if self.0.mcp {
            return ToolOutput::err("Databases are restored from the DevCat Agent in DevCat only, where the user confirms it.");
        }
        let c = match db_conn(&self.0, &input, "db_restore").and_then(|c| may_change(&self.0, &c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let Some(path) = arg(&input, "path") else { return ToolOutput::err("Missing `path`.") };
        match transfer::restore_database(St::get(), c.id.clone(), database_of(&c, &input), local_path(ctx, &path), options(&input)).await {
            Ok(summary) => ToolOutput::ok(summary),
            Err(e) => ToolOutput::err(connect_error(&self.0, &c, &e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_emptying_a_table_asks_to_type_its_name() {
        let t = DbImport(Arc::new(Scope::default()));
        assert_eq!(t.confirm(&json!({ "table": "users", "options": { "mode": "append" } })), None);
        assert_eq!(t.confirm(&json!({ "table": "users" })), None);
        assert_eq!(t.confirm(&json!({ "table": "users", "options": { "mode": "TRUNCATE" } })).as_deref(), Some("users"));
    }
}
