//! Result export: re-runs one plain-read statement without a row cap and streams it to a file,
//! as CSV or as INSERT statements.

use std::path::Path;

use futures_util::TryStreamExt;
use sqlx::{Column, Database, Executor, Row};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::core::cell::{field, line};
use crate::core::connections::{Connection, Env, target_database};
use crate::core::db::conn::{describe_columns, Conn};
use crate::core::db::ctx::Ctx;
use crate::core::db::dump::{self, Inserts};
use crate::core::db::reader::{self, CellFn};
use crate::core::db::session::{self, Settle};
use crate::core::db::value::Full;
use crate::core::engine::SqlEngine;
use crate::core::error::AppError;
use crate::core::export_file::ExportFile;
use crate::core::result::{
    ExportRequest, ExportResult, InsertMode, MAX_ROWS_PER_STATEMENT, SqlExportRequest, SqlOptions,
};
use crate::core::sql::{facts, split};

type SqlCellFn<DB> = fn(&mut String, &<DB as Database>::Row, usize) -> Result<(), AppError>;

enum Format {
    Csv,
    Sql(SqlPlan),
}

struct SqlPlan {
    engine: SqlEngine,
    /// Quoted, maybe qualified, name.
    table: String,
    options: SqlOptions,
}

/// The conflict handling the SQL export writes, as the export dialog names it. The writer
/// below builds the same text around its own column lists.
pub struct ConflictText {
    /// A row that is already in the target is skipped with this form.
    pub skip: &'static str,
    /// A row that is already in the target is updated with this clause.
    pub upsert: &'static str,
}

pub fn conflict_text(engine: SqlEngine) -> ConflictText {
    match engine {
        SqlEngine::Mysql => ConflictText {
            skip: "INSERT IGNORE",
            upsert: "ON DUPLICATE KEY UPDATE",
        },
        SqlEngine::Pg => ConflictText {
            skip: "ON CONFLICT DO NOTHING",
            upsert: "ON CONFLICT (key) DO UPDATE",
        },
        SqlEngine::Sqlite => ConflictText {
            skip: "INSERT OR IGNORE",
            upsert: "ON CONFLICT (key) DO UPDATE",
        },
    }
}

impl SqlPlan {
    /// The written result columns, and the text before and after the rows of each statement,
    /// for a result with these column names.
    fn parts(&self, names: &[&str]) -> Result<(Vec<usize>, String, String), AppError> {
        let o = &self.options;
        let engine = self.engine;
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
        let q = |i: &usize| engine.quote(names[*i]);
        let list = cols.iter().map(q).collect::<Vec<_>>().join(", ");
        let head = format!("{} {} ({list}) VALUES", engine.insert_verb(o.mode == InsertMode::Skip), self.table);
        let set: Vec<String> = cols
            .iter()
            .filter(|i| !o.key.contains(i))
            .map(|i| engine.upsert_assignment(&q(i)))
            .collect();
        let key = o.key.iter().map(q).collect::<Vec<_>>().join(", ");
        let tail = match (o.mode, engine) {
            (InsertMode::Insert, _) | (InsertMode::Skip, SqlEngine::Mysql | SqlEngine::Sqlite) => {
                String::new()
            }
            (InsertMode::Skip, SqlEngine::Pg) => "\nON CONFLICT DO NOTHING".into(),
            // Every column is in the key: nothing to update.
            (InsertMode::Upsert, SqlEngine::Mysql) if set.is_empty() => {
                let c = q(&cols[0]);
                format!("\nON DUPLICATE KEY UPDATE {c} = {c}")
            }
            (InsertMode::Upsert, SqlEngine::Mysql) => {
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

    fn begin(&self) -> String {
        format!("{};\n\n", self.engine.begin_transaction())
    }
}

struct Codec<DB: Database> {
    csv: CellFn<DB>,
    sql: SqlCellFn<DB>,
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
    let (cols, head, tail) = match plan.parts(&names) {
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
            let fields: Vec<String> = (0..row.len()).map(|i| field(&cell(&row, i))).collect();
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
        if let Some(columns) = describe_columns(&mut *conn, sql).await
            && !columns.is_empty()
        {
            let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
            out.write_all(line(names).as_bytes()).await?;
        }
    }
    Ok(n)
}

/// Streams rows through whichever connection owns them: the tab session when it is on this
/// target, else the pool.
async fn stream_conn<W: AsyncWrite + Unpin>(
    c: &mut Conn,
    sql: &str,
    out: &mut W,
    format: &Format,
    left: &mut bool,
) -> Result<u64, AppError> {
    match c {
        Conn::Pg(o) => stream(o.connection(), sql, out, format, PG, left).await,
        Conn::MySql(o) => stream(o.connection(), sql, out, format, MYSQL, left).await,
        Conn::Sqlite(o) => stream(o.connection(), sql, out, format, SQLITE, left).await,
    }
}

/// Rejects anything but one plain read, so the export never repeats a write. Call it before
/// asking the user for a file.
pub fn check(engine: SqlEngine, sql: &str) -> Result<(), AppError> {
    match split::split(engine, sql).as_slice() {
        [one] if facts::is_plain_read_sql(engine, one) => Ok(()),
        _ => Err(AppError::invalid(
            "Export needs exactly one read-only statement (a SELECT)",
        )),
    }
}

pub fn check_sql(engine: SqlEngine, req: &SqlExportRequest) -> Result<(), AppError> {
    check(engine, &req.query.sql)?;
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
        if engine != SqlEngine::Mysql && o.key.is_empty() {
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

/// Streams the result as CSV to a temp file next to `path`, then renames it over `path`. On
/// failure only the temp file is removed; an existing file at `path` is never touched.
pub async fn export_csv(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: ExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    check(engine, &req.sql)?;
    export(ctx, conn, engine, req, Format::Csv, path).await
}

pub async fn export_sql(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: SqlExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    check_sql(engine, &req)?;
    let table = match req.schema.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("{}.{}", engine.quote(s), engine.quote(&req.table)),
        None => engine.quote(&req.table),
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
    export(ctx, conn, engine, req.query, Format::Sql(plan), path).await
}

async fn export(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: ExportRequest,
    format: Format,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let sql = split::split(engine, &req.sql).remove(0);
    // A production tab exports what its open transaction sees: the same session the run used.
    // Without one (no open transaction, or a session on another target) the pool's view is
    // the same committed data. Read-only connections never hold a session.
    let database = target_database(conn, req.database.as_deref());
    let mut file =
        ExportFile::for_query(path, ctx.queries, &conn.id, &database, conn.engine).await?;
    if let Format::Sql(plan) = &format {
        if plan.options.header {
            file.out()
                .write_all(dump::header(conn, engine, &plan.table).as_bytes())
                .await?;
        }
        if plan.options.transaction {
            file.out().write_all(plan.begin().as_bytes()).await?;
        }
    }
    // Set when the export dropped its row stream mid-result (write to the file failed): the
    // server is still sending rows, so a session connection cannot be probed or kept.
    let mut left = false;
    let session_read = if conn.env == Env::Production && conn.may_write() {
        Some(
            session::with_tab(
                ctx,
                conn,
                &database,
                req.tab_id.as_deref(),
                async |s| {
                    let tx_open = s.tx_open;
                    let r = stream_conn(&mut s.conn, &sql, file.out(), &format, &mut left).await;
                    let settle = if left {
                        Settle::Discard
                    } else {
                        Settle::Refresh
                    };
                    ((r, tx_open), settle)
                },
            )
            .await?,
        )
    } else {
        None
    };
    let res = match session_read {
        Some(Some((r, tx_open))) => {
            let mut r = r;
            if left
                && tx_open
                && let Err(e) = &mut r
            {
                e.message = format!(
                    "{} The open transaction was rolled back; its uncommitted changes are lost.",
                    e.message
                );
            }
            r
        }
        _ => {
            let pool = ctx.pool_for(conn, req.database.as_deref())?;
            let mut c = Conn::acquire(&pool).await?;
            let r = stream_conn(&mut c, &sql, file.out(), &format, &mut left).await;
            c.settle(true).await;
            r
        }
    };
    if let Format::Sql(plan) = &format
        && plan.options.transaction
    {
        file.out().write_all(b"\nCOMMIT;\n").await?;
    }
    match res {
        Ok(rows) => file.finish(rows).await,
        Err(e) => Err(e),
    }
}

const PG: Codec<sqlx::Postgres> = Codec {
    csv: reader::pg_cell::<Full>,
    sql: dump::push_pg_cell,
};
const MYSQL: Codec<sqlx::MySql> = Codec {
    csv: reader::mysql_cell::<Full>,
    sql: dump::push_mysql_cell,
};
const SQLITE: Codec<sqlx::Sqlite> = Codec {
    csv: reader::sqlite_cell::<Full>,
    sql: dump::push_sqlite_cell,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cell::{escape, field};
    use crate::core::db::pool::DbPool;
    use crate::core::state::AppState;
    use serde_json::{Value, json};

    /// The tests run on one SQLite connection, so this wrapper routes for them.
    async fn export_sql(
        state: &AppState,
        req: SqlExportRequest,
        path: &Path,
    ) -> Result<ExportResult, AppError> {
        let conn = state.connection(&req.query.conn_id).unwrap();
        super::export_sql(&state.db(), &conn, SqlEngine::Sqlite, req, path).await
    }

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
        use crate::core::engine::Engine;
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
        let conn = st
            .create_connection(
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
        let DbPool::Sqlite(p) = st.pool_for(&conn, None).unwrap() else {
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
        let parts = |p: SqlPlan| p.parts(&names).unwrap();
        let (_, head, tail) = parts(plan(SqlEngine::Pg, InsertMode::Skip, vec![]));
        assert_eq!(head, "INSERT INTO t (\"id\", \"name\") VALUES");
        assert_eq!(tail, "\nON CONFLICT DO NOTHING");
        let (_, _, tail) = parts(
            plan(SqlEngine::Pg, InsertMode::Upsert, vec![0])
        );
        assert_eq!(
            tail,
            "\nON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
        let (_, _, tail) = parts(
            plan(SqlEngine::Pg, InsertMode::Upsert, vec![0, 1])
        );
        assert_eq!(tail, "\nON CONFLICT (\"id\", \"name\") DO NOTHING");
        let (_, head, tail) = parts(
            plan(SqlEngine::Mysql, InsertMode::Skip, vec![])
        );
        assert_eq!(head, "INSERT IGNORE INTO t (`id`, `name`) VALUES");
        assert_eq!(tail, "");
        let (_, _, tail) = parts(
            plan(SqlEngine::Mysql, InsertMode::Upsert, vec![0])
        );
        assert_eq!(tail, "\nON DUPLICATE KEY UPDATE `name` = VALUES(`name`)");
        // MySQL finds the key itself: no key updates every column.
        let (_, _, tail) = parts(
            plan(SqlEngine::Mysql, InsertMode::Upsert, vec![])
        );
        assert_eq!(
            tail,
            "\nON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
    }
}
