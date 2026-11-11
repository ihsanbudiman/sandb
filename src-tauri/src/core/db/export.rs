//! Result export: re-runs one plain-read statement without a row cap and streams it to a file,
//! as CSV or as INSERT statements.

use std::path::{Path, PathBuf};

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Column, Database, Executor, Row};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use crate::core::connections::{self, Engine, Env};
use crate::core::db::dump::{self, Inserts};
use crate::core::db::pool::DbPool;
use crate::core::db::query::{mysql_cell, pg_cell, sqlite_cell};
use crate::core::db::session::{self, Conn};
use crate::core::error::AppError;
use crate::core::mongo;
use crate::core::sql::{facts, split};
use crate::core::state::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub conn_id: String,
    #[serde(default)]
    pub database: Option<String>,
    pub sql: String,
    /// Production editor tab: the export reads inside the tab's open transaction.
    #[serde(default)]
    pub tab_id: Option<String>,
}

/// A result as INSERT statements into `table` (in `schema`, the PG schema or MySQL database,
/// when set), for loading the rows into the same table in another database.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlExportRequest {
    #[serde(flatten)]
    pub query: ExportRequest,
    #[serde(default)]
    pub schema: Option<String>,
    pub table: String,
    #[serde(default)]
    pub options: SqlOptions,
}

/// A MongoDB read as Extended JSON, one document after the other.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentExportRequest {
    #[serde(flatten)]
    pub query: ExportRequest,
    pub format: DocumentFormat,
    pub ejson: EjsonFlavor,
}

/// How the documents are separated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFormat {
    /// One JSON array of documents.
    Json,
    /// One document per line (newline-delimited JSON).
    Ndjson,
}

/// How BSON values appear in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EjsonFlavor {
    /// Numbers as JSON numbers where the type survives, everything else as `{"$type": ...}`.
    Relaxed,
    /// Every value keeps its type.
    Canonical,
}

/// What a row already in the target does to its INSERT.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InsertMode {
    /// Plain INSERT: the statement fails.
    #[default]
    Insert,
    /// The row is skipped: PG `ON CONFLICT DO NOTHING`, MySQL `INSERT IGNORE`, SQLite
    /// `INSERT OR IGNORE`.
    Skip,
    /// The row is updated: `ON CONFLICT (key) DO UPDATE`, MySQL `ON DUPLICATE KEY UPDATE`.
    Upsert,
}

/// Column indexes are positions in the result, so repeated names stay apart.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SqlOptions {
    /// Result columns to write, in this order; `None` writes every column.
    pub columns: Option<Vec<usize>>,
    pub mode: InsertMode,
    /// Conflict key of an upsert (result columns). MySQL finds the key itself and only
    /// leaves these columns out of the update.
    pub key: Vec<usize>,
    pub rows_per_statement: usize,
    /// `BEGIN;` … `COMMIT;` around the statements, so a failed row loads nothing.
    pub transaction: bool,
    /// The `-- sandb SQL export` comment block at the top.
    pub header: bool,
}

impl Default for SqlOptions {
    fn default() -> Self {
        Self {
            columns: None,
            mode: InsertMode::Insert,
            key: Vec::new(),
            rows_per_statement: 100,
            transaction: false,
            header: true,
        }
    }
}

const MAX_ROWS_PER_STATEMENT: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// The user closed the save dialog; nothing was written.
    pub cancelled: bool,
    pub rows: u64,
    pub path: String,
}
/// RFC 4180: quote when the field has a comma, quote, CR or LF; double inner quotes.
pub fn escape(field: &str) -> std::borrow::Cow<'_, str> {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\"")).into()
    } else {
        field.into()
    }
}

/// NULL → empty, bytes → base64, JSON values → compact JSON, the rest → their text.
pub fn field(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(o) => match o.get("$bytes") {
            Some(Value::String(b64)) if o.contains_key("len") => b64.clone(),
            _ => v.to_string(),
        },
        Value::Array(_) => v.to_string(),
    }
}

pub fn line<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut s = fields.into_iter().map(escape).collect::<Vec<_>>().join(",");
    s.push_str("\r\n");
    s
}

type CellFn<DB> = fn(&<DB as Database>::Row, usize, Option<usize>) -> Value;
type SqlCellFn<DB> = fn(&mut String, &<DB as Database>::Row, usize) -> Result<(), AppError>;

/// What the export writes for each row.
enum Format {
    Csv,
    Sql(SqlPlan),
}

struct SqlPlan {
    engine: Engine,
    /// Quoted, maybe qualified, name.
    table: String,
    options: SqlOptions,
}

impl SqlPlan {
    /// The written result columns, and the text before and after the rows of each statement,
    /// for a result with these column names.
    fn parts(
        &self,
        names: &[&str],
        ident: fn(&str) -> String,
    ) -> Result<(Vec<usize>, String, String), AppError> {
        let o = &self.options;
        let cols: Vec<usize> = o
            .columns
            .clone()
            .unwrap_or_else(|| (0..names.len()).collect());
        if let Some(&i) = cols.iter().chain(&o.key).find(|&&i| i >= names.len()) {
            return Err(AppError::invalid(format!(
                "The result has no column {}; run the query again and export",
                i + 1
            )));
        }
        let q = |i: &usize| ident(names[*i]);
        let list = cols.iter().map(q).collect::<Vec<_>>().join(", ");
        let verb = match (o.mode, self.engine) {
            (InsertMode::Skip, Engine::Mysql) => "INSERT IGNORE INTO",
            (InsertMode::Skip, Engine::Sqlite) => "INSERT OR IGNORE INTO",
            _ => "INSERT INTO",
        };
        let head = format!("{verb} {} ({list}) VALUES", self.table);
        let set: Vec<String> = cols
            .iter()
            .filter(|i| !o.key.contains(i))
            .map(|i| {
                let c = q(i);
                match self.engine {
                    Engine::Mysql => format!("{c} = VALUES({c})"),
                    Engine::Pg | Engine::Sqlite | Engine::Mongo => format!("{c} = EXCLUDED.{c}"),
                }
            })
            .collect();
        let key = o.key.iter().map(q).collect::<Vec<_>>().join(", ");
        let tail = match (o.mode, self.engine) {
            (InsertMode::Insert, _)
            | (InsertMode::Skip, Engine::Mysql | Engine::Sqlite | Engine::Mongo) => String::new(),
            (InsertMode::Skip, Engine::Pg) => "\nON CONFLICT DO NOTHING".into(),
            // Every column is in the key: nothing to update.
            (InsertMode::Upsert, Engine::Mysql) if set.is_empty() => {
                let c = q(&cols[0]);
                format!("\nON DUPLICATE KEY UPDATE {c} = {c}")
            }
            (InsertMode::Upsert, Engine::Mysql) => {
                format!("\nON DUPLICATE KEY UPDATE {}", set.join(", "))
            }
            (InsertMode::Upsert, _) if set.is_empty() => {
                format!("\nON CONFLICT ({key}) DO NOTHING")
            }
            (InsertMode::Upsert, _) => {
                format!("\nON CONFLICT ({key}) DO UPDATE SET {}", set.join(", "))
            }
        };
        Ok((cols, head, tail))
    }

    fn begin(&self) -> &'static str {
        match self.engine {
            Engine::Mysql => "START TRANSACTION;\n\n",
            Engine::Pg | Engine::Sqlite | Engine::Mongo => "BEGIN;\n\n",
        }
    }
}

/// How one engine's rows turn into CSV fields or SQL literals.
struct Codec<DB: Database> {
    csv: CellFn<DB>,
    sql: SqlCellFn<DB>,
    ident: fn(&str) -> String,
}

async fn stream<DB>(
    conn: &mut DB::Connection,
    sql: &str,
    out: &mut (impl AsyncWrite + Unpin),
    format: &Format,
    codec: Codec<DB>,
    left: &mut bool,
) -> Result<u64, AppError>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    match format {
        Format::Csv => stream_rows(conn, sql, out, codec.csv, left).await,
        Format::Sql(plan) => stream_inserts(conn, sql, out, plan, &codec, left).await,
    }
}

/// INSERT statements for the rows of `sql`; nothing for an empty result. `left` as for
/// `stream_rows`.
async fn stream_inserts<DB>(
    conn: &mut DB::Connection,
    sql: &str,
    out: &mut (impl AsyncWrite + Unpin),
    plan: &SqlPlan,
    codec: &Codec<DB>,
    left: &mut bool,
) -> Result<u64, AppError>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    let mut stream = sqlx::raw_sql(sql).fetch(&mut *conn);
    let Some(mut row) = stream.try_next().await? else {
        return Ok(0);
    };
    let names: Vec<&str> = row.columns().iter().map(|c| c.name()).collect();
    let (cols, head, tail) = match plan.parts(&names, codec.ident) {
        Ok(p) => p,
        Err(e) => {
            *left = true;
            return Err(e);
        }
    };
    let mut ins = Inserts::with(out, head, tail, plan.options.rows_per_statement);
    let mut buf = String::new();
    loop {
        buf.clear();
        buf.push('(');
        let mut res = Ok(());
        for (n, &i) in cols.iter().enumerate() {
            if n > 0 {
                buf.push_str(", ");
            }
            res = (codec.sql)(&mut buf, &row, i);
            if res.is_err() {
                break;
            }
        }
        buf.push(')');
        if let Err(e) = match res {
            Ok(()) => ins.row(&buf).await,
            Err(e) => Err(e),
        } {
            *left = true;
            return Err(e);
        }
        match stream.try_next().await? {
            Some(next) => row = next,
            None => break,
        }
    }
    ins.finish().await
}

/// `left` is set when the stream was dropped mid-result (a write to `out` failed): the
/// server is still sending rows, so the connection cannot be reused — a session holding it
/// must close.
async fn stream_rows<DB>(
    conn: &mut DB::Connection,
    sql: &str,
    out: &mut (impl AsyncWrite + Unpin),
    cell: CellFn<DB>,
    left: &mut bool,
) -> Result<u64, AppError>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    let mut n = 0u64;
    {
        let mut stream = sqlx::raw_sql(sql).fetch(&mut *conn);
        while let Some(row) = stream.try_next().await? {
            if n == 0 {
                let names: Vec<&str> = row.columns().iter().map(|c| c.name()).collect();
                if let Err(e) = out.write_all(line(names).as_bytes()).await {
                    *left = true;
                    return Err(e.into());
                }
            }
            let fields: Vec<String> = (0..row.len())
                .map(|i| field(&cell(&row, i, None)))
                .collect();
            let bytes = line(fields.iter().map(String::as_str));
            if let Err(e) = out.write_all(bytes.as_bytes()).await {
                *left = true;
                return Err(e.into());
            }
            n += 1;
        }
    }
    if n == 0 {
        // No row carried the column names, so ask the server. Some reads (e.g. SHOW) cannot be
        // described; the file is then empty.
        if let Ok(d) = (&mut *conn).describe(sql).await {
            let names: Vec<&str> = d.columns().iter().map(|c| c.name()).collect();
            if !names.is_empty() {
                out.write_all(line(names).as_bytes()).await?;
            }
        }
    }
    Ok(n)
}
/// Rejects anything but one plain read, so the export never repeats a write. Call it before
/// asking the user for a file.
pub fn check(state: &AppState, req: &ExportRequest) -> Result<(), AppError> {
    let conn = connections::get(state, &req.conn_id)?;
    if conn.engine == Engine::Mongo {
        return mongo::export::check(&mongo::parse::parse(&req.sql)?);
    }
    match split::split(conn.engine, &req.sql).as_slice() {
        [sql] if facts::is_plain_read_sql(conn.engine, sql) => Ok(()),
        _ => Err(AppError::invalid(
            "Export needs exactly one read-only statement (a SELECT)",
        )),
    }
}

/// `check`, plus a MongoDB connection: Extended JSON is a MongoDB feature. Call it before asking
/// the user for a file.
pub fn check_documents(state: &AppState, req: &DocumentExportRequest) -> Result<(), AppError> {
    let conn = connections::get(state, &req.query.conn_id)?;
    if conn.engine != Engine::Mongo {
        return Err(AppError::invalid(
            "Extended JSON export needs a MongoDB connection",
        ));
    }
    mongo::export::check(&mongo::parse::parse(&req.query.sql)?)
}

/// `check`, plus a table name and options that make sense for the connection's engine.
pub fn check_sql(state: &AppState, req: &SqlExportRequest) -> Result<(), AppError> {
    check(state, &req.query)?;
    let engine = connections::get(state, &req.query.conn_id)?.engine;
    if engine == Engine::Mongo {
        return Err(AppError::unsupported_mongo());
    }
    if req.table.trim().is_empty() {
        return Err(AppError::invalid(
            "Name the table for the INSERT statements",
        ));
    }
    let o = &req.options;
    if !(1..=MAX_ROWS_PER_STATEMENT).contains(&o.rows_per_statement) {
        return Err(AppError::invalid(format!(
            "Rows per statement must be from 1 to {MAX_ROWS_PER_STATEMENT}"
        )));
    }
    let unique = |v: &[usize]| {
        let mut s = v.to_vec();
        s.sort_unstable();
        s.dedup();
        s.len() == v.len()
    };
    if let Some(cols) = &o.columns {
        if cols.is_empty() {
            return Err(AppError::invalid("Choose at least one column to export"));
        }
        if !unique(cols) {
            return Err(AppError::invalid("A column is chosen twice"));
        }
    }
    if o.mode == InsertMode::Upsert {
        if !unique(&o.key) {
            return Err(AppError::invalid("A key column is chosen twice"));
        }
        if engine != Engine::Mysql && o.key.is_empty() {
            return Err(AppError::invalid(
                "Choose the key columns that find an existing row for the upsert",
            ));
        }
        if let Some(cols) = &o.columns
            && o.key.iter().any(|k| !cols.contains(k))
        {
            return Err(AppError::invalid("Export every key column of the upsert"));
        }
    }
    Ok(())
}

/// Hidden sibling of `path`, so the final rename stays on one file system.
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.part", uuid::Uuid::new_v4()))
}

/// Streams the result as CSV to a temp file next to `path`, then renames it over `path`. On
/// failure only the temp file is removed; an existing file at `path` is never touched.
pub async fn export_csv(
    state: &AppState,
    req: ExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let conn = connections::get(state, &req.conn_id)?;
    if conn.engine == Engine::Mongo {
        return mongo::export::export_csv(state, &conn, req, path).await;
    }
    export(state, req, Format::Csv, path).await
}

/// As `export_csv`, as Extended JSON documents of the read.
pub async fn export_documents(
    state: &AppState,
    req: DocumentExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let conn = connections::get(state, &req.query.conn_id)?;
    mongo::export::export_documents(state, &conn, req, path).await
}

/// As `export_csv`, as INSERT statements shaped by `req.options`.
pub async fn export_sql(
    state: &AppState,
    req: SqlExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    check_sql(state, &req)?;
    let engine = connections::get(state, &req.query.conn_id)?.engine;
    let quote = match engine {
        Engine::Mysql => dump::quote_bt,
        Engine::Pg | Engine::Sqlite | Engine::Mongo => dump::quote_dq,
    };
    let table = match req.schema.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("{}.{}", quote(s), quote(&req.table)),
        None => quote(&req.table),
    };
    let mut options = req.options;
    // The key only matters to an upsert.
    if options.mode != InsertMode::Upsert {
        options.key.clear();
    }
    let plan = SqlPlan {
        engine,
        table,
        options,
    };
    export(state, req.query, Format::Sql(plan), path).await
}

async fn export(
    state: &AppState,
    req: ExportRequest,
    format: Format,
    path: &Path,
) -> Result<ExportResult, AppError> {
    check(state, &req)?;
    if !path.is_absolute() {
        return Err(AppError::invalid("Choose a file to export to"));
    }
    let conn = connections::get(state, &req.conn_id)?;
    let sql = split::split(conn.engine, &req.sql).remove(0);
    let tmp = temp_path(path);
    let file = tokio::fs::File::create(&tmp).await?;
    let mut out = BufWriter::new(file);
    // A production tab exports what its open transaction sees: the same session the run used.
    // Without one (no open transaction, or a session on another target) the pool's view is
    // the same committed data. Read-only connections never hold a session.
    let database = connections::target_database(&conn, req.database.as_deref());
    let mut session = None;
    if conn.env == Env::Production
        && !conn.read_only
        && let Some(tab_id) = req.tab_id.as_deref()
        && let Some(s) = state.sessions.take(tab_id)?
    {
        if s.conn_id == conn.id && s.database == database {
            session = Some((tab_id.to_string(), s));
        } else {
            state.sessions.put(tab_id, s).await;
        }
    }
    // Set when the export dropped its row stream mid-result (write to the file failed): the
    // server is still sending rows, so a session connection cannot be probed or kept.
    let mut left = false;
    let res = async {
        if let Format::Sql(plan) = &format {
            if plan.options.header {
                out.write_all(dump::header(&conn, &plan.table).as_bytes())
                    .await?;
            }
            if plan.options.transaction {
                out.write_all(plan.begin().as_bytes()).await?;
            }
        }
        let n = match &mut session {
            Some((_, s)) => match &mut s.conn {
                Conn::Pg(c) => stream(&mut *c, &sql, &mut out, &format, PG, &mut left).await?,
                Conn::MySql(c) => {
                    stream(&mut *c, &sql, &mut out, &format, MYSQL, &mut left).await?
                }
                Conn::Sqlite(c) => {
                    stream(&mut *c, &sql, &mut out, &format, SQLITE, &mut left).await?
                }
            },
            None => {
                let pool = connections::pool_for(state, &conn.id, req.database.as_deref())?;
                match pool {
                    DbPool::Pg(p) => {
                        let mut c = p.acquire().await?;
                        stream(&mut *c, &sql, &mut out, &format, PG, &mut left).await?
                    }
                    DbPool::MySql(p) => {
                        let mut c = p.acquire().await?;
                        stream(&mut *c, &sql, &mut out, &format, MYSQL, &mut left).await?
                    }
                    DbPool::Sqlite(p) => {
                        let mut c = p.acquire().await?;
                        stream(&mut *c, &sql, &mut out, &format, SQLITE, &mut left).await?
                    }
                }
            }
        };
        if let Format::Sql(plan) = &format
            && plan.options.transaction
        {
            out.write_all(b"\nCOMMIT;\n").await?;
        }
        out.flush().await?;
        out.get_ref().sync_all().await?;
        Ok::<_, AppError>(n)
    }
    .await;
    let mut res = res;
    if let Some((tab_id, mut s)) = session {
        if left {
            // As in `run_in_session`: probing first would drain the unfinished result behind
            // the probe's timeout. Close the session (rolling its transaction back) and say so.
            if s.tx_open
                && let Err(e) = &mut res
            {
                e.message = format!(
                    "{} The open transaction was rolled back; its uncommitted changes are lost.",
                    e.message
                );
            }
            state.sessions.discard(&tab_id, Some(s)).await;
        } else {
            session::refresh(&mut s).await;
            session::settle(state, &tab_id, s).await;
        }
    }
    drop(out);
    let res = match res {
        Ok(rows) => tokio::fs::rename(&tmp, path)
            .await
            .map(|()| rows)
            .map_err(AppError::from),
        Err(e) => Err(e),
    };
    match res {
        Ok(rows) => Ok(ExportResult {
            cancelled: false,
            rows,
            path: path.display().to_string(),
        }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

const PG: Codec<sqlx::Postgres> = Codec {
    csv: pg_cell,
    sql: dump::push_pg_cell,
    ident: dump::quote_dq,
};
const MYSQL: Codec<sqlx::MySql> = Codec {
    csv: mysql_cell,
    sql: dump::push_mysql_cell,
    ident: dump::quote_bt,
};
const SQLITE: Codec<sqlx::Sqlite> = Codec {
    csv: sqlite_cell,
    sql: dump::push_sqlite_cell,
    ident: dump::quote_dq,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rfc4180_quoting() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape("two\nlines"), "\"two\nlines\"");
        assert_eq!(escape("cr\r"), "\"cr\r\"");
        assert_eq!(escape(""), "");
        assert_eq!(line(["a", "b,c", ""]), "a,\"b,c\",\r\n");
    }

    #[test]
    fn fields_by_value_kind() {
        assert_eq!(field(&Value::Null), "");
        assert_eq!(field(&json!("日本語 🚀")), "日本語 🚀");
        assert_eq!(field(&json!(true)), "true");
        assert_eq!(field(&json!(-1.5)), "-1.5");
        assert_eq!(field(&json!({"$bytes": "3q2+7w==", "len": 4})), "3q2+7w==");
        assert_eq!(field(&json!({"a": [1, 2]})), r#"{"a":[1,2]}"#);
        assert_eq!(field(&json!([1, null])), "[1,null]");
    }

    /// The point of the SQL export: the filtered rows of a read load into the same table in
    /// another database with every value intact (quotes, backslashes, NULL, blobs, reals).
    #[tokio::test]
    async fn sql_export_copies_the_read_rows_to_another_database() {
        use crate::core::connections::{ConnectionInput, SslMode};
        use crate::core::error::ErrorCode;
        use crate::core::secrets::MemoryStore;
        use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
        use sqlx::{Connection as _, Executor as _};

        const TABLE: &str =
            "CREATE TABLE \"it's\" (id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB);";
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        let file = dir.path().join("dev.db");
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        let setup = format!(
            "{TABLE} \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 250) \
             INSERT INTO \"it's\" SELECT i, 'n' || i, i / 3.0, NULL FROM n; \
             UPDATE \"it's\" SET name = 'o''k \\ \"q\"', score = NULL, data = X'00FF' WHERE id = 2;"
        );
        (&mut c).execute(sqlx::raw_sql(&setup)).await.unwrap();
        c.close().await.unwrap();
        let conn = connections::create(
            &st,
            ConnectionInput {
                name: "dev".into(),
                engine: Engine::Sqlite,
                env: Env::Local,
                host: String::new(),
                port: 0,
                database: "main".into(),
                user: String::new(),
                ssl: SslMode::Disable,
                file: file.to_string_lossy().into_owned(),
                ai_write: false,
                read_only: true,
                version: None,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            None,
        )
        .unwrap();
        let req_with = |sql: &str, table: &str, options: SqlOptions| SqlExportRequest {
            query: ExportRequest {
                conn_id: conn.id.clone(),
                database: None,
                sql: sql.into(),
                tab_id: None,
            },
            schema: None,
            table: table.into(),
            options,
        };
        let req = |sql: &str, table: &str| req_with(sql, table, SqlOptions::default());
        let path = dir.path().join("rows.sql");
        // More rows than one INSERT holds, and no row cap.
        let read = "SELECT * FROM \"it's\" WHERE id % 2 = 0 ORDER BY id";
        let r = export_sql(&st, req(read, "it's"), &path).await.unwrap();
        assert_eq!(r.rows, 125);
        let sql = std::fs::read_to_string(&path).unwrap();
        assert!(
            sql.starts_with("-- sandb SQL export\n-- Table: \"it's\"\n"),
            "{sql}"
        );

        let mut other = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        (&mut other).execute(sqlx::raw_sql(TABLE)).await.unwrap();
        (&mut other).execute(sqlx::raw_sql(&sql)).await.unwrap();
        let q = "SELECT id, name, score, hex(data) FROM \"it's\" ORDER BY id";
        type R = (i64, Option<String>, Option<f64>, Option<String>);
        let got: Vec<R> = sqlx::query_as(q).fetch_all(&mut other).await.unwrap();
        let DbPool::Sqlite(p) = connections::pool_for(&st, &conn.id, None).unwrap() else {
            unreachable!()
        };
        let want: Vec<R> = sqlx::query_as(
            "SELECT id, name, score, hex(data) FROM \"it's\" WHERE id % 2 = 0 ORDER BY id",
        )
        .fetch_all(&p)
        .await
        .unwrap();
        assert_eq!(got, want);
        assert_eq!(got[0].1.as_deref(), Some("o'k \\ \"q\""));

        // An empty result writes the header only; a write or a missing table name writes nothing.
        let r = export_sql(&st, req("SELECT * FROM \"it's\" WHERE 0", "it's"), &path)
            .await
            .unwrap();
        assert_eq!(r.rows, 0);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("INSERT"));
        for (sql, table) in [("DELETE FROM \"it's\"", "it's"), (read, " ")] {
            let e = export_sql(&st, req(sql, table), &path).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{sql}");
        }

        // Upsert of a column subset into a target that already holds some of the rows: the
        // exported columns win, the others keep the target's values.
        let upsert = SqlOptions {
            columns: Some(vec![0, 1]),
            mode: InsertMode::Upsert,
            key: vec![0],
            rows_per_statement: 7,
            transaction: true,
            header: false,
        };
        let read = "SELECT * FROM \"it's\" WHERE id <= 20 ORDER BY id";
        let r = export_sql(&st, req_with(read, "it's", upsert.clone()), &path)
            .await
            .unwrap();
        assert_eq!(r.rows, 20);
        let sql = std::fs::read_to_string(&path).unwrap();
        assert!(sql.starts_with("BEGIN;\n\nINSERT INTO \"it's\" (\"id\", \"name\") VALUES\n"));
        assert!(sql.ends_with(";\n\nCOMMIT;\n"), "{sql}");
        assert_eq!(sql.matches("INSERT INTO").count(), 3, "20 rows by 7");
        let load = |seed: &'static str, sql: String| async move {
            let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
            (&mut db).execute(sqlx::raw_sql(TABLE)).await.unwrap();
            (&mut db).execute(sqlx::raw_sql(seed)).await.unwrap();
            (&mut db).execute(sqlx::raw_sql(&sql)).await.unwrap();
            let rows: Vec<(i64, Option<String>, Option<f64>)> =
                sqlx::query_as("SELECT id, name, score FROM \"it's\" ORDER BY id")
                    .fetch_all(&mut db)
                    .await
                    .unwrap();
            rows
        };
        let seed =
            "INSERT INTO \"it's\" (id, name, score) VALUES (3, 'old', 99.0), (500, 'keep', 1.0);";
        let got = load(seed, sql).await;
        assert_eq!(got.len(), 21);
        assert_eq!(
            got[2],
            (3, Some("n3".into()), Some(99.0)),
            "name updated, score kept"
        );
        assert_eq!(got[3], (4, Some("n4".into()), None));
        assert_eq!(got[20], (500, Some("keep".into()), Some(1.0)));

        // Skip leaves an existing row as it is.
        let skip = SqlOptions {
            mode: InsertMode::Skip,
            ..upsert.clone()
        };
        export_sql(&st, req_with(read, "it's", skip), &path)
            .await
            .unwrap();
        let got = load(seed, std::fs::read_to_string(&path).unwrap()).await;
        assert_eq!(got[2], (3, Some("old".into()), Some(99.0)));
        assert_eq!(got.len(), 21);

        // Options that cannot make a valid file are refused before anything is written.
        let bad = [
            SqlOptions {
                key: vec![],
                ..upsert.clone()
            },
            SqlOptions {
                key: vec![2],
                ..upsert.clone()
            },
            SqlOptions {
                rows_per_statement: 0,
                ..upsert.clone()
            },
            SqlOptions {
                columns: Some(vec![]),
                ..SqlOptions::default()
            },
        ];
        for o in bad {
            let e = export_sql(&st, req_with(read, "it's", o.clone()), &path)
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{o:?}");
        }
        // A column the result does not have.
        let e = export_sql(
            &st,
            req_with(
                read,
                "it's",
                SqlOptions {
                    columns: Some(vec![0, 9]),
                    ..SqlOptions::default()
                },
            ),
            &path,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
    }

    /// PG and MySQL have no local database here, so their statement text is pinned instead.
    #[test]
    fn conflict_clauses_per_engine() {
        let plan = |engine, mode, key: Vec<usize>| SqlPlan {
            engine,
            table: "t".into(),
            options: SqlOptions {
                mode,
                key,
                ..SqlOptions::default()
            },
        };
        let names = ["id", "name"];
        let parts = |p: SqlPlan, ident| p.parts(&names, ident).unwrap();
        let (_, head, tail) = parts(plan(Engine::Pg, InsertMode::Skip, vec![]), dump::quote_dq);
        assert_eq!(head, "INSERT INTO t (\"id\", \"name\") VALUES");
        assert_eq!(tail, "\nON CONFLICT DO NOTHING");
        let (_, _, tail) = parts(
            plan(Engine::Pg, InsertMode::Upsert, vec![0]),
            dump::quote_dq,
        );
        assert_eq!(
            tail,
            "\nON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
        let (_, _, tail) = parts(
            plan(Engine::Pg, InsertMode::Upsert, vec![0, 1]),
            dump::quote_dq,
        );
        assert_eq!(tail, "\nON CONFLICT (\"id\", \"name\") DO NOTHING");
        let (_, head, tail) = parts(
            plan(Engine::Mysql, InsertMode::Skip, vec![]),
            dump::quote_bt,
        );
        assert_eq!(head, "INSERT IGNORE INTO t (`id`, `name`) VALUES");
        assert_eq!(tail, "");
        let (_, _, tail) = parts(
            plan(Engine::Mysql, InsertMode::Upsert, vec![0]),
            dump::quote_bt,
        );
        assert_eq!(tail, "\nON DUPLICATE KEY UPDATE `name` = VALUES(`name`)");
        // MySQL finds the key itself: no key updates every column.
        let (_, _, tail) = parts(
            plan(Engine::Mysql, InsertMode::Upsert, vec![]),
            dump::quote_bt,
        );
        assert_eq!(
            tail,
            "\nON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
    }
}
