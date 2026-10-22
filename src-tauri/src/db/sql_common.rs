//! Helpers shared by SQL drivers: identifier quoting, literal rendering,
//! script splitting, DML generation for grid edits.
use super::{Dialect, RowChange, SortSpec, TableRef};
use crate::error::AppError;
use serde_json::{Map, Value};

/// Binary values larger than this are truncated when rendered for the grid.
pub const MAX_BINARY_DISPLAY: usize = 4096;

pub fn quote_ident(d: Dialect, ident: &str) -> String {
    match d {
        Dialect::Mysql => format!("`{}`", ident.replace('`', "``")),
        Dialect::Mssql => format!("[{}]", ident.replace(']', "]]")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

/// Quote a (possibly) qualified object name for the dialect.
///
/// - Postgres: `"schema"."name"` (a connection is bound to one database)
/// - MySQL: `` `database`.`name` ``
/// - SQLite: `"database"."name"` (`main`, `temp` or an attached database)
/// - MSSQL: `[database].[schema].[name]`
pub fn qualified_name(d: Dialect, t: &TableRef) -> String {
    let parts: Vec<&str> = match d {
        Dialect::Postgres => [t.schema.as_deref(), Some(t.name.as_str())].into_iter().flatten().collect(),
        Dialect::Mysql => [t.database.as_deref(), Some(t.name.as_str())].into_iter().flatten().collect(),
        Dialect::Sqlite => [t.database.as_deref().or(t.schema.as_deref()), Some(t.name.as_str())]
            .into_iter()
            .flatten()
            .collect(),
        Dialect::Mssql | Dialect::None => {
            let schema = t.schema.as_deref().or(t.database.as_ref().map(|_| "dbo"));
            [t.database.as_deref(), schema, Some(t.name.as_str())].into_iter().flatten().collect()
        }
    };
    parts.into_iter().filter(|p| !p.is_empty()).map(|p| quote_ident(d, p)).collect::<Vec<_>>().join(".")
}

/// Render a JSON value as a SQL literal for the given dialect.
///
/// Strings are always fully escaped for the dialect so the result is safe to
/// splice into SQL text:
/// - Postgres: `'...'` with `''`; strings containing backslashes use `E'...'`
///   with doubled backslashes so the literal is correct whatever
///   `standard_conforming_strings` is set to.
/// - MySQL: `'...'` with `''` and doubled backslashes (also safe under
///   `NO_BACKSLASH_ESCAPES`, where it merely keeps the doubled backslashes).
/// - MSSQL: `N'...'` with `''`.
/// - SQLite: `'...'` with `''`.
///
/// Objects and arrays are rendered as their JSON text.
pub fn literal(d: Dialect, v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => match d {
            Dialect::Postgres => if *b { "TRUE".into() } else { "FALSE".into() },
            _ => if *b { "1".into() } else { "0".into() },
        },
        Value::Number(n) => n.to_string(),
        Value::String(s) => string_literal(d, s),
        other => string_literal(d, &other.to_string()),
    }
}

/// Escape a string as a SQL string literal (see [`literal`]).
pub fn string_literal(d: Dialect, s: &str) -> String {
    let esc = s.replace('\'', "''");
    match d {
        Dialect::Mysql => format!("'{}'", esc.replace('\\', "\\\\")),
        Dialect::Mssql => format!("N'{esc}'"),
        Dialect::Postgres if s.contains('\\') => format!("E'{}'", esc.replace('\\', "\\\\")),
        _ => format!("'{esc}'"),
    }
}

/// `"\x0a0b…"` rendering of binary data used in grid results, truncated to
/// [`MAX_BINARY_DISPLAY`] bytes (with a trailing `…`).
pub fn binary_to_value(bytes: &[u8]) -> Value {
    let truncated = bytes.len() > MAX_BINARY_DISPLAY;
    let shown = if truncated { &bytes[..MAX_BINARY_DISPLAY] } else { bytes };
    let mut s = String::with_capacity(shown.len() * 2 + 4);
    s.push_str("\\x");
    s.push_str(&hex::encode(shown));
    if truncated {
        s.push('…');
    }
    Value::String(s)
}

/// Inverse of [`binary_to_value`] for grid edits: `"\x0a0b"` → bytes.
/// Returns `None` if the string is not in that format (or was truncated).
pub fn parse_hex_binary(s: &str) -> Option<Vec<u8>> {
    let h = s.strip_prefix("\\x").or_else(|| s.strip_prefix("0x"))?;
    hex::decode(h).ok()
}

// ---------------------------------------------------------------------------
// Script splitting
// ---------------------------------------------------------------------------

/// One statement of a script with its byte offset in the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStatement {
    pub sql: String,
    /// Byte offset of the first character of `sql` in the script.
    pub offset: usize,
}

impl ScriptStatement {
    /// 1-based line number of the statement start within the script.
    pub fn line(&self, script: &str) -> usize {
        script[..self.offset.min(script.len())].matches('\n').count() + 1
    }
}

/// Split a script into individual statements for the given dialect, honoring
/// quotes, comments, Postgres dollar-quoting, MySQL `DELIMITER` and MSSQL `GO`.
///
/// - MSSQL scripts are split into batches on lines containing only `GO`
///   (semicolons are left to the server, as with sqlcmd/SSMS).
/// - `CREATE ... BEGIN ... END` bodies (MySQL routines/triggers, SQLite
///   triggers, Postgres `BEGIN ATOMIC`) are kept whole even without `DELIMITER`.
/// - Statements consisting only of whitespace/comments are dropped.
pub fn split_statements(d: Dialect, script: &str) -> Vec<String> {
    split_script(d, script).into_iter().map(|s| s.sql).collect()
}

/// Like [`split_statements`] but keeps the offset of each statement.
pub fn split_script(d: Dialect, script: &str) -> Vec<ScriptStatement> {
    Splitter::new(d, script).run()
}

struct Splitter<'a> {
    d: Dialect,
    s: &'a str,
    b: &'a [u8],
    i: usize,
    start: usize,
    delimiter: String,
    out: Vec<ScriptStatement>,
    /// First keyword of the current statement (uppercase), once seen.
    first_word: Option<String>,
    /// BEGIN/CASE ... END nesting depth inside CREATE statements.
    depth: i32,
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_word_char(c: u8, d: Dialect) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || (c == b'$' && d != Dialect::Mssql) || (c == b'@' && d == Dialect::Mssql) || (c == b'#' && d == Dialect::Mssql)
}

impl<'a> Splitter<'a> {
    fn new(d: Dialect, s: &'a str) -> Self {
        Splitter { d, s, b: s.as_bytes(), i: 0, start: 0, delimiter: ";".into(), out: vec![], first_word: None, depth: 0 }
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.b.get(self.i + off).copied()
    }

    fn push(&mut self, end: usize) {
        let raw = &self.s[self.start..end];
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !is_only_comments(self.d, trimmed) {
            let lead = raw.len() - raw.trim_start().len();
            self.out.push(ScriptStatement { sql: trimmed.to_string(), offset: self.start + lead });
        }
        self.first_word = None;
        self.depth = 0;
    }

    fn line_end(&self, from: usize) -> usize {
        self.b[from..].iter().position(|&c| c == b'\n').map(|p| from + p).unwrap_or(self.b.len())
    }

    /// Handle client directives that must start a line (`GO`, `DELIMITER`).
    /// Returns true if the line was consumed.
    fn line_directive(&mut self) -> bool {
        let end = self.line_end(self.i);
        let line = self.s[self.i..end].trim();
        match self.d {
            Dialect::Mssql => {
                let mut parts = line.split_whitespace();
                let is_go = parts.next().is_some_and(|w| w.eq_ignore_ascii_case("go"))
                    && parts.next().is_none_or(|n| n.chars().all(|c| c.is_ascii_digit()) || n.starts_with("--"));
                if is_go && line.len() >= 2 {
                    self.push(self.i);
                    self.i = end;
                    self.start = end;
                    return true;
                }
            }
            Dialect::Mysql => {
                if line.len() > 9 && line[..9].eq_ignore_ascii_case("delimiter") && line.as_bytes()[9].is_ascii_whitespace() {
                    if let Some(delim) = line[9..].split_whitespace().next() {
                        self.push(self.i);
                        self.delimiter = delim.to_string();
                        self.i = end;
                        self.start = end;
                        return true;
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn run(mut self) -> Vec<ScriptStatement> {
        let n = self.b.len();
        while self.i < n {
            if (self.i == 0 || self.b[self.i - 1] == b'\n') && self.line_directive() {
                continue;
            }
            let c = self.b[self.i];
            // Delimiter (outside of strings/comments and CREATE bodies).
            if self.d != Dialect::Mssql && self.depth <= 0 && self.s[self.i..].starts_with(self.delimiter.as_str()) {
                self.push(self.i);
                self.i += self.delimiter.len();
                self.start = self.i;
                continue;
            }
            match c {
                b'\'' => {
                    let backslash = self.d == Dialect::Mysql || (self.d == Dialect::Postgres && self.is_pg_escape_string());
                    self.skip_quoted(b'\'', backslash);
                }
                b'"' => self.skip_quoted(b'"', self.d == Dialect::Mysql),
                b'`' if matches!(self.d, Dialect::Mysql | Dialect::Sqlite) => self.skip_quoted(b'`', false),
                b'[' if matches!(self.d, Dialect::Mssql | Dialect::Sqlite) => self.skip_until_close(b']'),
                b'-' if self.peek(1) == Some(b'-')
                    && (self.d != Dialect::Mysql || self.peek(2).is_none_or(|c| c.is_ascii_whitespace())) =>
                {
                    self.i = self.line_end(self.i);
                }
                b'#' if self.d == Dialect::Mysql => self.i = self.line_end(self.i),
                b'/' if self.peek(1) == Some(b'*') => self.skip_block_comment(),
                b'$' if self.d == Dialect::Postgres => self.skip_dollar_quote(),
                c if is_word_start(c) => self.word(),
                c if c.is_ascii_digit() => {
                    while self.i < n && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'.' || self.b[self.i] == b'_') {
                        self.i += 1;
                    }
                }
                _ => self.i += 1,
            }
        }
        self.push(n);
        self.out
    }

    fn is_pg_escape_string(&self) -> bool {
        self.i > 0
            && matches!(self.b[self.i - 1], b'e' | b'E')
            && (self.i < 2 || !is_word_char(self.b[self.i - 2], self.d))
    }

    fn skip_quoted(&mut self, q: u8, backslash: bool) {
        let n = self.b.len();
        self.i += 1;
        while self.i < n {
            let c = self.b[self.i];
            if backslash && c == b'\\' {
                self.i += 2;
                continue;
            }
            if c == q {
                if self.peek(1) == Some(q) {
                    self.i += 2;
                    continue;
                }
                self.i += 1;
                return;
            }
            self.i += 1;
        }
        self.i = n;
    }

    fn skip_until_close(&mut self, close: u8) {
        let n = self.b.len();
        self.i += 1;
        while self.i < n {
            if self.b[self.i] == close {
                if self.peek(1) == Some(close) {
                    self.i += 2;
                    continue;
                }
                self.i += 1;
                return;
            }
            self.i += 1;
        }
    }

    fn skip_block_comment(&mut self) {
        let n = self.b.len();
        let nested = self.d == Dialect::Postgres || self.d == Dialect::Mssql;
        let mut depth = 0;
        while self.i < n {
            if self.b[self.i] == b'/' && self.peek(1) == Some(b'*') {
                if depth == 0 || nested {
                    depth += 1;
                }
                self.i += 2;
            } else if self.b[self.i] == b'*' && self.peek(1) == Some(b'/') {
                depth -= 1;
                self.i += 2;
                if depth == 0 {
                    return;
                }
            } else {
                self.i += 1;
            }
        }
    }

    fn skip_dollar_quote(&mut self) {
        // $tag$ ... $tag$ where tag is empty or an identifier not starting with a digit.
        let n = self.b.len();
        let mut j = self.i + 1;
        if j < n && (self.b[j].is_ascii_alphabetic() || self.b[j] == b'_' || self.b[j] >= 0x80) {
            while j < n && (self.b[j].is_ascii_alphanumeric() || self.b[j] == b'_' || self.b[j] >= 0x80) {
                j += 1;
            }
        }
        if j < n && self.b[j] == b'$' {
            let tag = &self.s[self.i..=j];
            let body_start = j + 1;
            match self.s[body_start..].find(tag) {
                Some(p) => self.i = body_start + p + tag.len(),
                None => self.i = n,
            }
        } else {
            // positional parameter ($1) or a lone '$'
            self.i += 1;
        }
    }

    fn read_word(&mut self) -> &'a str {
        let st = self.i;
        // A custom delimiter such as `$$` may directly follow a word (`END$$`).
        while self.i < self.b.len() && is_word_char(self.b[self.i], self.d) && (self.i == st || !self.s[self.i..].starts_with(self.delimiter.as_str())) {
            self.i += 1;
        }
        &self.s[st..self.i]
    }

    /// Peek at the next word after whitespace/comments without consuming input.
    fn next_word_upper(&self) -> String {
        let mut j = self.i;
        let n = self.b.len();
        while j < n && self.b[j].is_ascii_whitespace() {
            j += 1;
        }
        let st = j;
        while j < n && is_word_char(self.b[j], self.d) {
            j += 1;
        }
        self.s[st..j].to_ascii_uppercase()
    }

    fn word(&mut self) {
        let w = self.read_word().to_ascii_uppercase();
        if self.first_word.is_none() {
            self.first_word = Some(w.clone());
        }
        // Only CREATE statements (routines, triggers, events, BEGIN ATOMIC
        // functions) can contain bodies with embedded semicolons.
        if self.delimiter != ";" || self.d == Dialect::Mssql || self.first_word.as_deref() != Some("CREATE") {
            return;
        }
        match w.as_str() {
            "BEGIN" => {
                // `BEGIN` followed by ';' / WORK / TRANSACTION is not a block
                let next = self.next_word_upper();
                if !matches!(next.as_str(), "WORK" | "TRANSACTION") {
                    self.depth += 1;
                }
            }
            "CASE" => self.depth += 1,
            "END" => {
                let next = self.next_word_upper();
                match next.as_str() {
                    "IF" | "LOOP" | "WHILE" | "REPEAT" | "FOR" => {
                        self.skip_ws();
                        self.read_word();
                    }
                    "CASE" => {
                        self.skip_ws();
                        self.read_word();
                        self.depth -= 1;
                    }
                    _ => self.depth -= 1,
                }
            }
            _ => {}
        }
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
}

/// True if `sql` contains nothing but comments and whitespace.
fn is_only_comments(d: Dialect, sql: &str) -> bool {
    strip_leading_comments(d, sql).is_empty()
}

/// Remove leading whitespace and comments.
pub fn strip_leading_comments(d: Dialect, sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") || (d == Dialect::Mysql && s.starts_with('#')) {
            s = match s.find('\n') {
                Some(p) => s[p + 1..].trim_start(),
                None => "",
            };
        } else if s.starts_with("/*") {
            s = match s[2..].find("*/") {
                Some(p) => s[p + 4..].trim_start(),
                None => "",
            };
        } else {
            return s;
        }
    }
}

/// First keyword of a statement, uppercased (skips comments and opening parens).
pub fn first_keyword(d: Dialect, sql: &str) -> String {
    let s = strip_leading_comments(d, sql).trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>().to_ascii_uppercase()
}

/// Heuristic: can this statement produce a result set (used to fetch column
/// metadata for queries that returned no rows)?
pub fn may_return_rows(d: Dialect, sql: &str) -> bool {
    let kw = first_keyword(d, sql);
    matches!(
        kw.as_str(),
        "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "PRAGMA" | "CALL" | "EXEC" | "EXECUTE" | "FETCH"
    ) || sql.to_ascii_uppercase().contains("RETURNING")
}

/// Pure queries whose execution may be aborted once enough rows were read
/// (no side effects to lose: plain SELECT/WITH/VALUES/TABLE/SHOW).
pub fn can_stop_early(d: Dialect, sql: &str) -> bool {
    matches!(first_keyword(d, sql).as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW") && is_read_only_statement(d, sql)
}

/// Statements allowed on a read-only connection.
pub fn is_read_only_statement(d: Dialect, sql: &str) -> bool {
    let kw = first_keyword(d, sql);
    let upper = sql.to_ascii_uppercase();
    // Anything that switches the session or transaction back to read-write, in any statement:
    // `BEGIN READ WRITE`, `START TRANSACTION READ WRITE`, `SELECT set_config('default_transaction_read_only', …)`.
    let spaced = upper.split_whitespace().collect::<Vec<_>>().join(" ");
    if spaced.contains("READ WRITE") || contains_word(&upper, "SET_CONFIG") || upper.contains("TRANSACTION_READ_ONLY") {
        return false;
    }
    match kw.as_str() {
        // `SELECT … INTO new_table` / `INTO OUTFILE` write; the keyword may follow a newline
        "SELECT" => !contains_word(&upper, "INTO"),
        "WITH" => !["INSERT", "UPDATE", "DELETE", "MERGE", "INTO"].iter().any(|k| contains_word(&upper, k)),
        "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE" | "USE" | "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "END" | "HELP" => {
            !(kw == "EXPLAIN" && contains_word(&upper, "ANALYZE") && ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|k| contains_word(&upper, k)))
        }
        // PRAGMA writes are blocked by opening SQLite read-only.
        "PRAGMA" => d == Dialect::Sqlite,
        // Session settings are fine, but not ones that would lift read-only mode.
        // Server-wide and identity changes are writes too (MySQL `SET GLOBAL`, `SET PASSWORD`, PG `SET ROLE`).
        "SET" => !upper.contains("READ") && !["GLOBAL", "PERSIST", "PERSIST_ONLY", "PASSWORD", "ROLE", "AUTHORIZATION"].iter().any(|k| contains_word(&upper, k)),
        _ => false,
    }
}

fn contains_word(upper: &str, word: &str) -> bool {
    upper
        .match_indices(word)
        .any(|(i, _)| {
            let before = upper[..i].chars().next_back();
            let after = upper[i + word.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
}

/// Error for a read-only connection receiving a write statement.
pub fn read_only_error(stmt_no: usize) -> AppError {
    AppError::msg(format!(
        "Statement {stmt_no}: this connection is read-only; only SELECT/SHOW/EXPLAIN style statements are allowed"
    ))
}

/// Error message for statement `n` (1-based) of a script.
pub fn statement_error(n: usize, total: usize, line: usize, sql: &str, err: impl std::fmt::Display) -> AppError {
    let preview: String = sql.chars().take(200).collect();
    let ellipsis = if sql.chars().count() > 200 { "…" } else { "" };
    if total > 1 {
        AppError::msg(format!("Error in statement {n} of {total} (line {line}): {err}\n\n{preview}{ellipsis}"))
    } else {
        AppError::msg(format!("{err}"))
    }
}

/// Translate a 1-based character position within `sql` into "line L, column C".
pub fn describe_position(sql: &str, pos: usize) -> String {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in sql.chars().enumerate() {
        if i + 1 >= pos {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    format!("line {line}, column {col}")
}

// ---------------------------------------------------------------------------
// SELECT / DML generation
// ---------------------------------------------------------------------------

pub fn where_clause(filter: Option<&str>) -> String {
    match filter.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!(" WHERE ({f})"),
        None => String::new(),
    }
}

pub fn order_by_clause(d: Dialect, sort: &[SortSpec]) -> String {
    if sort.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = sort
        .iter()
        .map(|s| format!("{}{}", quote_ident(d, &s.column), if s.desc { " DESC" } else { " ASC" }))
        .collect();
    format!(" ORDER BY {}", parts.join(", "))
}

/// `SELECT * FROM <table> [WHERE ...] [ORDER BY ...] <page>` for grid browsing.
/// `default_order` is used when `sort` is empty (e.g. the primary key) so that
/// paging is stable.
pub fn select_page_sql(
    d: Dialect,
    table: &TableRef,
    filter: Option<&str>,
    sort: &[SortSpec],
    default_order: &[String],
    limit: u64,
    offset: u64,
) -> String {
    let mut sql = format!("SELECT * FROM {}{}", qualified_name(d, table), where_clause(filter));
    let order = if sort.is_empty() {
        let def: Vec<SortSpec> = default_order.iter().map(|c| SortSpec { column: c.clone(), desc: false }).collect();
        order_by_clause(d, &def)
    } else {
        order_by_clause(d, sort)
    };
    sql.push_str(&order);
    match d {
        Dialect::Mssql => {
            if order.is_empty() {
                sql.push_str(" ORDER BY (SELECT NULL)");
            }
            sql.push_str(&format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY"));
        }
        _ => sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
    }
    sql
}

pub fn count_sql(d: Dialect, table: &TableRef, filter: Option<&str>) -> String {
    format!("SELECT COUNT(*) FROM {}{}", qualified_name(d, table), where_clause(filter))
}

/// Bind placeholder for parameter `n` (1-based).
pub fn placeholder(d: Dialect, n: usize) -> String {
    match d {
        Dialect::Postgres => format!("${n}"),
        Dialect::Mssql => format!("@P{n}"),
        _ => "?".into(),
    }
}

/// A DML statement with positional parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct DmlStatement {
    pub sql: String,
    pub params: Vec<Value>,
    /// Column each parameter belongs to (same order as `params`).
    pub param_columns: Vec<String>,
    /// UPDATE/DELETE: the statement must affect exactly one row.
    pub expect_one: bool,
}

/// Build a parameterized INSERT/UPDATE/DELETE for a grid change.
///
/// `wrap(column, placeholder)` lets a driver decorate placeholders, e.g. add a
/// cast (`CAST($1 AS integer)`) for Postgres. Key columns with a NULL value are
/// matched with `IS NULL`.
pub fn build_change(
    d: Dialect,
    table: &TableRef,
    change: &RowChange,
    wrap: &dyn Fn(&str, String) -> String,
) -> Result<DmlStatement, AppError> {
    let mut params: Vec<Value> = Vec::new();
    let mut param_columns: Vec<String> = Vec::new();
    let (sql, expect_one) = build_with(d, table, change, &mut |col, v| {
        params.push(v.clone());
        param_columns.push(col.to_string());
        wrap(col, placeholder(d, params.len()))
    })?;
    Ok(DmlStatement { sql, params, param_columns, expect_one })
}

/// Same as [`build_change`] but with values inlined as literals (for scripts / previews).
#[allow(dead_code)]
pub fn build_change_literal(d: Dialect, table: &TableRef, change: &RowChange) -> Result<String, AppError> {
    Ok(build_with(d, table, change, &mut |_, v| literal(d, v))?.0)
}

fn build_with(
    d: Dialect,
    table: &TableRef,
    change: &RowChange,
    render: &mut dyn FnMut(&str, &Value) -> String,
) -> Result<(String, bool), AppError> {
    let qn = qualified_name(d, table);
    let where_of = |key: &Map<String, Value>, render: &mut dyn FnMut(&str, &Value) -> String| {
        if key.is_empty() {
            return Err(AppError::msg("Cannot identify the row: the table has no primary key"));
        }
        let conds: Vec<String> = key
            .iter()
            .map(|(k, v)| {
                if v.is_null() {
                    format!("{} IS NULL", quote_ident(d, k))
                } else {
                    format!("{} = {}", quote_ident(d, k), render(k, v))
                }
            })
            .collect();
        Ok(conds.join(" AND "))
    };
    Ok(match change {
        RowChange::Insert { values } if values.is_empty() => match d {
            Dialect::Mysql => (format!("INSERT INTO {qn} () VALUES ()"), false),
            _ => (format!("INSERT INTO {qn} DEFAULT VALUES"), false),
        },
        RowChange::Insert { values } => {
            let cols: Vec<String> = values.keys().map(|k| quote_ident(d, k)).collect();
            let vals: Vec<String> = values.iter().map(|(k, v)| render(k, v)).collect();
            (format!("INSERT INTO {qn} ({}) VALUES ({})", cols.join(", "), vals.join(", ")), false)
        }
        RowChange::Update { key, values } => {
            if values.is_empty() {
                return Err(AppError::msg("Update without any changed column"));
            }
            let sets: Vec<String> = values.iter().map(|(k, v)| format!("{} = {}", quote_ident(d, k), render(k, v))).collect();
            let w = where_of(key, render)?;
            (format!("UPDATE {qn} SET {} WHERE {w}", sets.join(", ")), true)
        }
        RowChange::Delete { key } => {
            let w = where_of(key, render)?;
            (format!("DELETE FROM {qn} WHERE {w}"), true)
        }
    })
}

/// Error when an UPDATE/DELETE did not affect exactly one row.
pub fn row_count_error(change_no: usize, sql: &str, affected: u64) -> AppError {
    let what = if sql.starts_with("UPDATE") { "update" } else { "delete" };
    AppError::msg(format!(
        "Change {change_no}: {what} affected {affected} rows (expected exactly 1); all changes were rolled back. \
         The row may have been modified or deleted by someone else, or the key is not unique."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn split(d: Dialect, s: &str) -> Vec<String> {
        split_statements(d, s)
    }

    #[test]
    fn split_basic() {
        assert_eq!(split(Dialect::Postgres, "select 1; select 2;"), vec!["select 1", "select 2"]);
        assert_eq!(split(Dialect::Postgres, "select 1"), vec!["select 1"]);
        assert_eq!(split(Dialect::Postgres, " ; ;\n"), Vec::<String>::new());
    }

    #[test]
    fn split_quotes() {
        assert_eq!(split(Dialect::Postgres, "select 'a;b'; select \"x;y\""), vec!["select 'a;b'", "select \"x;y\""]);
        assert_eq!(split(Dialect::Postgres, "select 'it''s;'; select 2"), vec!["select 'it''s;'", "select 2"]);
        assert_eq!(split(Dialect::Postgres, r"select E'a\';b'; select 2"), vec![r"select E'a\';b'", "select 2"]);
        // standard strings: backslash is not an escape
        assert_eq!(split(Dialect::Postgres, r"select 'a\'; select 2"), vec![r"select 'a\'", "select 2"]);
        assert_eq!(split(Dialect::Mysql, r"select 'a\';b'; select `c;d`"), vec![r"select 'a\';b'", "select `c;d`"]);
        assert_eq!(split(Dialect::Mysql, r#"select "a\";b"; select 2"#), vec![r#"select "a\";b""#, "select 2"]);
        assert_eq!(split(Dialect::Mssql, "select [a;b]; select 2\nGO\nselect 3"), vec!["select [a;b]; select 2", "select 3"]);
        assert_eq!(split(Dialect::Sqlite, "select [a;b]; select `c;d`"), vec!["select [a;b]", "select `c;d`"]);
    }

    #[test]
    fn split_comments() {
        assert_eq!(split(Dialect::Postgres, "select 1 -- x;y\n; select 2"), vec!["select 1 -- x;y", "select 2"]);
        assert_eq!(split(Dialect::Postgres, "select /* a; /* nested; */ b; */ 1; select 2"), vec!["select /* a; /* nested; */ b; */ 1", "select 2"]);
        assert_eq!(split(Dialect::Mysql, "select 1 # x;y\n; select 2"), vec!["select 1 # x;y", "select 2"]);
        assert_eq!(split(Dialect::Mysql, "select 1 /* ; */; select 2"), vec!["select 1 /* ; */", "select 2"]);
        // MySQL needs whitespace after --
        assert_eq!(split(Dialect::Mysql, "select 1--1; select 2"), vec!["select 1--1", "select 2"]);
        // comment-only trailing statement is dropped
        assert_eq!(split(Dialect::Postgres, "select 1; -- done\n/* end */"), vec!["select 1"]);
        // '#' is not a comment in Postgres
        assert_eq!(split(Dialect::Postgres, "select '{1}'::jsonb #> '{}'; select 2").len(), 2);
    }

    #[test]
    fn split_dollar_quotes() {
        let f = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\nselect f();";
        let s = split(Dialect::Postgres, f);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[0].ends_with("LANGUAGE plpgsql"));
        let f = "DO $body$ BEGIN PERFORM 1; RAISE NOTICE '$$;'; END $body$; select $1";
        let s = split(Dialect::Postgres, f);
        assert_eq!(s, vec!["DO $body$ BEGIN PERFORM 1; RAISE NOTICE '$$;'; END $body$", "select $1"]);
        // identifiers containing $ are not dollar quotes
        assert_eq!(split(Dialect::Postgres, "select a$b$c from t; select 2").len(), 2);
    }

    #[test]
    fn split_pg_begin_atomic() {
        let f = "CREATE FUNCTION f(a int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN a > 0 THEN 1 ELSE 0 END; SELECT 2; END; select 3";
        let s = split(Dialect::Postgres, f);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[1], "select 3");
        // transaction control is unaffected
        assert_eq!(split(Dialect::Postgres, "BEGIN; select 1; END;"), vec!["BEGIN", "select 1", "END"]);
    }

    #[test]
    fn split_mysql_delimiter() {
        let script = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;\nCALL p();\nselect 'x';";
        let s = split(Dialect::Mysql, script);
        assert_eq!(s, vec!["CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END", "CALL p()", "select 'x'"]);
        let script = "delimiter //\nselect 1//\nselect 2 //";
        assert_eq!(split(Dialect::Mysql, script), vec!["select 1", "select 2"]);
    }

    #[test]
    fn split_mysql_body_without_delimiter() {
        let script = "CREATE PROCEDURE p(IN x INT)\nBEGIN\n  IF x > 0 THEN SELECT 1; ELSE SELECT 2; END IF;\n  CASE x WHEN 1 THEN SELECT 3; END CASE;\n  l: LOOP LEAVE l; END LOOP l;\nEND;\nCALL p(1);";
        let s = split(Dialect::Mysql, script);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[1], "CALL p(1)");
        let trig = "CREATE TRIGGER t AFTER INSERT ON a FOR EACH ROW BEGIN INSERT INTO b VALUES (1); UPDATE c SET x = 1; END; select 1";
        assert_eq!(split(Dialect::Sqlite, trig).len(), 2);
        // CASE expressions in normal statements don't break splitting
        assert_eq!(split(Dialect::Mysql, "select case when 1 then 2 end; select 3").len(), 2);
        assert_eq!(split(Dialect::Mysql, "create view v as select case when 1 then 2 end x; select 3").len(), 2);
    }

    #[test]
    fn split_mssql_go() {
        let script = "create table t (a int);\ngo\ninsert into t values (1); select * from t\nGO 2\n  go  \nselect 'go'\n";
        let s = split(Dialect::Mssql, script);
        assert_eq!(s, vec!["create table t (a int);", "insert into t values (1); select * from t", "select 'go'"]);
        // 'GO' inside a string spanning lines is not a separator
        let s = split(Dialect::Mssql, "select 'a\nGO\nb'");
        assert_eq!(s.len(), 1);
        // a column called "go" on its own line within a statement would split; but "gopher" must not
        assert_eq!(split(Dialect::Mssql, "select\ngopher from t").len(), 1);
    }

    #[test]
    fn split_offsets() {
        let script = "select 1;\n\n  select 2;";
        let s = split_script(Dialect::Postgres, script);
        assert_eq!(s[1].offset, 13);
        assert_eq!(s[1].line(script), 3);
    }

    #[test]
    fn literal_escaping() {
        assert_eq!(literal(Dialect::Postgres, &json!(null)), "NULL");
        assert_eq!(literal(Dialect::Postgres, &json!(true)), "TRUE");
        assert_eq!(literal(Dialect::Mysql, &json!(false)), "0");
        assert_eq!(literal(Dialect::Sqlite, &json!(12.5)), "12.5");
        assert_eq!(literal(Dialect::Postgres, &json!("it's")), "'it''s'");
        assert_eq!(literal(Dialect::Postgres, &json!(r"a\'b")), r"E'a\\''b'");
        assert_eq!(literal(Dialect::Mysql, &json!(r"a\'; DROP TABLE x; --")), r"'a\\''; DROP TABLE x; --'");
        assert_eq!(literal(Dialect::Mssql, &json!("x'y")), "N'x''y'");
        assert_eq!(literal(Dialect::Sqlite, &json!("x'y")), "'x''y'");
        assert_eq!(literal(Dialect::Postgres, &json!({"a": "b'c"})), r#"'{"a":"b''c"}'"#);
    }

    /// Every rendered string literal must be a single, complete literal token
    /// according to our own splitter's quote rules.
    #[test]
    fn literal_is_single_token() {
        let nasty = ["'", "''", "\\", "\\'", "'\\", "a\\\\'b", "\"", "`", "$$", "';--", "\n'\n", "\\\\\\'"];
        for d in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite, Dialect::Mssql] {
            for s in nasty {
                let sql = format!("select {}; select 2", literal(d, &json!(s)));
                let parts = split(d, &sql);
                assert_eq!(parts.len(), if d == Dialect::Mssql { 1 } else { 2 }, "{d:?} {s:?} -> {sql}");
            }
        }
    }

    #[test]
    fn qualified_names() {
        let t = TableRef { database: Some("db".into()), schema: Some("public".into()), name: "t\"x".into() };
        assert_eq!(qualified_name(Dialect::Postgres, &t), "\"public\".\"t\"\"x\"");
        assert_eq!(qualified_name(Dialect::Mysql, &t), "`db`.`t\"x`");
        assert_eq!(qualified_name(Dialect::Mssql, &t), "[db].[public].[t\"x]");
        let t = TableRef { database: Some("main".into()), schema: None, name: "a".into() };
        assert_eq!(qualified_name(Dialect::Sqlite, &t), "\"main\".\"a\"");
    }

    #[test]
    fn dml_generation() {
        let t = TableRef { database: None, schema: Some("s".into()), name: "t".into() };
        let mut key = Map::new();
        key.insert("id".into(), json!(1));
        key.insert("k2".into(), Value::Null);
        let mut values = Map::new();
        values.insert("name".into(), json!("x"));
        let up = build_change(Dialect::Postgres, &t, &RowChange::Update { key: key.clone(), values: values.clone() }, &|_, p| p).unwrap();
        assert_eq!(up.sql, "UPDATE \"s\".\"t\" SET \"name\" = $1 WHERE \"id\" = $2 AND \"k2\" IS NULL");
        assert_eq!(up.params, vec![json!("x"), json!(1)]);
        assert!(up.expect_one);
        let del = build_change(Dialect::Mysql, &t, &RowChange::Delete { key }, &|_, p| p).unwrap();
        assert_eq!(del.sql, "DELETE FROM `t` WHERE `id` = ? AND `k2` IS NULL");
        let ins = build_change(Dialect::Postgres, &t, &RowChange::Insert { values }, &|c, p| format!("CAST({p} AS {c})")).unwrap();
        assert_eq!(ins.sql, "INSERT INTO \"s\".\"t\" (\"name\") VALUES (CAST($1 AS name))");
        assert!(build_change(Dialect::Postgres, &t, &RowChange::Delete { key: Map::new() }, &|_, p| p).is_err());
        let lit = build_change_literal(Dialect::Mysql, &t, &RowChange::Delete { key: [("id".to_string(), json!("a?b"))].into_iter().collect() }).unwrap();
        assert_eq!(lit, "DELETE FROM `t` WHERE `id` = 'a?b'");
    }

    #[test]
    fn page_sql() {
        let t = TableRef { database: Some("d".into()), schema: None, name: "t".into() };
        let sort = vec![SortSpec { column: "a".into(), desc: true }];
        assert_eq!(
            select_page_sql(Dialect::Mysql, &t, Some("x > 1"), &sort, &[], 10, 20),
            "SELECT * FROM `d`.`t` WHERE (x > 1) ORDER BY `a` DESC LIMIT 10 OFFSET 20"
        );
        assert_eq!(select_page_sql(Dialect::Mysql, &t, Some("  "), &[], &["id".into()], 5, 0), "SELECT * FROM `d`.`t` ORDER BY `id` ASC LIMIT 5 OFFSET 0");
    }

    #[test]
    fn keywords() {
        assert_eq!(first_keyword(Dialect::Mysql, "  -- hi\n# x\n/* c */ (select 1)"), "SELECT");
        assert!(is_read_only_statement(Dialect::Postgres, "select * from t"));
        assert!(!is_read_only_statement(Dialect::Postgres, "with x as (delete from t returning *) select * from x"));
        assert!(!is_read_only_statement(Dialect::Postgres, "update t set a = 1"));
        assert!(!is_read_only_statement(Dialect::Postgres, "set default_transaction_read_only = off"));
        assert!(is_read_only_statement(Dialect::Postgres, "set search_path = public"));
        assert!(!is_read_only_statement(Dialect::Postgres, "begin read  write"));
        assert!(!is_read_only_statement(Dialect::Mysql, "START TRANSACTION READ WRITE"));
        assert!(!is_read_only_statement(Dialect::Postgres, "select set_config('default_transaction_read_only', 'off', false)"));
        assert!(!is_read_only_statement(Dialect::Postgres, "select *\ninto t2 from t"));
        assert!(!is_read_only_statement(Dialect::Mysql, "select * from t into\toutfile '/tmp/x'"));
        assert!(!is_read_only_statement(Dialect::Mysql, "set global max_connections = 1"));
        assert!(!is_read_only_statement(Dialect::Mysql, "SET @@GLOBAL.read_only_x = 0"));
        assert!(!is_read_only_statement(Dialect::Mysql, "set password = 'x'"));
        assert!(!is_read_only_statement(Dialect::Postgres, "set role postgres"));
        assert!(is_read_only_statement(Dialect::Postgres, "begin"));
        assert!(is_read_only_statement(Dialect::Postgres, "select interval_col from intervals"));
        assert!(may_return_rows(Dialect::Postgres, "insert into t values (1) returning id"));
        assert!(!may_return_rows(Dialect::Postgres, "insert into t values (1)"));
    }

    #[test]
    fn binary_rendering() {
        assert_eq!(binary_to_value(&[0, 255]), json!("\\x00ff"));
        let big = vec![1u8; MAX_BINARY_DISPLAY + 1];
        let v = binary_to_value(&big);
        assert!(v.as_str().unwrap().ends_with('…'));
        assert_eq!(parse_hex_binary("\\x00ff"), Some(vec![0, 255]));
        assert_eq!(parse_hex_binary("\\x00ff…"), None);
    }
}
