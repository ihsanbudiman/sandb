//! Runs an AI write that the user approved in the chat. The statements run in one transaction
//! that commits at once, also on Production — except on MySQL, where a statement with an
//! implicit commit (DDL, LOCK TABLES, SET autocommit, CALL, GRANT, …) commits the ones before it
//! as it runs; the approval preview warns about that. The approval is the user's decision, so
//! the write does not join a tab's manual-commit transaction. The connection leaves the pool and
//! is closed afterwards, so session state that the SQL sets (SET, USE, temp tables) never
//! reaches the pool.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, read_only_refusal};
use crate::core::db::conn::Conn;
use crate::core::db::ctx::Ctx;
use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::WriteResult;
use crate::core::sql::facts::{self, StatementKind};
use crate::core::sql::split;

/// Checks the saved connection again, then runs `sql` in one transaction on `database` (see the
/// module note for MySQL's implicit commits). `stop` stops the statements before COMMIT; on MySQL
/// a statement that already committed implicitly stays committed, and the result says so. COMMIT
/// itself does not stop, so the result is always known.
pub async fn run_approved(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
    sql: &str,
    stop: &CancellationToken,
) -> Result<WriteResult, AppError> {
    if !conn.may_write() {
        return Err(read_only_refusal(conn, None));
    }
    let stmts = split::split(engine, sql);
    if stmts.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    if stmts.iter().any(|s| controls_transaction(engine, s)) {
        return Err(AppError::invalid(
            "The write must not contain BEGIN, COMMIT, ROLLBACK or SAVEPOINT: it runs in its \
             own transaction",
        ));
    }
    let started = Instant::now();
    let pool = ctx.pool_for(conn, Some(database))?;
    let implicit: Vec<bool> = stmts.iter().map(|s| commits_implicitly(engine, s)).collect();
    let affected = in_transaction(Conn::detached(&pool).await?, &stmts, &implicit, stop).await?;
    Ok(WriteResult {
        affected,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// BEGIN, COMMIT, ROLLBACK, SAVEPOINT and the like would end the write's own transaction early.
/// SQL that does not parse passes: the preview warned that its effect is unknown, and the user
/// approved it.
fn controls_transaction(engine: SqlEngine, sql: &str) -> bool {
    facts::analyze(engine, sql).is_ok_and(|f| {
        f.statements
            .iter()
            .any(|s| s.kind == StatementKind::Transaction)
    })
}

/// Whether `sql` may commit the run's transaction as it runs (MySQL only, see
/// `sql::facts::StatementFacts::implicit_commit`). SQL that does not parse may be one of the
/// statements MySQL commits at that the parser cannot read (`REPAIR TABLE`, `LOAD DATA`,
/// `INSTALL PLUGIN`), so it counts: the result never claims a rollback it cannot make.
fn commits_implicitly(engine: SqlEngine, sql: &str) -> bool {
    if engine != SqlEngine::Mysql {
        return false;
    }
    match facts::analyze(engine, sql) {
        Ok(f) => f.statements.iter().any(|s| s.implicit_commit),
        Err(_) => true,
    }
}

async fn in_transaction(
    mut c: Conn,
    stmts: &[String],
    implicit: &[bool],
    stop: &CancellationToken,
) -> Result<u64, AppError> {
    let statements = async {
        c.begin().await?;
        let mut n = 0;
        for (i, s) in stmts.iter().enumerate() {
            n += c.execute_all(s).await.map_err(|mut e| {
                // MySQL commits before an implicit-commit statement runs, so the rollback below
                // cannot undo the statements before this one — including when this one failed.
                // With no statement before it nothing can have been committed yet.
                if i > 0 && implicit[..=i].iter().any(|&b| b) {
                    e.message.push_str(
                        " MySQL may have committed a statement before this one; check the database.",
                    );
                }
                e
            })?;
        }
        Ok::<_, AppError>(n)
    };
    let r = tokio::select! {
        biased;
        _ = stop.cancelled() => Err(AppError::new(
            ErrorCode::Cancelled,
            if implicit.iter().any(|&b| b) {
                "Stopped. MySQL may have committed a statement before this one; check the database."
            } else {
                "Stopped before the write committed. Nothing changed."
            },
        )),
        r = statements => r,
    };
    let r = match r {
        Ok(n) => c.end_tx_unbounded(true).await.map(|()| n),
        Err(e) => Err(e),
    };
    // Closing a connection without COMMIT rolls its transaction back on the server.
    c.close().await;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{ConnectionInput, Env, SslMode};
    use crate::core::db::pool::DbPool;
    use crate::core::engine::Engine;
    use crate::core::secrets::MemoryStore;
    use crate::core::state::AppState;
    use sqlx::{Connection as _, Executor as _};

    /// The tests run on one SQLite connection, so this wrapper routes for them.
    async fn run_approved(
        state: &AppState,
        conn_id: &str,
        database: &str,
        sql: &str,
        stop: &CancellationToken,
    ) -> Result<WriteResult, AppError> {
        let conn = state.connection(conn_id).unwrap();
        super::run_approved(&state.db(), &conn, SqlEngine::Sqlite, database, sql, stop).await
    }

    async fn setup(read_only: bool) -> (tempfile::TempDir, AppState, String) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        let file = dir.path().join("w.db");
        let mut c = sqlx::sqlite::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        (&mut c)
            .execute(sqlx::raw_sql(
                "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1), (2), (3)",
            ))
            .await
            .unwrap();
        c.close().await.unwrap();
        let conn = st.create_connection(
            ConnectionInput {
                name: "w".into(),
                engine: Engine::Sqlite,
                env: Env::Production,
                host: String::new(),
                port: 0,
                database: "main".into(),
                user: String::new(),
                ssl: SslMode::Disable,
                file: file.to_string_lossy().into_owned(),
                ai_write: false,
                read_only,
                version: None,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            None,
        )
        .unwrap();
        (dir, st, conn.id)
    }

    async fn ids(st: &AppState, conn: &str) -> Vec<i64> {
        let state = st;
        let DbPool::Sqlite(p) = state.pool_for(&state.connection(conn).unwrap(), Some("main")).unwrap() else {
            unreachable!()
        };
        sqlx::query_scalar("SELECT id FROM t ORDER BY id")
            .fetch_all(&p)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn all_statements_commit_together() {
        let (_d, st, conn) = setup(false).await;
        let stop = CancellationToken::new();
        let sql = "DELETE FROM t WHERE id = 1; INSERT INTO t VALUES (4), (5)";
        let r = run_approved(&st, &conn, "main", sql, &stop).await.unwrap();
        assert_eq!(r.affected, 3);
        assert_eq!(ids(&st, &conn).await, [2, 3, 4, 5]);
    }

    /// A failing statement undoes the ones before it: the user approved the whole SQL, not part
    /// of it.
    #[tokio::test]
    async fn a_failing_statement_rolls_back_the_whole_write() {
        let (_d, st, conn) = setup(false).await;
        let stop = CancellationToken::new();
        let sql = "DELETE FROM t WHERE id = 1; INSERT INTO t VALUES (2)";
        let e = run_approved(&st, &conn, "main", sql, &stop)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Database, "{e:?}");
        assert_eq!(ids(&st, &conn).await, [1, 2, 3]);
    }

    #[tokio::test]
    async fn sql_that_ends_the_transaction_is_refused() {
        let (_d, st, conn) = setup(false).await;
        let stop = CancellationToken::new();
        let sql = "DELETE FROM t WHERE id = 1; COMMIT; INSERT INTO t VALUES (2)";
        let e = run_approved(&st, &conn, "main", sql, &stop)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert_eq!(ids(&st, &conn).await, [1, 2, 3]);
    }

    /// The connection became read-only after the preview: the approval cannot override that.
    #[tokio::test]
    async fn a_read_only_connection_refuses_an_approved_write() {
        let (_d, st, conn) = setup(true).await;
        let stop = CancellationToken::new();
        let e = run_approved(&st, &conn, "main", "DELETE FROM t", &stop)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectionReadOnly);
        assert_eq!(ids(&st, &conn).await, [1, 2, 3]);
    }

    /// The stop and failure results are about the whole batch: a statement MySQL may commit at,
    /// or one the parser cannot read at all, removes the "nothing changed" promise.
    #[test]
    fn implicit_commits_remove_the_rollback_promise() {
        assert!(commits_implicitly(
            SqlEngine::Mysql,
            "CREATE TABLE t (id int)"
        ));
        assert!(commits_implicitly(SqlEngine::Mysql, "REPAIR TABLE t"));
        assert!(!commits_implicitly(SqlEngine::Mysql, "SET @x = 1"));
        assert!(!commits_implicitly(
            SqlEngine::Mysql,
            "INSERT INTO t VALUES (1)"
        ));
        // DDL is transactional outside MySQL, and `REPAIR TABLE` is not even MySQL there.
        assert!(!commits_implicitly(
            SqlEngine::Pg,
            "CREATE TABLE t (id int)"
        ));
        assert!(!commits_implicitly(SqlEngine::Sqlite, "REPAIR TABLE t"));
    }
}
