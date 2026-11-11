//! Runs an AI write that the user approved in the chat. All statements run in one transaction
//! that commits at once, also on Production: the approval is the user's decision, so the write
//! does not join a tab's manual-commit transaction. The connection leaves the pool and is closed
//! afterwards, so session state that the SQL sets (SET, USE, temp tables) never reaches the pool.

use std::time::Instant;

use sqlx::mysql::MySql;
use sqlx::postgres::Postgres;
use sqlx::sqlite::Sqlite;
use sqlx::{Connection as _, Database, Executor};
use tokio_util::sync::CancellationToken;

use crate::core::conn_ro;
use crate::core::connections::{self, Engine};
use crate::core::db::pool::DbPool;
use crate::core::db::query::CLOSE_TIMEOUT;
use crate::core::error::{AppError, ErrorCode};
use crate::core::sql::facts::{self, StatementKind};
use crate::core::sql::split;
use crate::core::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteResult {
    /// Rows that all statements changed together.
    pub affected: u64,
    pub elapsed_ms: u64,
}

/// Checks the saved connection again, then runs `sql` in one transaction on `database`.
/// `stop` stops the statements before COMMIT; nothing changes then. COMMIT itself does not
/// stop, so the result is always known.
pub async fn run_approved(
    state: &AppState,
    conn_id: &str,
    database: &str,
    sql: &str,
    stop: &CancellationToken,
) -> Result<WriteResult, AppError> {
    let conn = connections::get(state, conn_id)?;
    if !conn_ro::writes_allowed(&conn) {
        return Err(AppError::new(
            ErrorCode::ConnectionReadOnly,
            format!("Connection \"{}\" is read-only", conn.name),
        ));
    }
    if conn.engine == Engine::Mongo {
        return crate::core::mongo::ai::run_approved(state, &conn, database, sql, stop).await;
    }
    let stmts = split::split(conn.engine, sql);
    if stmts.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    if stmts.iter().any(|s| controls_transaction(conn.engine, s)) {
        return Err(AppError::invalid(
            "The write must not contain BEGIN, COMMIT, ROLLBACK or SAVEPOINT: it runs in its \
             own transaction",
        ));
    }
    let started = Instant::now();
    let affected = match connections::pool_for(state, &conn.id, Some(database))? {
        DbPool::Pg(p) => {
            let c = p.acquire().await?.detach();
            in_transaction::<Postgres>(c, &stmts, stop, |r| r.rows_affected()).await
        }
        DbPool::MySql(p) => {
            let c = p.acquire().await?.detach();
            in_transaction::<MySql>(c, &stmts, stop, |r| r.rows_affected()).await
        }
        DbPool::Sqlite(p) => {
            let c = p.acquire().await?.detach();
            in_transaction::<Sqlite>(c, &stmts, stop, |r| r.rows_affected()).await
        }
    }?;
    Ok(WriteResult {
        affected,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// BEGIN, COMMIT, ROLLBACK, SAVEPOINT and the like would end the write's own transaction early.
/// SQL that does not parse passes: the preview warned that its effect is unknown, and the user
/// approved it.
fn controls_transaction(engine: Engine, sql: &str) -> bool {
    facts::analyze(engine, sql).is_ok_and(|f| {
        f.statements
            .iter()
            .any(|s| s.kind == StatementKind::Transaction)
    })
}

async fn in_transaction<DB>(
    mut c: DB::Connection,
    stmts: &[String],
    stop: &CancellationToken,
    affected: fn(&DB::QueryResult) -> u64,
) -> Result<u64, AppError>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    let statements = async {
        (&mut c).execute(sqlx::raw_sql("BEGIN")).await?;
        let mut n = 0;
        for s in stmts {
            n += affected(&(&mut c).execute(sqlx::raw_sql(s)).await?);
        }
        Ok::<_, sqlx::Error>(n)
    };
    let r = tokio::select! {
        biased;
        _ = stop.cancelled() => Err(AppError::new(
            ErrorCode::Cancelled,
            "Stopped before the write committed. Nothing changed.",
        )),
        r = statements => r.map_err(AppError::from),
    };
    let r = match r {
        Ok(n) => (&mut c)
            .execute(sqlx::raw_sql("COMMIT"))
            .await
            .map(|_| n)
            .map_err(AppError::from),
        Err(e) => Err(e),
    };
    // Closing a connection without COMMIT rolls its transaction back on the server.
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, c.close()).await;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{ConnectionInput, Env, SslMode};
    use crate::core::secrets::MemoryStore;

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
        let conn = connections::create(
            &st,
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
        let DbPool::Sqlite(p) = connections::pool_for(st, conn, Some("main")).unwrap() else {
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
}
