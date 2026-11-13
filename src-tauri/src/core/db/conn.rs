//! One SQL connection type for every engine: acquiring from a pool, opening one connection
//! outside any pool, the server session id, the kill, the transaction probe and the registry
//! attachment.
//!
//! A `Conn` is either checked out of a pool (and goes back to it) or detached from one (and is
//! always closed). The tab session, the AI read and an approved write all detach, so nothing
//! those set (`SET`, `USE`, temp tables, an open transaction) can reach the pool.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::mysql::{MySql, MySqlConnection};
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgConnection;
use sqlx::sqlite::{Sqlite, SqliteConnection};
use sqlx::{Connection as _, Database, Either, Executor, Row};
use tokio_util::sync::CancellationToken;

use crate::core::db::pool::{DbPool, sqlite_options};
use crate::core::db::reader::{self, CellFn};
use crate::core::db::value::Preview;
use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::query_registry::{QueryRegistry, Session, SessionGuard, SqliteHandle};
use crate::core::connections::Connection;
use crate::core::result::{ColumnMeta, StatementResult};

/// Bound for closing a connection whose session state must not be reused.
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Bound for a transaction-state probe, a tab commit or rollback: the connection dies after.
pub(crate) const TX_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for opening the extra connection that sends a cancel.
const CANCEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// `rows_affected` for a query result of any engine, so the generic runner needs no
/// per-call-site function.
trait RowsAffected {
    fn rows_affected(&self) -> u64;
}

impl RowsAffected for sqlx::postgres::PgQueryResult {
    fn rows_affected(&self) -> u64 {
        sqlx::postgres::PgQueryResult::rows_affected(self)
    }
}

impl RowsAffected for sqlx::mysql::MySqlQueryResult {
    fn rows_affected(&self) -> u64 {
        sqlx::mysql::MySqlQueryResult::rows_affected(self)
    }
}

impl RowsAffected for sqlx::sqlite::SqliteQueryResult {
    fn rows_affected(&self) -> u64 {
        sqlx::sqlite::SqliteQueryResult::rows_affected(self)
    }
}

pub enum Owner<DB: Database> {
    Pooled(PoolConnection<DB>),
    Detached(DB::Connection),
}

impl<DB> Owner<DB>
where
    DB: Database,
    DB::Connection: sqlx::Connection<Database = DB>,
{
    pub fn connection(&mut self) -> &mut DB::Connection {
        match self {
            Owner::Pooled(c) => &mut *c,
            Owner::Detached(c) => c,
        }
    }

    /// A pooled connection must not go back to the pool: whatever it holds (session settings,
    /// an open transaction) dies with it.
    pub fn close_on_drop(&mut self) {
        if let Owner::Pooled(c) = self {
            c.close_on_drop();
        }
    }

    /// A close that fails or hangs leaves the connection dead either way, so the outcome is
    /// not reported.
    pub async fn close(self) {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
            match self {
                Owner::Pooled(c) => c.detach().close().await,
                Owner::Detached(c) => c.close().await,
            }
        })
        .await;
    }

    pub async fn finish(self, reuse: bool) {
        match self {
            Owner::Pooled(c) if reuse => drop(c),
            other => other.close().await,
        }
    }
}

/// One live SQL server session.
pub enum Conn {
    Pg(Owner<sqlx::Postgres>),
    MySql(Owner<MySql>),
    Sqlite(Owner<Sqlite>),
}

/// What the server reports about its transaction.
pub struct Tx {
    pub engine: SqlEngine,
    pub open: bool,
    /// PostgreSQL: a statement failed inside the transaction (25P02), so only ROLLBACK helps.
    pub aborted: bool,
    /// Whether the transaction holds a write. `None` when the engine cannot tell cheaply.
    pub changed: Option<bool>,
}

impl Tx {
    /// The tab's "pending" flag after this probe, given what the tracker knew before.
    pub(crate) fn dirty(&self, was: bool) -> bool {
        match self.engine {
            // SQLite reports the state of the transaction precisely.
            SqlEngine::Sqlite => self.changed.unwrap_or(was),
            // MySQL only reports whether one is open; the tracker's flag is the write signal.
            SqlEngine::Mysql => was && self.open,
            // PG reports the write xact id, NULL in a transaction that wrote nothing yet (a
            // `SET` only one, for example); `was` only stands in when the probe had no answer.
            SqlEngine::Pg => self.changed.unwrap_or(was) && self.open,
        }
    }
}

impl Conn {
    pub fn engine(&self) -> SqlEngine {
        match self {
            Conn::Pg(_) => SqlEngine::Pg,
            Conn::MySql(_) => SqlEngine::Mysql,
            Conn::Sqlite(_) => SqlEngine::Sqlite,
        }
    }

    pub async fn acquire(pool: &DbPool) -> Result<Conn, AppError> {
        Ok(match pool {
            DbPool::Pg(p) => Conn::Pg(Owner::Pooled(p.acquire().await?)),
            DbPool::MySql(p) => Conn::MySql(Owner::Pooled(p.acquire().await?)),
            DbPool::Sqlite(p) => Conn::Sqlite(Owner::Pooled(p.acquire().await?)),
        })
    }

    /// A connection of its own, detached from its pool at once: the tab session, an approved
    /// write and a cancelled save keep it out of the pool's connection count.
    pub async fn detached(pool: &DbPool) -> Result<Conn, AppError> {
        let c = Self::acquire(pool).await?;
        Ok(match c {
            Conn::Pg(o) => Conn::Pg(detach(o)),
            Conn::MySql(o) => Conn::MySql(detach(o)),
            Conn::Sqlite(o) => Conn::Sqlite(detach(o)),
        })
    }

    /// One connection outside any pool. This is the only way to open one: a cancel must not
    /// queue behind a saturated pool, and it must not borrow the user's.
    pub async fn one_off(pool: &DbPool) -> Result<Conn, AppError> {
        let timed_out = || {
            AppError::new(
                ErrorCode::Timeout,
                format!(
                    "Could not reach the server to cancel within {} s",
                    CANCEL_CONNECT_TIMEOUT.as_secs()
                ),
            )
        };
        Ok(match pool {
            DbPool::Pg(p) => {
                let c = tokio::time::timeout(
                    CANCEL_CONNECT_TIMEOUT,
                    PgConnection::connect_with(p.connect_options().as_ref()),
                )
                .await
                .map_err(|_| timed_out())??;
                Conn::Pg(Owner::Detached(c))
            }
            DbPool::MySql(p) => {
                let c = tokio::time::timeout(
                    CANCEL_CONNECT_TIMEOUT,
                    MySqlConnection::connect_with(p.connect_options().as_ref()),
                )
                .await
                .map_err(|_| timed_out())??;
                Conn::MySql(Owner::Detached(c))
            }
            DbPool::Sqlite(_) => {
                return Err(AppError::internal("SQLite never opens a second connection"))
            }
        })
    }

    /// A fresh read-only SQLite connection outside any pool: the AI read must never touch the
    /// user's read-write pool.
    pub async fn sqlite_read_only(conn: &Connection) -> Result<Conn, AppError> {
        let c = SqliteConnection::connect_with(&sqlite_options(conn, true)?).await?;
        Ok(Conn::Sqlite(Owner::Detached(c)))
    }

    /// The engine's own connection type, for a generic row reader that needs it.
    pub fn as_pg(&mut self) -> Option<&mut PgConnection> {
        match self {
            Conn::Pg(o) => Some(o.connection()),
            _ => None,
        }
    }

    pub fn as_mysql(&mut self) -> Option<&mut MySqlConnection> {
        match self {
            Conn::MySql(o) => Some(o.connection()),
            _ => None,
        }
    }

    pub fn as_sqlite(&mut self) -> Option<&mut SqliteConnection> {
        match self {
            Conn::Sqlite(o) => Some(o.connection()),
            _ => None,
        }
    }

    pub fn close_on_drop(&mut self) {
        match self {
            Conn::Pg(o) => o.close_on_drop(),
            Conn::MySql(o) => o.close_on_drop(),
            Conn::Sqlite(o) => o.close_on_drop(),
        }
    }

    /// Hands the connection back to its pool (`reuse`) or closes it, rolling back whatever it
    /// still holds on the server.
    pub async fn settle(self, reuse: bool) {
        match self {
            Conn::Pg(o) => o.finish(reuse).await,
            Conn::MySql(o) => o.finish(reuse).await,
            Conn::Sqlite(o) => o.finish(reuse).await,
        }
    }

    pub async fn close(self) {
        match self {
            Conn::Pg(o) => o.close().await,
            Conn::MySql(o) => o.close().await,
            Conn::Sqlite(o) => o.close().await,
        }
    }

    /// The server session another connection can stop: `pg_backend_pid`, `CONNECTION_ID`, or
    /// the SQLite handle `sqlite3_interrupt` needs.
    pub async fn session(&mut self) -> Result<Session, AppError> {
        match self {
            Conn::Pg(o) => {
                let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(o.connection())
                    .await?;
                Ok(Session::Pg(pid))
            }
            Conn::MySql(o) => {
                let id: i64 = sqlx::query_scalar("SELECT CAST(CONNECTION_ID() AS SIGNED)")
                    .fetch_one(o.connection())
                    .await?;
                Ok(Session::MySql(id))
            }
            Conn::Sqlite(o) => {
                let mut h = o.connection().lock_handle().await?;
                Ok(Session::Sqlite(SqliteHandle(h.as_raw_handle().as_ptr())))
            }
        }
    }

    /// Registers this connection's server session for `query_id`; the registry forgets it when
    /// the returned guard drops, before the connection can go back to the pool.
    pub async fn attach<'a>(
        &mut self,
        queries: &'a QueryRegistry,
        query_id: Option<&str>,
    ) -> Result<Attached<'a>, AppError> {
        let Some(id) = query_id else {
            return Ok(Attached::default());
        };
        let session = self.session().await?;
        Ok(Attached {
            _guard: Some(queries.attach(id, session)),
        })
    }

    /// MySQL's `KILL QUERY` can end `SLEEP()` normally instead of with an error, so a cancelled
    /// run whose statements all answered is still a cancel.
    pub fn cancel_result(&self, cancel: &CancellationToken) -> Result<(), AppError> {
        if self.engine() == SqlEngine::Mysql && cancel.is_cancelled() {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }

    /// Whether the server is inside an explicit transaction, and whether that transaction has
    /// changes. `None` when it cannot tell (error or timeout).
    ///
    /// MySQL has no read-only way to ask: `@@in_transaction` exists only on MariaDB, so MySQL
    /// answers through `SET TRANSACTION READ WRITE`, which fails with 25001 while a transaction
    /// is open. That statement presets the *next* transaction's access mode; running it on the
    /// long-lived tab session is harmless (the tab's transaction is read-write anyway) and it is
    /// what makes implicit-commit detection possible there. On a short-lived connection the
    /// connection is closed right after, so the preset dies with it.
    pub async fn tx(&mut self, bound: Duration) -> Option<Tx> {
        match self {
            Conn::Pg(o) => {
                let probe = o.connection().fetch_one(sqlx::raw_sql(
                    "SELECT now() <> statement_timestamp(), \
                     pg_current_xact_id_if_assigned() IS NOT NULL",
                ));
                match tokio::time::timeout(bound, probe).await.ok()? {
                    Ok(row) => Some(Tx {
                        engine: SqlEngine::Pg,
                        open: row.try_get(0).unwrap_or(false),
                        aborted: false,
                        changed: Some(row.try_get(1).unwrap_or(false)),
                    }),
                    // 25P02: a statement failed inside the transaction — it is open and aborted.
                    Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("25P02") => {
                        Some(Tx {
                            engine: SqlEngine::Pg,
                            open: true,
                            aborted: true,
                            changed: None,
                        })
                    }
                    Err(_) => None,
                }
            }
            Conn::MySql(o) => {
                tokio::time::timeout(bound, mysql_probe(o.connection()))
                    .await
                    .ok()
                    .flatten()
            }
            Conn::Sqlite(o) => {
                let mut h = o.connection().lock_handle().await.ok()?;
                // SAFETY: the locked handle gives exclusive access to the live connection.
                let state = unsafe {
                    libsqlite3_sys::sqlite3_txn_state(h.as_raw_handle().as_ptr(), std::ptr::null())
                };
                if state < 0 {
                    return None;
                }
                Some(Tx {
                    engine: SqlEngine::Sqlite,
                    open: state != libsqlite3_sys::SQLITE_TXN_NONE,
                    aborted: false,
                    changed: Some(state == libsqlite3_sys::SQLITE_TXN_WRITE),
                })
            }
        }
    }

    /// The tab's "pending" state after [`Conn::tx`]: does the open transaction hold changes the
    /// user would lose? `was` is what that probe told the tracker; PG and SQLite report the
    /// write there, so only MySQL needs a second question.
    pub async fn has_changes(&mut self, was: bool) -> bool {
        match self {
            Conn::Pg(_) | Conn::Sqlite(_) => was,
            Conn::MySql(o) => {
                if !was {
                    return false;
                }
                // The session's own `innodb_trx` row: present only while uncommitted locks or
                // writes exist.
                let n: i64 = match sqlx::query_scalar(
                    "SELECT CAST(count(*) AS SIGNED) FROM information_schema.innodb_trx \
                     WHERE trx_mysql_thread_id = CONNECTION_ID()",
                )
                .fetch_one(o.connection())
                .await
                {
                    Ok(n) => n,
                    Err(_) => return was,
                };
                n > 0
            }
        }
    }

    /// Whether the connection still answers. An aborted PG transaction (25P02) still answers.
    pub async fn alive(&mut self, bound: Duration) -> bool {
        let ping = match self {
            Conn::Pg(o) => tokio::time::timeout(bound, o.connection().ping()).await,
            Conn::MySql(o) => tokio::time::timeout(bound, o.connection().ping()).await,
            Conn::Sqlite(o) => tokio::time::timeout(bound, o.connection().ping()).await,
        };
        match ping {
            Ok(Ok(())) => true,
            Ok(Err(sqlx::Error::Database(e))) => e.code().as_deref() == Some("25P02"),
            _ => false,
        }
    }

    pub async fn begin(&mut self) -> Result<(), AppError> {
        let sql = self.engine().begin_transaction();
        match tokio::time::timeout(TX_TIMEOUT, self.execute_raw(sql)).await {
            Ok(r) => Ok(r?),
            // A timed-out BEGIN may still have started the transaction on the server; the
            // caller closes the session.
            Err(e) => Err(AppError::new(ErrorCode::Timeout, e.to_string())),
        }
    }

    /// COMMIT (`true`) or ROLLBACK, bounded: a timed-out end may still have landed, so the
    /// caller closes the session.
    pub async fn end_tx(&mut self, commit: bool) -> Result<(), AppError> {
        match tokio::time::timeout(TX_TIMEOUT, self.end_tx_unbounded(commit)).await {
            Ok(r) => r,
            Err(e) => Err(AppError::new(ErrorCode::Timeout, e.to_string())),
        }
    }

    /// COMMIT or ROLLBACK with no deadline: the approved write needs COMMIT's reply to know
    /// the result, and the grid save races its own cancel around the send.
    pub async fn end_tx_unbounded(&mut self, commit: bool) -> Result<(), AppError> {
        self.execute_raw(if commit { "COMMIT" } else { "ROLLBACK" })
            .await
            .map_err(AppError::from)
    }

    pub async fn execute_all(&mut self, sql: &str) -> Result<u64, AppError> {
        Ok(match self {
            Conn::Pg(o) => o.connection().execute(sqlx::raw_sql(sql)).await?.rows_affected(),
            Conn::MySql(o) => o.connection().execute(sqlx::raw_sql(sql)).await?.rows_affected(),
            Conn::Sqlite(o) => o.connection().execute(sqlx::raw_sql(sql)).await?.rows_affected(),
        })
    }

    /// Runs a savepoint statement (`SAVEPOINT …`, `ROLLBACK TO …`, `RELEASE …`) and discards
    /// the answer. MySQL sends these through the text protocol, which accepts the forms its
    /// prepared protocol may not.
    pub async fn control(&mut self, sql: &str) -> Result<(), AppError> {
        match self {
            Conn::Pg(o) => {
                sqlx::query(sql).execute(o.connection()).await?;
            }
            Conn::MySql(o) => {
                o.connection().fetch_optional(sqlx::raw_sql(sql)).await?;
            }
            Conn::Sqlite(o) => {
                sqlx::query(sql).execute(o.connection()).await?;
            }
        }
        Ok(())
    }

    async fn execute_raw(&mut self, sql: &str) -> Result<(), sqlx::Error> {
        match self {
            Conn::Pg(o) => {
                o.connection().execute(sqlx::raw_sql(sql)).await?;
            }
            Conn::MySql(o) => {
                o.connection().execute(sqlx::raw_sql(sql)).await?;
            }
            Conn::Sqlite(o) => {
                o.connection().execute(sqlx::raw_sql(sql)).await?;
            }
        }
        Ok(())
    }

    /// One statement of a run; the connection is dispatched here so no caller repeats the
    /// engine match. `in_session` drains reads past `limit` like writes.
    pub async fn run_one(
        &mut self,
        source_sql: &str,
        execution_sql: &str,
        read: bool,
        limit: usize,
        in_session: bool,
    ) -> Result<(Vec<StatementResult>, bool), sqlx::Error> {
        match self {
            Conn::Pg(o) => {
                run_one::<sqlx::Postgres>(
                    o.connection(),
                    source_sql,
                    execution_sql,
                    read,
                    limit,
                    in_session,
                    reader::pg_cell::<Preview>,
                )
                .await
            }
            Conn::MySql(o) => {
                run_one::<MySql>(
                    o.connection(),
                    source_sql,
                    execution_sql,
                    read,
                    limit,
                    in_session,
                    reader::mysql_cell::<Preview>,
                )
                .await
            }
            Conn::Sqlite(o) => {
                run_one::<Sqlite>(
                    o.connection(),
                    source_sql,
                    execution_sql,
                    read,
                    limit,
                    in_session,
                    reader::sqlite_cell::<Preview>,
                )
                .await
            }
        }
    }

    /// An autocommit run: every statement in order, one result set each on the common path.
    /// The flag says a read stopped mid-result, so the connection must not be reused.
    pub async fn run_all(
        &mut self,
        stmts: &[String],
        execution_stmts: &[String],
        reads: &[bool],
        limit: usize,
        cancel: &CancellationToken,
        done: &AtomicUsize,
    ) -> Result<(Vec<StatementResult>, bool), AppError> {
        let mut out = Vec::with_capacity(stmts.len());
        let mut cut_off = false;
        let n = stmts.len();
        for (i, sql) in stmts.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            match self
                .run_one(sql, &execution_stmts[i], reads[i], limit, false)
                .await
            {
                Ok((r, cut)) => {
                    cut_off = cut;
                    out.extend(r);
                    done.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) if cancel.is_cancelled() => return Err(AppError::cancelled()),
                Err(e) => {
                    let mut e = AppError::from(e);
                    if n > 1 {
                        e.message = format!("Statement {} of {n}: {}", i + 1, e.message);
                    }
                    return Err(e);
                }
            }
        }
        self.cancel_result(cancel)?;
        Ok((out, cut_off))
    }
}

fn detach<DB: Database>(o: Owner<DB>) -> Owner<DB>
where
    DB::Connection: sqlx::Connection<Database = DB>,
{
    match o {
        Owner::Pooled(c) => Owner::Detached(c.detach()),
        Owner::Detached(c) => Owner::Detached(c),
    }
}

/// The run registry's session, forgotten when this drops. Hold it until the statements that can
/// be killed are finished, and drop it before asking the connection anything else: a cancel then
/// sets the flag but sends no kill.
#[derive(Default)]
pub struct Attached<'a> {
    _guard: Option<SessionGuard<'a>>,
}

/// Sends the server kill for `session`, or interrupts a SQLite statement in this process.
/// A cancel must never queue behind a busy pool: an idle pooled connection is used when there is
/// one, and a fresh connection outside the pool otherwise. `pool` is the session's own pool and
/// is unused for SQLite.
pub async fn kill(pool: Option<&DbPool>, session: Session) -> Result<(), AppError> {
    if let Session::Sqlite(h) = session {
        // SAFETY: `SqliteHandle` is in the registry only while its connection is alive, and
        // `sqlite3_interrupt` may be called from any thread, including on a statement in flight.
        unsafe { libsqlite3_sys::sqlite3_interrupt(h.0) };
        return Ok(());
    }
    let pool = pool.ok_or_else(|| AppError::internal("No pool to cancel this session"))?;
    let (mut c, temporary) = match try_acquired(pool) {
        Some(c) => (c, false),
        None => (Conn::one_off(pool).await?, true),
    };
    let r = c.kill_over(session).await;
    c.settle(!temporary).await;
    r
}

fn try_acquired(pool: &DbPool) -> Option<Conn> {
    match pool {
        DbPool::Pg(p) => p.try_acquire().map(|c| Conn::Pg(Owner::Pooled(c))),
        DbPool::MySql(p) => p.try_acquire().map(|c| Conn::MySql(Owner::Pooled(c))),
        DbPool::Sqlite(p) => p.try_acquire().map(|c| Conn::Sqlite(Owner::Pooled(c))),
    }
}

impl Conn {
    /// Sends the server kill over this connection, which must be the session's own engine.
    async fn kill_over(&mut self, session: Session) -> Result<(), AppError> {
        match (self, session) {
            (Conn::Pg(o), Session::Pg(pid)) => {
                sqlx::query("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .execute(o.connection())
                    .await?;
            }
            (Conn::MySql(o), Session::MySql(id)) => {
                o.connection()
                    .execute(sqlx::query(&format!("KILL QUERY {id}")))
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Statements that may return a result set even when it has no rows.
fn looks_like_read(sql: &str) -> bool {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix('#') {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix('(') {
            s = rest.trim_start();
        } else {
            break;
        }
    }
    let word: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "select"
            | "with"
            | "values"
            | "show"
            | "explain"
            | "describe"
            | "desc"
            | "pragma"
            | "table"
    )
}

/// The columns of `sql`, for a result that came back without a row to describe them: no rows at
/// all, or a read stopped at its cap. `None` when the server cannot describe the statement
/// (e.g. `SHOW`).
pub(crate) async fn describe_columns<DB>(
    conn: &mut DB::Connection,
    sql: &str,
) -> Option<Vec<ColumnMeta>>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    (&mut *conn)
        .describe(sql)
        .await
        .ok()
        .map(|d| d.columns().iter().map(reader::meta).collect())
}

#[derive(Default)]
struct ResultSet {
    columns: Option<Vec<ColumnMeta>>,
    rows: Vec<Vec<Value>>,
    truncated: bool,
    changed: u64,
}

/// Runs one piece of SQL and returns one result per result set: one for a split statement,
/// several for text the splitter could not split. The `bool` is true when a read stopped
/// mid-result at `limit` and left the connection unusable; `in_session` holds the run's
/// connection, so a session-mode read drains like a write instead (it never sets the flag).
async fn run_one<DB>(
    conn: &mut DB::Connection,
    source_sql: &str,
    execution_sql: &str,
    read: bool,
    limit: usize,
    in_session: bool,
    cell: CellFn<DB>,
) -> Result<(Vec<StatementResult>, bool), sqlx::Error>
where
    DB: Database,
    DB::QueryResult: RowsAffected,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    let started = Instant::now();
    let mut sets: Vec<ResultSet> = Vec::new();
    let mut cur = ResultSet::default();
    let mut cut_off = false;
    {
        let mut stream = sqlx::raw_sql(execution_sql).fetch_many(&mut *conn);
        while let Some(item) = stream.try_next().await? {
            match item {
                // Every engine ends each result set or command with its own completion.
                Either::Left(done) => {
                    cur.changed += RowsAffected::rows_affected(&done);
                    sets.push(std::mem::take(&mut cur));
                }
                Either::Right(row) => {
                    if cur.columns.is_none() {
                        cur.columns = Some(row.columns().iter().map(reader::meta).collect());
                    }
                    if cur.rows.len() >= limit {
                        cur.truncated = true;
                        // Dropping the stream of a write (RETURNING) would roll it back, and
                        // inside a tab session it would cost the open transaction the next
                        // statement or probe, so only an autocommit read stops early; the rest
                        // runs to the end, dropping the extra rows, and keeps `limit` of them.
                        if read && !in_session {
                            cut_off = true;
                            break;
                        }
                        continue;
                    }
                    cur.rows.push(
                        (0..row.len())
                            .map(|i| cell(&row, i))
                            .collect(),
                    );
                }
            }
        }
    }
    if cut_off || cur.columns.is_some() || sets.is_empty() {
        sets.push(cur);
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    // No rows came back, so no row described the columns; ask the server instead.
    let described = if let [set] = sets.as_slice()
        && set.columns.is_none()
        && looks_like_read(execution_sql)
    {
        describe_columns(conn, execution_sql).await
    } else {
        None
    };
    let mut out = Vec::with_capacity(sets.len());
    for set in sets {
        let columns = set
            .columns
            .or_else(|| described.clone())
            .unwrap_or_default();
        out.push(StatementResult {
            sql: source_sql.to_string(),
            row_count: set.rows.len(),
            affected: columns.is_empty().then_some(set.changed),
            columns,
            rows: set.rows,
            truncated: set.truncated,
            limit,
            is_read: read,
            sortable: false,
            elapsed_ms,
            documents: None,
            cell_types: None,
            database: None,
        });
    }
    Ok((out, cut_off))
}

/// The MySQL/MariaDB transaction probe (see [`Conn::tx`] for the state it changes).
async fn mysql_probe(c: &mut MySqlConnection) -> Option<Tx> {
    if let Ok(v) = sqlx::query_scalar::<_, i64>("SELECT CAST(@@in_transaction AS SIGNED)")
        .fetch_one(&mut *c)
        .await
    {
        return Some(Tx {
            engine: SqlEngine::Mysql,
            open: v != 0,
            aborted: false,
            changed: None,
        });
    }
    match c.execute(sqlx::raw_sql("SET TRANSACTION READ WRITE")).await {
        Ok(_) => Some(Tx {
            engine: SqlEngine::Mysql,
            open: false,
            aborted: false,
            changed: None,
        }),
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("25001") => Some(Tx {
            engine: SqlEngine::Mysql,
            open: true,
            aborted: false,
            changed: None,
        }),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_detection_skips_comments_and_parens() {
        assert!(looks_like_read("SELECT 1"));
        assert!(looks_like_read("  -- hi\n/* x */ (select 1)"));
        assert!(looks_like_read("# mysql comment\nSHOW TABLES"));
        assert!(looks_like_read("with x as (select 1) select * from x"));
        assert!(looks_like_read("PRAGMA table_info(t)"));
        assert!(!looks_like_read("UPDATE t SET a = 1"));
        assert!(!looks_like_read("create table t (id int)"));
        assert!(!looks_like_read("-- only a comment"));
    }

    #[test]
    fn tx_dirty_rule_per_engine() {
        let tx = |engine, open, changed| Tx {
            engine,
            open,
            aborted: false,
            changed,
        };
        assert!(tx(SqlEngine::Pg, true, Some(true)).dirty(false));
        assert!(!tx(SqlEngine::Pg, false, Some(true)).dirty(false));
        assert!(tx(SqlEngine::Pg, true, None).dirty(true), "keeps the flag");
        assert!(tx(SqlEngine::Mysql, true, None).dirty(true));
        assert!(!tx(SqlEngine::Mysql, false, None).dirty(true), "implicit commit");
        assert!(tx(SqlEngine::Sqlite, true, Some(true)).dirty(false));
        assert!(!tx(SqlEngine::Sqlite, true, Some(false)).dirty(true));
        assert!(tx(SqlEngine::Sqlite, true, None).dirty(true));
    }

    #[test]
    fn sqlite_handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SqliteHandle>();
    }
}
