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
use serde::Serialize;
use serde_json::Value;
use sqlx::mysql::{MySql, MySqlDatabaseError, MySqlPool};
use sqlx::postgres::{PgPool, Postgres};
use sqlx::sqlite::{Sqlite, SqliteConnection};
use sqlx::{Connection as _, Database, Either, Executor, Row};

use crate::core::connections::{self, Connection, Engine};
use crate::core::db::pool::{self, DbPool, OneConnection};
use crate::core::db::query::{self, CLOSE_TIMEOUT, ColumnMeta, Kill, SqliteHandle};
use crate::core::db::value::PREVIEW_BYTES;
use crate::core::error::{AppError, ErrorCode};
use crate::core::guard;
use crate::core::secrets::password_account;
use crate::core::state::AppState;

pub const DEFAULT_MAX_ROWS: usize = 500;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// The outer safety net fires this long after the database's own limit should have.
pub(crate) const GRACE: Duration = Duration::from_secs(1);
/// Bound for the server-side cancel sent by the safety net.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    pub max_rows: usize,
    pub timeout: Duration,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiReadResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    /// More rows exist than `max_rows`; they were not fetched.
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// MongoDB only: each document of the page as relaxed Extended JSON, one per row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<Value>>,
}

/// Runs AI-written `sql` read-only: the layer 1 guard, then the database-enforced read-only
/// session with `limits`. `database`: the connection's own when absent. On MongoDB `sql` is one
/// mongosh read statement.
pub async fn run_ai_read(
    state: &AppState,
    conn_id: &str,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let conn = connections::get(state, conn_id)?;
    if conn.engine == Engine::Mongo {
        return crate::core::mongo::ai::read(state, &conn, database, sql, limits).await;
    }
    guard::check_ai_read(sql, conn.engine)?;
    execute(state, &conn, database, sql, limits).await
}

/// Layers 2 and 3 only, so integration tests can prove that the database itself refuses a write
/// that got past layer 1. Never compiled into a normal build.
#[cfg(any(test, feature = "it"))]
pub async fn run_read_only_unchecked(
    state: &AppState,
    conn_id: &str,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let conn = connections::get(state, conn_id)?;
    execute(state, &conn, database, sql, limits).await
}

/// The server session running the read, so the safety net can stop it.
#[derive(Clone, Copy)]
enum Target {
    Pg(i32),
    MySql(i64),
    Sqlite(SqliteHandle),
}

type Slot = Arc<Mutex<Option<Target>>>;

fn put(slot: &Slot, t: Option<Target>) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = t;
}

#[derive(Default)]
struct Fetched {
    columns: Option<Vec<ColumnMeta>>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
    /// Row data exceeded `ROW_BYTES_LIMIT`; the read must abort.
    over_budget: bool,
}

async fn execute(
    state: &AppState,
    conn: &Connection,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    // A zero timeout means "no limit" to PG and MySQL, so never send one.
    let limits = ReadLimits {
        max_rows: limits.max_rows.max(1),
        timeout: limits.timeout.max(Duration::from_millis(1)),
    };
    let database = connections::target_database(conn, database);
    let started = Instant::now();
    let slot: Slot = Arc::default();
    let work = run_engine(state, conn, &database, sql, limits, slot.clone());
    tokio::pin!(work);
    let fetched = tokio::select! {
        r = &mut work => r?,
        _ = tokio::time::sleep(limits.timeout + GRACE) => {
            // `work` (and its connection) stays alive until this returns, so the SQLite handle
            // in the slot is still valid while it is interrupted.
            stop(state, conn, &database, &slot).await;
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
    state: &AppState,
    conn: &Connection,
    database: &str,
    sql: &str,
    limits: ReadLimits,
    slot: Slot,
) -> Result<Fetched, AppError> {
    if conn.engine == Engine::Sqlite {
        // Never the user's read-write pool.
        return sqlite_read(conn, sql, limits, &slot).await;
    }
    match connections::pool_for(state, &conn.id, Some(database))? {
        DbPool::Pg(p) => pg_read(p, sql, limits, &slot).await,
        DbPool::MySql(p) => mysql_read(p, sql, limits, &slot).await,
        DbPool::Sqlite(_) => Err(AppError::internal("SQLite reads never use the pool")),
    }
}

/// Safety net: the database did not stop the read in time. Cancels it on the server through a
/// fresh, separate connection: borrowing the user's pool could queue behind a saturated pool,
/// and the read's own settings must never touch it.
async fn stop(state: &AppState, conn: &Connection, database: &str, slot: &Slot) {
    let kill = {
        let guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(Target::Sqlite(h)) => {
                // SAFETY: the connection that owns the handle is alive (see `execute`), and
                // `sqlite3_interrupt` may be called from any thread.
                unsafe { libsqlite3_sys::sqlite3_interrupt(h.0) };
                None
            }
            Some(Target::Pg(pid)) => Some(Kill::Pg(pid)),
            Some(Target::MySql(id)) => Some(Kill::MySql(id)),
            None => None,
        }
    };
    if let Some(k) = kill {
        let _ = tokio::time::timeout(STOP_TIMEOUT, server_kill(state, conn, database, k)).await;
    }
}

/// `pg_cancel_backend` / `KILL QUERY` on a one-off connection that is closed right after.
/// Public under `it` so the integration test can drive the kill directly: the safety net only
/// fires when a server-side limit fails first, which a test cannot arrange on its own.
#[cfg(feature = "it")]
pub async fn server_kill(
    state: &AppState,
    conn: &Connection,
    database: &str,
    kill: Kill,
) -> Result<(), AppError> {
    server_kill_inner(state, conn, database, kill).await
}

#[cfg(not(feature = "it"))]
async fn server_kill(
    state: &AppState,
    conn: &Connection,
    database: &str,
    kill: Kill,
) -> Result<(), AppError> {
    server_kill_inner(state, conn, database, kill).await
}

async fn server_kill_inner(
    state: &AppState,
    conn: &Connection,
    database: &str,
    kill: Kill,
) -> Result<(), AppError> {
    let password = state.secrets.get(&password_account(&conn.id))?;
    let one = pool::connect_one(conn, database, password.as_deref()).await?;
    match (one, kill) {
        (OneConnection::Pg(mut c), Kill::Pg(pid)) => {
            sqlx::query("SELECT pg_cancel_backend($1)")
                .bind(pid)
                .execute(&mut c)
                .await?;
            let _ = c.close().await;
        }
        (OneConnection::MySql(mut c), Kill::MySql(id)) => {
            c.execute(sqlx::query(&format!("KILL QUERY {id}"))).await?;
            let _ = c.close().await;
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn timeout_error(limit: Duration) -> AppError {
    let shown = if limit < Duration::from_secs(1) {
        format!("{} ms", limit.as_millis())
    } else {
        format!("{} s", limit.as_secs_f64())
    };
    AppError::new(
        ErrorCode::Timeout,
        format!("The AI query did not finish within {shown} and was stopped"),
    )
}

enum Refusal {
    ReadOnly,
    /// More than one statement reached the server (it rejects a stacked prepared statement).
    Stacked,
    Timeout,
}

/// True when `sql` holds more than one statement, counted with the same tokenizer the splitter
/// uses. A `;` inside a string literal does not count.
fn has_multiple_statements(engine: Engine, sql: &str) -> bool {
    crate::core::sql::split::split(engine, sql).len() >= 2
}

/// The server's answer to the read, with read-only violations and the read's own limits
/// recognised; everything else keeps the normal mapping.
fn map_read_error(engine: Engine, e: sqlx::Error, sql: &str, limit: Duration) -> AppError {
    let stacked = has_multiple_statements(engine, sql);
    let refusal = match &e {
        sqlx::Error::Database(db) => match engine {
            // 25006 read_only_sql_transaction; 57014 query_canceled (statement_timeout);
            // 55P03 lock_not_available (lock_timeout).
            Engine::Pg => match db.code().as_deref() {
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
            Engine::Mysql => match db
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
            Engine::Sqlite => match db
                .code()
                .and_then(|c| c.parse::<i32>().ok())
                .map(|c| c & 0xff)
            {
                Some(8 | 23) => Some(Refusal::ReadOnly),
                Some(9) => Some(Refusal::Timeout),
                _ => None,
            },
            Engine::Mongo => None,
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

type CellFn<DB> = fn(&<DB as Database>::Row, usize, Option<usize>) -> Value;

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
                out.columns = Some(row.columns().iter().map(query::meta).collect());
            }
            if out.rows.len() >= max_rows {
                out.truncated = true;
                break;
            }
            let values: Vec<Value> = (0..row.len())
                .map(|i| cell(&row, i, Some(PREVIEW_BYTES)))
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
        && let Ok(d) = (&mut *conn).describe(sql).await
    {
        out.columns = Some(d.columns().iter().map(query::meta).collect());
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

async fn pg_read(
    p: PgPool,
    sql: &str,
    limits: ReadLimits,
    slot: &Slot,
) -> Result<Fetched, AppError> {
    let mut c = p.acquire().await?;
    // Also on an early return or when the safety net drops this future.
    c.close_on_drop();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *c)
        .await?;
    put(slot, Some(Target::Pg(pid)));
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
    // text with raw_sql afterwards so results come back in text format through pg_cell.
    (&mut *c)
        .prepare(sql)
        .await
        .map_err(|e| map_read_error(Engine::Pg, e, sql, limits.timeout))?;
    let r = fetch::<Postgres>(&mut c, sql, limits.max_rows, false, query::pg_cell)
        .await
        .map_err(|e| map_read_error(Engine::Pg, e, sql, limits.timeout));
    // After a cut-off read the server is still sending rows; closing the connection ends the
    // transaction without reading them.
    if r.as_ref().is_ok_and(|f| !f.truncated && !f.over_budget) {
        let _ =
            tokio::time::timeout(CLOSE_TIMEOUT, (&mut *c).execute(sqlx::raw_sql("ROLLBACK"))).await;
    }
    put(slot, None);
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, c.detach().close()).await;
    r.and_then(|f| {
        if f.over_budget {
            Err(over_budget_error())
        } else {
            Ok(f)
        }
    })
}

/// MySQL schemas an AI read may never run inside: their unqualified tables (`user`, `processlist`,
/// `general_log`, …) resolve to system tables the privacy rule blocks only when they are named.
fn is_system_schema(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "mysql" | "sys" | "performance_schema" | "information_schema"
    )
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

async fn mysql_read(
    p: MySqlPool,
    sql: &str,
    limits: ReadLimits,
    slot: &Slot,
) -> Result<Fetched, AppError> {
    let mut c = p.acquire().await?;
    c.close_on_drop();
    let (id, version, mode, db): (i64, String, String, Option<String>) = sqlx::query_as(
        "SELECT CAST(CONNECTION_ID() AS SIGNED), VERSION(), CAST(@@SESSION.sql_mode AS CHAR), DATABASE()",
    )
    .fetch_one(&mut *c)
    .await?;
    put(slot, Some(Target::MySql(id)));
    if db.as_deref().is_some_and(is_system_schema) {
        return Err(AppError::new(
            ErrorCode::GuardRejected,
            "AI reads are blocked for privacy inside a MySQL system schema: unqualified table \
             names would resolve to system tables",
        ));
    }
    let ms = limits.timeout.as_millis().max(1);
    let secs = limits.timeout.as_secs().max(1);
    let time_limit = if version.to_ascii_lowercase().contains("mariadb") {
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
    let r = fetch::<MySql>(&mut c, sql, limits.max_rows, true, query::mysql_cell_binary)
        .await
        .map_err(|e| map_read_error(Engine::Mysql, e, sql, limits.timeout));
    if r.as_ref().is_ok_and(|f| !f.truncated && !f.over_budget) {
        let _ =
            tokio::time::timeout(CLOSE_TIMEOUT, (&mut *c).execute(sqlx::raw_sql("ROLLBACK"))).await;
    }
    put(slot, None);
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, c.detach().close()).await;
    r.and_then(|f| {
        if f.over_budget {
            Err(over_budget_error())
        } else {
            Ok(f)
        }
    })
}

async fn sqlite_read(
    conn: &Connection,
    sql: &str,
    limits: ReadLimits,
    slot: &Slot,
) -> Result<Fetched, AppError> {
    // SQLITE_OPEN_READONLY (what `mode=ro` sets), without building a URI from the path.
    let opts = pool::sqlite_options(conn, true)?;
    let mut c = SqliteConnection::connect_with(&opts).await?;
    (&mut c)
        .execute(sqlx::raw_sql("PRAGMA query_only = ON"))
        .await?;
    let deadline = Instant::now() + limits.timeout;
    {
        let mut h = c.lock_handle().await?;
        let raw = h.as_raw_handle().as_ptr();
        put(slot, Some(Target::Sqlite(SqliteHandle(raw))));
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
    let r = fetch::<Sqlite>(&mut c, sql, limits.max_rows, false, query::sqlite_cell)
        .await
        .map_err(|e| map_read_error(Engine::Sqlite, e, sql, limits.timeout));
    // Cleared before the connection closes, so the safety net never interrupts a freed handle.
    put(slot, None);
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, c.close()).await;
    r.and_then(|f| {
        if f.over_budget {
            Err(over_budget_error())
        } else {
            Ok(f)
        }
    })
}

/// Pragmas the authorizer lets through, in read (no value) form only — the same schema-info
/// list the layer 1 guard accepts.
const AUTH_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "foreign_key_list",
];

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
            if c_str(arg1).is_some_and(|n| AUTH_PRAGMAS.contains(&n.as_str())) {
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
    use crate::core::db::query::RunRequest;
    use crate::core::secrets::MemoryStore;
    use sqlx::sqlite::SqliteConnectOptions;

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
        let conn =
            connections::create(&st, input(Engine::Sqlite, 0, &file.to_string_lossy()), None)
                .unwrap();
        (dir, st, conn.id, file)
    }

    async fn count(st: &AppState, id: &str) -> String {
        let r = query::run(
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
    /// attempt (an immediate `guard_rejected`), while an allowed read does try to connect.
    #[tokio::test]
    async fn guard_runs_before_any_database_contact() {
        let (_d, st) = state();
        for engine in [Engine::Pg, Engine::Mysql] {
            let c = connections::create(&st, input(engine, 1, ""), None).unwrap();
            let e = run_ai_read(&st, &c.id, None, "DELETE FROM t", ReadLimits::default())
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::GuardRejected, "{engine:?}: {e:?}");
            // The pool retries a refused port until the safety net stops it.
            let quick = ReadLimits {
                max_rows: 1,
                timeout: Duration::from_millis(200),
            };
            let e = run_ai_read(&st, &c.id, None, "SELECT 1", quick)
                .await
                .unwrap_err();
            assert!(
                matches!(e.code, ErrorCode::ConnectFailed | ErrorCode::Timeout),
                "{engine:?}: {e:?}"
            );
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
            !st.pools.contains(&id),
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
    fn limits_default_and_json_shape() {
        assert_eq!(
            ReadLimits::default(),
            ReadLimits {
                max_rows: 500,
                timeout: Duration::from_secs(15)
            }
        );
        let v = serde_json::to_value(AiReadResult {
            columns: vec![],
            rows: vec![],
            truncated: true,
            elapsed_ms: 3,
            documents: None,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"columns": [], "rows": [], "truncated": true, "elapsedMs": 3})
        );
        assert_eq!(
            timeout_error(Duration::from_secs(15)).message,
            "The AI query did not finish within 15 s and was stopped"
        );
        assert_eq!(
            timeout_error(Duration::from_millis(300)).message,
            "The AI query did not finish within 300 ms and was stopped"
        );
    }

    #[test]
    fn stacked_detection_ignores_semicolons_in_strings() {
        // A `;` inside a string literal is one statement: a 1064 on it stays `database`, not
        // "refused a stacked statement".
        assert!(!has_multiple_statements(
            Engine::Mysql,
            "SELECT id FROM orders WHERE status ILIKE 'a;b'"
        ));
        assert!(has_multiple_statements(
            Engine::Mysql,
            "SELECT 1; SET SESSION max_execution_time = 0"
        ));
        assert!(has_multiple_statements(Engine::Pg, "COMMIT; SELECT 1"));
        // Trailing semicolons and comments are still one statement.
        assert!(!has_multiple_statements(Engine::Mysql, "SELECT 1; -- x"));
    }

    #[test]
    fn system_schemas_are_recognised() {
        // `SELECT DATABASE()` answers may vary in case; the check folds it.
        for s in [
            "mysql",
            "MySQL",
            "sys",
            "performance_schema",
            "information_schema",
        ] {
            assert!(is_system_schema(s), "{s}");
        }
        for s in ["shop", "analytics", "test", ""] {
            assert!(!is_system_schema(s), "{s}");
        }
    }
}
