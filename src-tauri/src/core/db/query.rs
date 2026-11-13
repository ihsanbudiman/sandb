//! Runs editor SQL: one connection per run, statements in order, rows capped, cancellable.
//! Runs are auto-commit: a connection goes back to the pool only after a lone plain read, so
//! session state (open transaction, SET, USE, temp tables) never reaches the next run.

use std::time::Instant;

use sqlparser::ast::{OrderBy, OrderByExpr, OrderByKind, OrderByOptions, OrderBySort, Statement};
use sqlparser::parser::Parser;

use crate::core::conn_ro;
use crate::core::connections::{Connection, Env, target_database};
use crate::core::db::conn::{CLOSE_TIMEOUT, Conn};
use crate::core::db::ctx::Ctx;
use crate::core::db::session;
use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{QueryRun, RunRequest, RunSort, StatementResult, limit};
use crate::core::sql::{facts, split};

/// Added to a run that ended with a transaction still open; closing its connection rolled it back.
pub const OPEN_TX_NOTICE: &str =
    "Open transaction was rolled back. Runs are auto-commit; put BEGIN…COMMIT in one run.";

/// A lone plain read that parses as one query: the only shape [`sorted_statements`] accepts.
fn is_sortable(engine: SqlEngine, stmts: &[String], reads: &[bool]) -> bool {
    let ([sql], [true]) = (stmts, reads) else {
        return false;
    };
    matches!(
        Parser::parse_sql(&*split::dialect(engine), sql).as_deref(),
        Ok([Statement::Query(_)])
    )
}

/// Flags each result whose statement is a lone plain read, so the grid offers sorting only where
/// it works. A multi-statement run returns one result per statement on the common path; when a
/// statement returns more than one set the counts do not line up and nothing is marked.
fn mark_sortable(
    engine: SqlEngine,
    stmts: &[String],
    reads: &[bool],
    results: &mut [StatementResult],
) {
    if stmts.len() != reads.len() || stmts.len() != results.len() {
        return;
    }
    for (i, r) in results.iter_mut().enumerate() {
        if reads[i] && is_sortable(engine, &stmts[i..=i], &reads[i..=i]) {
            r.sortable = true;
        }
    }
}

/// Builds the execution text separately from the editor text: sorting changes only what is sent
/// to the server, never the SQL retained in `StatementResult` for source reruns and editing.
fn sorted_statements(
    engine: SqlEngine,
    stmts: &[String],
    reads: &[bool],
    sort: Option<&RunSort>,
) -> Result<Vec<String>, AppError> {
    let Some(sort) = sort else {
        return Ok(stmts.to_vec());
    };
    if stmts.len() != 1 || reads != [true] {
        return Err(AppError::invalid(
            "Sorting requires exactly one plain read query",
        ));
    }
    if sort.column.is_empty() {
        return Err(AppError::invalid("Sort column cannot be empty"));
    }
    let dialect = split::dialect(engine);
    let mut parsed = Parser::parse_sql(&*dialect, &stmts[0]).map_err(|e| {
        AppError::invalid("Cannot sort a query that does not parse").with_detail(e.to_string())
    })?;
    let [Statement::Query(query)] = parsed.as_mut_slice() else {
        return Err(AppError::invalid("Sorting requires a SELECT query"));
    };
    if let sqlparser::ast::SetExpr::Select(select) = &*query.body {
        let explicit_arity = select
            .projection
            .iter()
            .try_fold(0usize, |n, item| match item {
                sqlparser::ast::SelectItem::UnnamedExpr(_)
                | sqlparser::ast::SelectItem::ExprWithAlias { .. } => n.checked_add(1),
                _ => None,
            });
        if let Some(n) = explicit_arity
            && sort.column_index >= n
        {
            return Err(AppError::invalid(format!(
                "Sort column index {} is outside the query's {n} output columns",
                sort.column_index
            )));
        }
    }
    let ordinal = sort
        .column_index
        .checked_add(1)
        .ok_or_else(|| AppError::invalid("Sort column index is too large"))?;
    let expr = OrderByExpr {
        expr: sqlparser::ast::Expr::Value(
            sqlparser::ast::Value::Number(ordinal.to_string(), false).into(),
        ),
        options: OrderByOptions {
            sort: Some(if sort.ascending {
                OrderBySort::Asc
            } else {
                OrderBySort::Desc
            }),
            nulls_first: None,
        },
        with_fill: None,
    };
    query.order_by = Some(OrderBy {
        kind: OrderByKind::Expressions(vec![expr]),
        interpolate: None,
    });
    Ok(vec![query.to_string()])
}

/// Runs `req.sql` statement by statement on one connection, so `SET` and temp tables persist
/// within the run (not after it). No restrictions on the SQL: this is the user's own editor.
/// On a writable production connection with `tab_id`, the run executes inside the tab's
/// dedicated session and open transaction instead (manual commit).
pub async fn run(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: RunRequest,
) -> Result<QueryRun, AppError> {
    let stmts = split::split(engine, &req.sql);
    if stmts.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    // Read-only connection: the session barrier is the real wall; this check gives the clear
    // error and refuses statements that could undo the barrier itself.
    if !conn.may_write() {
        conn_ro::check_read_only(conn, engine, &stmts)?;
    }
    execute(ctx, conn, engine, req, stmts).await
}

/// Same editor path without the read-only pre-check, so integration tests can prove that the
/// database itself refuses a write that got past it. Never compiled into a normal build.
#[cfg(feature = "it")]
pub async fn run_unchecked(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: RunRequest,
) -> Result<QueryRun, AppError> {
    let stmts = split::split(engine, &req.sql);
    if stmts.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    execute(ctx, conn, engine, req, stmts).await
}

async fn execute(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    req: RunRequest,
    stmts: Vec<String>,
) -> Result<QueryRun, AppError> {
    let database = target_database(conn, req.database.as_deref());
    let reads: Vec<bool> = stmts
        .iter()
        .map(|s| facts::is_plain_read_sql(engine, s))
        .collect();
    let execution_stmts = sorted_statements(engine, &stmts, &reads, req.sort.as_ref())?;
    // Read-only connections never enter a tab session: there is nothing to commit, and the
    // frontend hides Commit / Rollback for them. Plain reads on a tab without a session need
    // none either: they take the pool like any autocommit read.
    if conn.env == Env::Production
        && conn.may_write()
        && let Some(tab_id) = req.tab_id.as_deref()
        && (ctx.sessions.has(tab_id) || reads.contains(&false))
    {
        return run_in_session(
            ctx,
            conn,
            engine,
            &database,
            tab_id,
            &req,
            &stmts,
            &execution_stmts,
            &reads,
        )
        .await;
    }
    let lone_read = reads == [true];
    let limit = limit(req.limit, || facts::row_limit(engine, stmts.last()?));
    let started = Instant::now();
    let run = ctx
        .queries
        .register(&req.query_id, &conn.id, &database, conn.engine, stmts.len())?;
    let cancelled = run.cancelled.clone();
    let done = run.done.clone();
    if cancelled.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let pool = ctx.pool_for(conn, Some(&database))?;
    let mut c = Conn::acquire(&pool).await?;
    let attached = c.attach(ctx.queries, Some(&req.query_id)).await?;
    let r = c
        .run_all(&stmts, &execution_stmts, &reads, limit, &cancelled, &done)
        .await;
    drop(attached);
    let (reuse, ask) = after_run(lone_read, &r, &cancelled);
    // SQLite is always asked: a read can leave it inside a transaction the driver does not see.
    let in_tx = if ask || engine == SqlEngine::Sqlite {
        c.tx(CLOSE_TIMEOUT).await.map(|t| t.open)
    } else {
        None
    };
    let reuse = reuse && (engine != SqlEngine::Sqlite || in_tx == Some(false));
    c.settle(reuse).await;
    let (mut statements, _) = r?;
    mark_sortable(engine, &stmts, &reads, &mut statements);
    drop(run);
    // The text heuristic only stands in when the server could not be asked.
    let open_tx = in_tx.unwrap_or_else(|| split::leaves_transaction_open(engine, &req.sql));
    Ok(QueryRun {
        statements,
        notices: if open_tx {
            vec![OPEN_TX_NOTICE.to_string()]
        } else {
            Vec::new()
        },
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// (return the connection to the pool, ask the server whether a transaction is open).
/// A lone plain read cannot open a transaction; after a cut-off read the server is still
/// sending rows, so it is not asked either and the text heuristic stands in.
fn after_run(
    lone_read: bool,
    r: &Result<(Vec<StatementResult>, bool), AppError>,
    cancelled: &tokio_util::sync::CancellationToken,
) -> (bool, bool) {
    let finished = r.as_ref().is_ok_and(|(_, cut)| !cut) && !cancelled.is_cancelled();
    (lone_read && finished, !lone_read && finished)
}

/// Errors whose connection cannot stay in the session: it is dead or unusable.
fn is_lost(e: &AppError) -> bool {
    matches!(
        e.code,
        ErrorCode::ConnectFailed | ErrorCode::Timeout | ErrorCode::Internal
    ) || {
        let d = e.detail.as_deref().unwrap_or("");
        d.contains("connection closed") || d.contains("broken pipe") || d.contains("pool is closed")
    }
}

/// Production manual-commit path: run `stmts` inside the tab session's explicit transaction.
/// The session outlives the run while that transaction is open, for Commit / Rollback and the
/// next run.
///
/// Plain reads run as they are; every other statement runs inside the transaction, which is
/// opened before it when none is open. Statements that may end the transaction (a typed
/// COMMIT, MySQL's implicit commits) are followed by a probe, so a statement after them opens
/// a new one instead of autocommitting.
#[allow(clippy::too_many_arguments)]
async fn run_in_session(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
    tab_id: &str,
    req: &RunRequest,
    stmts: &[String],
    execution_stmts: &[String],
    reads: &[bool],
) -> Result<QueryRun, AppError> {
    let kinds: Vec<Option<facts::StatementKind>> = stmts
        .iter()
        .map(|s| {
            facts::analyze(engine, s)
                .ok()
                .and_then(|f| f.statements.first().map(|s| s.kind))
        })
        .collect();
    let mysql = engine == SqlEngine::Mysql;
    let limit = limit(req.limit, || facts::row_limit(engine, stmts.last()?));
    let started = Instant::now();
    let run = ctx
        .queries
        .register(&req.query_id, &conn.id, database, conn.engine, stmts.len())?;
    let cancelled = run.cancelled.clone();
    let done = run.done.clone();
    if cancelled.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let mut s = session::for_tab(ctx, conn, database, tab_id).await?;
    let mut notices = Vec::new();
    let r = async {
        // Dropped before the probes below, so a cancel after the last statement sets the flag
        // but leaves no kill queued for the user's next run in this tab.
        let _attached = s.conn.attach(ctx.queries, Some(&req.query_id)).await?;
        let n = stmts.len();
        let mut results = Vec::with_capacity(n);
        for (i, sql) in stmts.iter().enumerate() {
            if cancelled.is_cancelled() {
                return Err(AppError::cancelled());
            }
            let kind = kinds[i];
            let probe = !reads[i] && session::may_end_tx(kind);
            // MySQL: the earlier uncommitted work an implicit commit would take along.
            let mut swept = false;
            if !reads[i] {
                session::begin_if_needed(&mut s).await?;
                if mysql && probe && !matches!(kind, Some(facts::StatementKind::Transaction)) {
                    swept = session::pending(&mut s).await;
                }
                if session::may_write(kind) {
                    s.dirty = true;
                }
            }
            match s
                .conn
                .run_one(sql, &execution_stmts[i], reads[i], limit, true)
                .await
            {
                Ok((r, _)) => {
                    results.extend(r);
                    done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                Err(_) if cancelled.is_cancelled() => return Err(AppError::cancelled()),
                Err(e) => {
                    let mut e = AppError::from(e);
                    if n > 1 {
                        e.message = format!("Statement {} of {n}: {}", i + 1, e.message);
                    }
                    return Err(e);
                }
            }
            if probe {
                session::refresh(&mut s).await;
                if mysql && !s.tx_open && !matches!(kind, Some(facts::StatementKind::Transaction)) {
                    notices.push(session::implicit_commit_notice(kind, swept, i, n));
                }
            }
        }
        Ok(results)
    }
    .await;
    let r = match r {
        Ok(results) => s.conn.cancel_result(&cancelled).map(|()| results),
        Err(e) => Err(e),
    };
    session::refresh(&mut s).await;
    let r = match r {
        Ok(mut statements) => Ok(QueryRun {
            statements: {
                mark_sortable(engine, stmts, reads, &mut statements);
                statements
            },
            notices,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }),
        Err(mut e) => {
            // The transaction aborts with the failed statement: the user must see it, and the
            // session stays open so Rollback is still available.
            if s.aborted {
                e.message = format!("{} Transaction aborted, roll back to continue.", e.message);
            }
            // An implicit commit before the failure already happened; the error must say so.
            if !notices.is_empty() {
                e.message = format!("{} ({})", e.message, notices.join("; "));
            }
            Err(e)
        }
    };
    session::finish(ctx, tab_id, s, matches!(&r, Err(e) if is_lost(e))).await;
    drop(run);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::result::MAX_LIMIT;

    #[test]
    fn limit_defaults_and_bounds() {
        assert_eq!(limit(None, || None), 200);
        assert_eq!(limit(None, || Some(50)), 200);
        assert_eq!(limit(None, || Some(1000)), 1000);
        assert_eq!(limit(None, || Some(u64::MAX)), MAX_LIMIT);
        assert_eq!(limit(Some(400), || Some(1000)), 400);
        assert_eq!(limit(Some(0), || None), 1);
        assert_eq!(limit(Some(20_000), || None), 20_000);
        assert_eq!(limit(Some(usize::MAX), || None), MAX_LIMIT);
    }

    #[test]
    fn sort_replaces_order_by_before_limit_and_preserves_inputs() {
        let source = vec!["SELECT id, name FROM users ORDER BY id DESC LIMIT 25".into()];
        let sorted = sorted_statements(
            SqlEngine::Pg,
            &source,
            &[true],
            Some(&RunSort {
                column: "displayName".into(),
                column_index: 1,
                ascending: true,
            }),
        )
        .unwrap();
        assert_eq!(
            source[0],
            "SELECT id, name FROM users ORDER BY id DESC LIMIT 25"
        );
        assert_eq!(
            sorted[0],
            "SELECT id, name FROM users ORDER BY 2 ASC LIMIT 25"
        );
    }

    #[test]
    fn sort_uses_ordinals_for_expressions_and_checks_explicit_arity() {
        let source = vec!["SELECT lower(name) AS name, upper(name) AS name FROM users".into()];
        let sorted = sorted_statements(
            SqlEngine::Pg,
            &source,
            &[true],
            Some(&RunSort {
                column: "name".into(),
                column_index: 1,
                ascending: false,
            }),
        )
        .unwrap();
        assert_eq!(
            sorted[0],
            "SELECT lower(name) AS name, upper(name) AS name FROM users ORDER BY 2 DESC"
        );
        assert!(
            sorted_statements(
                SqlEngine::Pg,
                &source,
                &[true],
                Some(&RunSort {
                    column: "name".into(),
                    column_index: 2,
                    ascending: true,
                }),
            )
            .is_err()
        );
    }

    #[test]
    fn sort_rejects_writes_multiple_statements_and_bad_columns() {
        let sort = RunSort {
            column: "id".into(),
            column_index: 0,
            ascending: false,
        };
        assert!(
            sorted_statements(
                SqlEngine::Sqlite,
                &["DELETE FROM t".into()],
                &[false],
                Some(&sort)
            )
            .is_err()
        );
        assert!(
            sorted_statements(
                SqlEngine::Sqlite,
                &["SELECT 1".into(), "SELECT 2".into()],
                &[true, true],
                Some(&sort),
            )
            .is_err()
        );
        assert!(
            sorted_statements(
                SqlEngine::Sqlite,
                &["SELECT 1".into()],
                &[true],
                Some(&RunSort {
                    column: String::new(),
                    column_index: 0,
                    ascending: true
                }),
            )
            .is_err()
        );
    }

    /// EXPLAIN and SHOW are plain reads (they page and export) but have no ORDER BY to add;
    /// offering a header sort on them only produced "Sorting requires a SELECT query".
    #[test]
    fn only_lone_queries_are_sortable() {
        let read =
            |e, sql: &str| is_sortable(e, &[sql.into()], &[facts::is_plain_read_sql(e, sql)]);
        assert!(read(SqlEngine::Pg, "SELECT * FROM t"));
        assert!(read(SqlEngine::Pg, "WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(!read(SqlEngine::Pg, "EXPLAIN SELECT * FROM t"));
        assert!(!read(SqlEngine::Pg, "SHOW search_path"));
        assert!(!read(SqlEngine::Mysql, "EXPLAIN SELECT * FROM t"));
        assert!(!read(SqlEngine::Pg, "DELETE FROM t"));
        assert!(!is_sortable(
            SqlEngine::Pg,
            &["SELECT 1".into(), "SELECT 2".into()],
            &[true, true]
        ));
    }

    #[test]
    fn each_result_of_a_multi_statement_run_is_sortable() {
        let result = |sql: &str, is_read: bool| StatementResult {
            sql: sql.into(),
            row_count: 0,
            affected: None,
            columns: vec![],
            rows: vec![],
            truncated: false,
            limit: 200,
            is_read,
            sortable: false,
            elapsed_ms: 0,
            documents: None,
            cell_types: None,
            database: None,
        };
        let mut results = vec![result("SELECT 1", true), result("SELECT 2", true)];
        mark_sortable(
            SqlEngine::Pg,
            &["SELECT 1".into(), "SELECT 2".into()],
            &[true, true],
            &mut results,
        );
        assert!(results.iter().all(|r| r.sortable));

        let mut mixed = vec![result("SELECT 1", true), result("DELETE FROM t", false)];
        mark_sortable(
            SqlEngine::Pg,
            &["SELECT 1".into(), "DELETE FROM t".into()],
            &[true, false],
            &mut mixed,
        );
        assert!(mixed[0].sortable);
        assert!(!mixed[1].sortable);
    }

    #[test]
    fn run_request_deserializes_optional_sort() {
        let request: RunRequest = serde_json::from_value(serde_json::json!({
            "connId": "c",
            "sql": "SELECT * FROM t",
            "queryId": "q",
            "sort": {"column": "name", "columnIndex": 0, "ascending": false}
        }))
        .unwrap();
        let sort = request.sort.unwrap();
        assert_eq!(sort.column, "name");
        assert_eq!(sort.column_index, 0);
        assert!(!sort.ascending);
    }

}
