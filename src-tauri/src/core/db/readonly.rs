//! Layers 2 and 3 of the AI safeguards. One AI-written read runs on its own connection inside a
//! read-only transaction (PG, MySQL/MariaDB) or on a separate read-only connection (SQLite), with
//! a row cap and a time limit. The connection is always closed afterwards, so no session state
//! (read-only mode, timeouts, sql_mode) reaches the pool. The public entry point always runs the
//! layer 1 guard first; there is no way to skip it outside test builds.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::mysql::{MySql, MySqlConnection, MySqlDatabaseError};
use sqlx::postgres::{PgConnection, Postgres};
use sqlx::sqlite::Sqlite;
use sqlx::{Database, Either, Executor, Row};

use crate::core::connections::{Connection, target_database};
use crate::core::db::conn::{self as sql_conn, CLOSE_TIMEOUT, Conn};
use crate::core::db::ctx::Ctx;
use crate::core::db::reader::{self, CellFn};
use crate::core::db::value::Preview;
use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::guard;
use crate::core::query_registry::{Session, SqliteHandle};
use crate::core::result::{AiReadResult, ColumnMeta, GRACE, ReadLimits, timeout_error};

const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// PG `lock_timeout`: an AI read never queues long behind another session's lock.
const PG_LOCK_TIMEOUT_MS: u64 = 2_000;
/// Total decoded row bytes an AI read may produce; anything above stops the read. The row cap
/// and the per-cell preview cap do not cover value amplification (repeat, big BLOB columns).
const ROW_BYTES_LIMIT: usize = 16 * 1024 * 1024;
/// sql_mode flags that change how MySQL reads quotes and backslashes, or switch the dialect. With
/// them the server could end a string literal where the guard's parser does not.
const LEXING_MODES: &[&str] = &[
    "NO_BACKSLASH_ESCAPES",
    "ANSI_QUOTES",
    "ANSI",
    "ORACLE",
    "MSSQL",
    "DB2",
    "POSTGRESQL",
    "MAXDB",
];

/// Runs AI-written `sql` read-only: the layer 1 guard, then the database-enforced read-only
/// session with `limits`. `database`: the connection's own when absent.
pub async fn run_ai_read(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    guard::classify::check_ai_read(sql, engine)?;
    execute(ctx, conn, engine, database, sql, limits).await
}

/// Layers 2 and 3 only, so integration tests can prove that the database itself refuses a write
/// that got past layer 1. Never compiled into a normal build.
#[cfg(feature = "it")]
pub async fn run_read_only_unchecked(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    execute(ctx, conn, engine, database, sql, limits).await
}

/// The server session running the read, so the safety net can stop it.
type Slot = Arc<Mutex<Option<Session>>>;

fn put(slot: &Slot, s: Option<Session>) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

#[derive(Default)]
struct Fetched {
    columns: Option<Vec<ColumnMeta>>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
    over_budget: bool,
}

async fn execute(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let limits = limits.sanitized();
    let database = target_database(conn, database);
    let started = Instant::now();
    let slot: Slot = Arc::default();
    let work = run_engine(ctx, conn, engine, &database, sql, limits, slot.clone());
    tokio::pin!(work);
    let fetched = tokio::select! {
        r = &mut work => r?,
        _ = tokio::time::sleep(limits.timeout + GRACE) => {
            // `work` (and its connection) stays alive until this returns, so the SQLite handle
            // in the slot is still valid while it is interrupted.
            stop(ctx, conn, &database, &slot).await;
            return Err(timeout_error(limits.timeout));
        }
    };
    Ok(AiReadResult {
        columns: fetched.columns.unwrap_or_default(),
        rows: fetched.rows,
        truncated: fetched.truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
        documents: None,
    })
}

async fn run_engine(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
    sql: &str,
    limits: ReadLimits,
    slot: Slot,
) -> Result<Fetched, AppError> {
    if engine == SqlEngine::Sqlite {
        // Never the user's read-write pool.
        return sqlite_read(conn, sql, limits, &slot).await;
    }
    let pool = ctx.pool_for(conn, Some(database))?;
    let mut c = Conn::acquire(&pool).await?;
    // Also on an early return or when the safety net drops this future.
    c.close_on_drop();
    put(&slot, Some(c.session().await?));
    let result = match engine {
        SqlEngine::Pg => {
            let cc = c.as_pg().expect("a PostgreSQL pool yields a PostgreSQL connection");
            async {
                pg_setup(cc, sql, limits).await?;
                finish_read::<Postgres>(
                    cc,
                    engine,
                    sql,
                    limits,
                    false,
                    reader::pg_cell::<Preview>,
                )
                .await
            }
            .await
        }
        SqlEngine::Mysql => {
            let cc = c.as_mysql().expect("a MySQL pool yields a MySQL connection");
            async {
                mysql_setup(cc, limits).await?;
                finish_read::<MySql>(
                    cc,
                    engine,
                    sql,
                    limits,
                    true,
                    reader::mysql_cell_binary::<Preview>,
                )
                .await
            }
            .await
        }
        SqlEngine::Sqlite => unreachable!("handled above"),
    };
    // Cleared before the connection closes, so the safety net never interrupts a freed handle.
    put(&slot, None);
    c.close().await;
    result
}

/// Safety net: the database did not stop the read in time. Cancels it on the server through a
/// fresh, separate connection: borrowing the user's pool could queue behind a saturated pool,
/// and the read's own settings must never touch it.
async fn stop(ctx: &Ctx<'_>, conn: &Connection, database: &str, slot: &Slot) {
    let session = *slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(session) = session else {
        return;
    };
    // SQLite's interrupt is in-process; the SQL engines need the pool their session belongs to.
    let pool = match session {
        Session::Sqlite(_) => None,
        _ => ctx.pool_for(conn, Some(database)).ok(),
    };
    // Best effort by design: the read already timed out, and the caller reports that.
    let _ = tokio::time::timeout(STOP_TIMEOUT, sql_conn::kill(pool.as_ref(), session)).await;
}

enum Refusal {
    ReadOnly,
    /// More than one statement reached the server (it rejects a stacked prepared statement).
    Stacked,
    Timeout,
}

/// The server's answer to the read, with read-only violations and the read's own limits
/// recognised; everything else keeps the normal mapping.
fn map_read_error(engine: SqlEngine, e: sqlx::Error, sql: &str, limit: Duration) -> AppError {
    // A `;` inside a string literal does not count: the splitter uses the same tokenizer.
    let stacked = crate::core::sql::split::split(engine, sql).len() >= 2;
    let refusal = match &e {
        sqlx::Error::Database(db) => match engine {
            // 25006 read_only_sql_transaction; 57014 query_canceled (statement_timeout);
            // 55P03 lock_not_available (lock_timeout).
            SqlEngine::Pg => match db.code().as_deref() {
                Some("25006") => Some(Refusal::ReadOnly),
                // 42601 "cannot insert multiple commands into a prepared statement": a stacked
                // statement tried to escape the one-read rule.
                Some("42601") if db.message().contains("multiple commands") => {
                    Some(Refusal::Stacked)
                }
                Some("57014" | "55P03") => Some(Refusal::Timeout),
                _ => None,
            },
            // 1792 read-only transaction; 3024 max_execution_time (MySQL); 1969
            // max_statement_time (MariaDB); 1205 lock wait timeout.
            SqlEngine::Mysql => match db
                .try_downcast_ref::<MySqlDatabaseError>()
                .map(|m| m.number())
            {
                Some(1792) => Some(Refusal::ReadOnly),
                // 1064 syntax error on text that holds more than one statement: the server
                // stopped parsing where the second statement starts.
                Some(1064) if stacked => Some(Refusal::Stacked),
                Some(3024 | 1969 | 1205) => Some(Refusal::Timeout),
                _ => None,
            },
            // Extended result codes: the primary code is the low byte. 8 SQLITE_READONLY
            // (mode=ro, query_only); 23 SQLITE_AUTH (the authorizer); 9 SQLITE_INTERRUPT (the
            // deadline's progress handler).
            SqlEngine::Sqlite => match db
                .code()
                .and_then(|c| c.parse::<i32>().ok())
                .map(|c| c & 0xff)
            {
                Some(8 | 23) => Some(Refusal::ReadOnly),
                Some(9) => Some(Refusal::Timeout),
                _ => None,
            },
        },
        _ => None,
    };
    match refusal {
        Some(Refusal::ReadOnly) => AppError::new(
            ErrorCode::ReadOnlyViolation,
            "The database refused a write: AI queries run in a read-only session",
        )
        .with_detail(e.to_string()),
        Some(Refusal::Stacked) => AppError::new(
            ErrorCode::ReadOnlyViolation,
            "The database refused a stacked statement: AI queries are one read-only statement",
        )
        .with_detail(e.to_string()),
        Some(Refusal::Timeout) => timeout_error(limit).with_detail(e.to_string()),
        None => AppError::from(e),
    }
}

/// Approximate decoded size of a mapped cell, so `repeat`-style amplification counts against
/// `ROW_BYTES_LIMIT` even though the preview caps what is kept.
fn cell_bytes(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Number(_) | Value::Bool(_) | Value::Null => 16,
        Value::Array(a) => a.iter().map(cell_bytes).sum(),
        // `{"$bytes": …, "len": n}` carries the full decoded length.
        Value::Object(o) if o.contains_key("$bytes") => {
            o.get("len").and_then(Value::as_u64).unwrap_or(0) as usize
        }
        Value::Object(o) => o.values().map(cell_bytes).sum(),
    }
}

/// Reads at most `max_rows` rows of the first result set and stops early past that, or when
/// decoded row data exceeds `ROW_BYTES_LIMIT`. `prepared`: run `sql` through the extended
/// protocol (one statement only — the server refuses a stacked second statement).
async fn fetch<DB>(
    conn: &mut DB::Connection,
    sql: &str,
    max_rows: usize,
    prepared: bool,
    cell: CellFn<DB>,
) -> Result<Fetched, sqlx::Error>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    for<'q> <DB as Database>::Arguments<'q>: sqlx::IntoArguments<'q, DB>,
{
    let mut out = Fetched::default();
    let mut bytes = 0usize;
    {
        let mut stream = if prepared {
            (&mut *conn).fetch_many(sqlx::query::<DB>(sql))
        } else {
            sqlx::raw_sql(sql).fetch_many(&mut *conn)
        };
        while let Some(item) = stream.try_next().await? {
            let Either::Right(row) = item else { continue };
            if out.columns.is_none() {
                out.columns = Some(row.columns().iter().map(reader::meta).collect());
            }
            if out.rows.len() >= max_rows {
                out.truncated = true;
                break;
            }
            let values: Vec<Value> = (0..row.len())
                .map(|i| cell(&row, i))
                .collect();
            bytes += values.iter().map(cell_bytes).sum::<usize>();
            out.rows.push(values);
            if bytes > ROW_BYTES_LIMIT {
                out.over_budget = true;
                break;
            }
        }
    }
    // No row described the columns (and the stream ran to its end); ask the server.
    if out.columns.is_none()
        && !out.over_budget
        && let Some(columns) = sql_conn::describe_columns(conn, sql).await
    {
        out.columns = Some(columns);
    }
    Ok(out)
}

fn over_budget_error() -> AppError {
    AppError::new(
        ErrorCode::Database,
        format!(
            "The AI query produced more than {} MiB of row data and was stopped; narrow it down",
            ROW_BYTES_LIMIT / (1024 * 1024)
        ),
    )
}

/// The shared lifecycle tail: fetch, map the error, roll a finished transaction back (closing
/// the connection ends an unfinished one), and turn the byte budget into the error.
async fn finish_read<DB>(
    c: &mut DB::Connection,
    engine: SqlEngine,
    sql: &str,
    limits: ReadLimits,
    prepared: bool,
    cell: CellFn<DB>,
) -> Result<Fetched, AppError>
where
    DB: Database,
    for<'x> &'x mut DB::Connection: Executor<'x, Database = DB>,
    for<'q> <DB as Database>::Arguments<'q>: sqlx::IntoArguments<'q, DB>,
{
    let r = fetch::<DB>(c, sql, limits.max_rows, prepared, cell)
        .await
        .map_err(|e| map_read_error(engine, e, sql, limits.timeout));
    // After a cut-off read the server is still sending rows; closing the connection ends the
    // transaction without reading them.
    if engine != SqlEngine::Sqlite && r.as_ref().is_ok_and(|f| !f.truncated && !f.over_budget) {
        // The rows are already in hand; a failed ROLLBACK only leaves the close below to end
        // the transaction.
        let _ =
            tokio::time::timeout(CLOSE_TIMEOUT, c.execute(sqlx::raw_sql("ROLLBACK"))).await;
    }
    r.and_then(|f| if f.over_budget { Err(over_budget_error()) } else { Ok(f) })
}

async fn pg_setup(
    c: &mut PgConnection,
    sql: &str,
    limits: ReadLimits,
) -> Result<(), AppError> {
    let ms = limits.timeout.as_millis().max(1);
    // The session default also makes any transaction a stacked COMMIT could start read-only.
    // standard_conforming_strings: the guard's parser reads 'a\' as a complete string.
    for s in [
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY".to_string(),
        "BEGIN READ ONLY".to_string(),
        format!("SET LOCAL statement_timeout = {ms}"),
        format!("SET LOCAL lock_timeout = {PG_LOCK_TIMEOUT_MS}"),
        format!("SET LOCAL idle_in_transaction_session_timeout = {ms}"),
        "SET LOCAL standard_conforming_strings = on".to_string(),
    ] {
        (&mut *c).execute(sqlx::raw_sql(&s)).await?;
    }
    // The extended-protocol Parse step refuses a second statement with 42601; run the same
    // text with raw_sql afterwards so results come back in text format through the cell reader.
    (&mut *c)
        .prepare(sql)
        .await
        .map_err(|e| map_read_error(SqlEngine::Pg, e, sql, limits.timeout))?;
    Ok(())
}

/// `sql_mode` without the flags in [`LEXING_MODES`]. Only plain mode names are kept, so the
/// result can go into a string literal.
fn safe_sql_mode(mode: &str) -> String {
    mode.split(',')
        .map(str::trim)
        .filter(|m| {
            !m.is_empty()
                && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !LEXING_MODES.contains(&m.to_ascii_uppercase().as_str())
        })
        .collect::<Vec<_>>()
        .join(",")
}

async fn mysql_setup(c: &mut MySqlConnection, limits: ReadLimits) -> Result<(), AppError> {
    let (version, mode, db): (String, String, Option<String>) = sqlx::query_as(
        "SELECT VERSION(), CAST(@@SESSION.sql_mode AS CHAR), DATABASE()",
    )
    .fetch_one(&mut *c)
    .await?;
    if db
        .as_deref()
        .is_some_and(|name| SqlEngine::Mysql.is_system_schema(name))
    {
        return Err(AppError::new(
            ErrorCode::GuardRejected,
            "AI reads are blocked for privacy inside a MySQL system schema: unqualified table \
             names would resolve to system tables",
        ));
    }
    let ms = limits.timeout.as_millis().max(1);
    let secs = limits.timeout.as_secs().max(1);
    let time_limit = if SqlEngine::is_mariadb(&version) {
        format!(
            "SET SESSION max_statement_time = {:.3}",
            (ms as f64 / 1000.0).max(0.001)
        )
    } else {
        format!("SET SESSION max_execution_time = {ms}")
    };
    // The session access mode also covers DDL, which commits the open transaction implicitly
    // and then runs outside it.
    for s in [
        time_limit,
        format!("SET SESSION lock_wait_timeout = {secs}"),
        format!("SET SESSION sql_mode = '{}'", safe_sql_mode(&mode)),
        "SET SESSION TRANSACTION READ ONLY".to_string(),
        "START TRANSACTION READ ONLY".to_string(),
    ] {
        (&mut *c).execute(sqlx::raw_sql(&s)).await?;
    }
    Ok(())
}

async fn sqlite_read(
    conn: &Connection,
    sql: &str,
    limits: ReadLimits,
    slot: &Slot,
) -> Result<Fetched, AppError> {
    let mut c = Conn::sqlite_read_only(conn).await?;
    let cc = c.as_sqlite().expect("a SQLite connection");
    (&mut *cc)
        .execute(sqlx::raw_sql("PRAGMA query_only = ON"))
        .await?;
    let deadline = Instant::now() + limits.timeout;
    {
        let mut h = cc.lock_handle().await?;
        let raw = h.as_raw_handle().as_ptr();
        put(slot, Some(Session::Sqlite(SqliteHandle(raw))));
        // Returning false interrupts the running statement (SQLITE_INTERRUPT).
        h.set_progress_handler(1_000, move || Instant::now() < deadline);
        // SAFETY: `h` holds the connection lock, so `raw` is valid and exclusively ours here.
        // The authorizer is the real read-only barrier: sqlx-sqlite runs every statement of a
        // multi-statement string, so `PRAGMA query_only = 0` or `VACUUM INTO` could otherwise
        // undo the setup above. `ai_authorizer` never touches the connection from inside the
        // callback (it only reads its arguments), which sqlite3_set_authorizer requires.
        // SQLITE_LIMIT_ATTACHED = 0 additionally blocks ATTACH, which VACUUM INTO uses.
        unsafe {
            libsqlite3_sys::sqlite3_set_authorizer(raw, Some(ai_authorizer), ptr::null_mut());
            libsqlite3_sys::sqlite3_limit(raw, libsqlite3_sys::SQLITE_LIMIT_ATTACHED, 0);
            libsqlite3_sys::sqlite3_limit(
                raw,
                libsqlite3_sys::SQLITE_LIMIT_LENGTH,
                (ROW_BYTES_LIMIT - 1) as c_int,
            );
        }
    }
    let r = finish_read::<Sqlite>(
        cc,
        SqlEngine::Sqlite,
        sql,
        limits,
        false,
        reader::sqlite_cell::<Preview>,
    )
    .await;
    // Cleared before the connection closes, so the safety net never interrupts a freed handle.
    put(slot, None);
    c.close().await;
    r
}

fn c_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: SQLite hands the authorizer NUL-terminated strings owned by the connection.
    Some(
        unsafe { CStr::from_ptr(p) }
            .to_string_lossy()
            .to_lowercase(),
    )
}

/// `sqlite3_set_authorizer` callback for the AI connection: every write action and every way to
/// weaken the session is denied, so a stacked `PRAGMA query_only = 0; INSERT …` cannot turn the
/// connection writable. Returns `SQLITE_DENY` unless the action is a plain read.
unsafe extern "C" fn ai_authorizer(
    _ctx: *mut c_void,
    action: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    _db: *const c_char,
    _trigger: *const c_char,
) -> c_int {
    use libsqlite3_sys::*;
    match action {
        SQLITE_PRAGMA => {
            // arg1 = pragma name, arg2 = its argument or new value. Only the schema-info
            // pragmas pass, whatever their argument; `PRAGMA query_only = 0` has a name that
            // is not listed, so it is denied.
            if c_str(arg1)
                .is_some_and(|n| SqlEngine::Sqlite.is_read_pragma(&n))
            {
                SQLITE_OK
            } else {
                SQLITE_DENY
            }
        }
        SQLITE_READ | SQLITE_SELECT | SQLITE_TRANSACTION | SQLITE_SAVEPOINT | SQLITE_RECURSIVE => {
            SQLITE_OK
        }
        SQLITE_FUNCTION => {
            // arg2 = function name: load_extension and file functions can escape the file.
            match c_str(arg2).as_deref() {
                Some(
                    "load_extension" | "readfile" | "writefile" | "edit" | "fts3_tokenizer"
                    | "zipfile",
                ) => SQLITE_DENY,
                _ => SQLITE_OK,
            }
        }
        // INSERT, UPDATE, DELETE, CREATE_*, DROP_*, ALTER_TABLE, REINDEX, ANALYZE, ATTACH,
        // DETACH, VACUUM's internal actions, COPY and anything else not listed above.
        _ => SQLITE_DENY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{ConnectionInput, Env, SslMode};
    use crate::core::db::query;
    use crate::core::engine::Engine;
    use crate::core::result::RunRequest;
    use crate::core::secrets::MemoryStore;
    use crate::core::state::AppState;
    use sqlx::Connection as _;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

    // The tests run on one SQLite connection, so these wrappers route for them.
    async fn run_ai_read(
        state: &AppState,
        conn_id: &str,
        database: Option<&str>,
        sql: &str,
        limits: ReadLimits,
    ) -> Result<AiReadResult, AppError> {
        let conn = state.connection(conn_id).unwrap();
        let engine = conn.engine.sql().unwrap();
        super::run_ai_read(&state.db(), &conn, engine, database, sql, limits).await
    }

    async fn run_read_only_unchecked(
        state: &AppState,
        conn_id: &str,
        database: Option<&str>,
        sql: &str,
        limits: ReadLimits,
    ) -> Result<AiReadResult, AppError> {
        let conn = state.connection(conn_id).unwrap();
        let engine = conn.engine.sql().unwrap();
        super::execute(&state.db(), &conn, engine, database, sql, limits).await
    }

    async fn run_sql(
        state: &AppState,
        req: RunRequest,
    ) -> Result<crate::core::result::QueryRun, AppError> {
        let conn = state.connection(&req.conn_id).unwrap();
        let engine = conn.engine.sql().unwrap();
        query::run(&state.db(), &conn, engine, req).await
    }

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        (dir, st)
    }

    fn input(engine: Engine, port: u16, file: &str) -> ConnectionInput {
        ConnectionInput {
            name: "t".into(),
            engine,
            env: Env::Local,
            host: "127.0.0.1".into(),
            port,
            database: "shop".into(),
            user: "u".into(),
            ssl: SslMode::Disable,
            file: file.into(),
            ai_write: false,
            read_only: false,
            version: None,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    /// A SQLite file with 1000 rows in `t`, saved as a connection.
    async fn sqlite_state() -> (tempfile::TempDir, AppState, String, std::path::PathBuf) {
        let (dir, st) = state();
        let file = dir.path().join("ro.db");
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        (&mut c)
            .execute(sqlx::raw_sql(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000) \
                 INSERT INTO t SELECT i, 'v' || i FROM n",
            ))
            .await
            .unwrap();
        c.close().await.unwrap();
        let conn = st
            .create_connection(input(Engine::Sqlite, 0, &file.to_string_lossy()), None)
            .unwrap();
        (dir, st, conn.id, file)
    }

    async fn count(st: &AppState, id: &str) -> String {
        let r = run_sql(
            st,
            RunRequest {
                conn_id: id.into(),
                database: None,
                sql: "SELECT count(*) AS n FROM t".into(),
                query_id: uuid::Uuid::new_v4().to_string(),
                limit: None,
                tab_id: None,
                sort: None,
            },
        )
        .await
        .unwrap();
        r.statements[0].rows[0][0].to_string()
    }

    /// Port 1 refuses connections: a DELETE must be refused by the guard before any connect
    /// attempt (an immediate `guard_rejected`), while an allowed read goes on to the read path and
    /// is stopped by its own time limit instead (`timeout`, not a guard refusal).
    #[tokio::test]
    async fn guard_runs_before_any_database_contact() {
        let (_d, st) = state();
        let quick = ReadLimits {
            max_rows: 1,
            timeout: Duration::from_millis(200),
        };
        for engine in [Engine::Pg, Engine::Mysql] {
            let c = st.create_connection(input(engine, 1, ""), None).unwrap();
            let e = run_ai_read(&st, &c.id, None, "DELETE FROM t", quick)
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::GuardRejected, "{engine:?}: {e:?}");
            let e = run_ai_read(&st, &c.id, None, "SELECT 1", quick)
                .await
                .unwrap_err();
            // The read's own limit stopped it: the pool's own error carries a detail.
            assert_eq!(e.code, ErrorCode::Timeout, "{engine:?}: {e:?}");
            assert!(e.detail.is_none(), "{engine:?}: {e:?}");
        }
    }

    #[tokio::test]
    async fn sqlite_reads_are_capped_and_writes_are_refused_by_sqlite() {
        let (_d, st, id, _file) = sqlite_state().await;
        let r = run_ai_read(
            &st,
            &id,
            None,
            "SELECT * FROM t ORDER BY id",
            ReadLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!((r.rows.len(), r.truncated), (500, true));
        assert_eq!(r.columns.len(), 2);
        let r = run_ai_read(
            &st,
            &id,
            None,
            "SELECT * FROM t WHERE id < 0",
            ReadLimits::default(),
        )
        .await
        .unwrap();
        assert!(r.rows.is_empty() && !r.truncated);
        assert_eq!(r.columns.len(), 2, "columns come from describe");

        for sql in [
            "INSERT INTO t VALUES (5000, 'x')",
            "UPDATE t SET v = 'x'",
            "DELETE FROM t",
            "CREATE TABLE t2 (a int)",
            "PRAGMA query_only = 0; DELETE FROM t",
        ] {
            let e = run_read_only_unchecked(&st, &id, None, sql, ReadLimits::default())
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::ReadOnlyViolation, "{sql}: {e:?}");
        }
        assert_eq!(count(&st, &id).await, "1000", "data unchanged");
    }

    #[tokio::test]
    async fn sqlite_timeout_interrupts_and_the_next_read_works() {
        let (_d, st, id, _file) = sqlite_state().await;
        let limits = ReadLimits {
            max_rows: 10,
            timeout: Duration::from_millis(300),
        };
        let started = Instant::now();
        let e = run_ai_read(
            &st,
            &id,
            None,
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000000000) SELECT count(*) FROM n",
            limits,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout, "{e:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let r = run_ai_read(&st, &id, None, "SELECT 1 AS one", limits)
            .await
            .unwrap();
        assert_eq!(r.rows, vec![vec![Value::from(1)]]);
    }

    #[tokio::test]
    async fn sqlite_ai_read_never_uses_the_read_write_pool() {
        let (_d, st, id, _file) = sqlite_state().await;
        run_ai_read(&st, &id, None, "SELECT 1", ReadLimits::default())
            .await
            .unwrap();
        assert!(
            !st.pools().contains(&id),
            "the AI read opened the user's pool"
        );
    }

    #[test]
    fn sql_mode_drops_lexing_flags() {
        assert_eq!(
            safe_sql_mode(
                "ONLY_FULL_GROUP_BY,NO_BACKSLASH_ESCAPES,ANSI_QUOTES,STRICT_TRANS_TABLES"
            ),
            "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES"
        );
        assert_eq!(safe_sql_mode("ANSI,REAL_AS_FLOAT"), "REAL_AS_FLOAT");
        assert_eq!(safe_sql_mode("ORACLE"), "");
        assert_eq!(safe_sql_mode(""), "");
        assert_eq!(safe_sql_mode("X',Y"), "Y", "never builds a broken literal");
    }

    #[test]
    fn limits_sanitize_zero_and_the_timeout_error_names_the_limit() {
        // PG and MySQL read 0 as "no limit"; the read never may.
        assert_eq!(
            ReadLimits {
                max_rows: 0,
                timeout: Duration::ZERO
            }
            .sanitized(),
            ReadLimits {
                max_rows: 1,
                timeout: Duration::from_millis(1)
            }
        );
        let e = timeout_error(Duration::from_millis(300));
        assert_eq!(e.code, ErrorCode::Timeout);
        assert!(e.message.contains("300 ms"), "{}", e.message);
        assert!(e.detail.is_none());
    }

    /// A fake server error, so the code tables can be checked without PostgreSQL or SQLite.
    /// MySQL numbers live on the driver's own error type, so that arm stays integration-only.
    #[derive(Debug)]
    struct FakeDbError(&'static str, &'static str);
    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "fake {}", self.0)
        }
    }
    impl std::error::Error for FakeDbError {}
    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            self.1
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    /// The server's answer decides between a refused write, a stacked statement, the read's own
    /// limit, and the plain database error.
    #[test]
    fn server_codes_map_to_their_refusal() {
        let err = |code: &'static str, message: &'static str| {
            sqlx::Error::Database(Box::new(FakeDbError(code, message)))
        };
        let limit = Duration::from_secs(5);
        let map = |engine, e, sql: &str| map_read_error(engine, e, sql, limit);

        let e = map(
            SqlEngine::Pg,
            err("25006", "cannot execute INSERT in a read-only transaction"),
            "INSERT INTO t VALUES (1)",
        );
        assert_eq!(e.code, ErrorCode::ReadOnlyViolation);

        // A syntax error on stacked text is the server stopping at the second statement; the
        // same code on one statement stays the server's error.
        let e = map(
            SqlEngine::Pg,
            err("42601", "cannot insert multiple commands into a prepared statement"),
            "SELECT 1; DELETE FROM t",
        );
        assert_eq!(e.code, ErrorCode::ReadOnlyViolation);
        let e = map(SqlEngine::Pg, err("42601", "syntax error"), "SELECT 1");
        assert_eq!(e.code, ErrorCode::Database);

        let e = map(SqlEngine::Pg, err("57014", "canceling statement"), "SELECT 1");
        assert_eq!(e.code, ErrorCode::Timeout);
        assert!(e.message.contains("5 s"), "{}", e.message);

        // SQLite's extended result code: the low byte is the primary code.
        for code in ["1032", "8"] {
            let e = map(
                SqlEngine::Sqlite,
                err(code, "attempt to write a readonly database"),
                "INSERT INTO t VALUES (1)",
            );
            assert_eq!(e.code, ErrorCode::ReadOnlyViolation, "{code}");
        }
        let e = map(SqlEngine::Sqlite, err("9", "interrupted"), "SELECT 1");
        assert_eq!(e.code, ErrorCode::Timeout);
    }

    #[test]
    fn stacked_detection_ignores_semicolons_in_strings() {
        let many = |e, sql: &str| crate::core::sql::split::split(e, sql).len() >= 2;
        // A `;` inside a string literal is one statement: a 1064 on it stays `database`, not
        // "refused a stacked statement".
        assert!(!many(
            SqlEngine::Mysql,
            "SELECT id FROM orders WHERE status ILIKE 'a;b'"
        ));
        assert!(many(
            SqlEngine::Mysql,
            "SELECT 1; SET SESSION max_execution_time = 0"
        ));
        assert!(many(SqlEngine::Pg, "COMMIT; SELECT 1"));
        // Trailing semicolons and comments are still one statement.
        assert!(!many(SqlEngine::Mysql, "SELECT 1; -- x"));
    }
}
