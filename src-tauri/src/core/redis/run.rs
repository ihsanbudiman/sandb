//! Run redis-cli statements in order. A SELECT changes the target, not a shared session.

use std::cmp::Ordering;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use redis::cluster_routing::{Route, RoutingInfo, SingleNodeRoutingInfo, SlotAddr};
use redis::{Cmd, Value};
use serde_json::Value as Cell;
use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, RedisMode, target_database};
use crate::core::engine::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{QueryRun, RunRequest, RunSort, StatementResult, limit};
use crate::core::secrets::password_account;

use super::KILL_TIMEOUT;
use super::client::{self, Access};
use super::command::{self, Kind};
use super::conn::{RedisConn, mode, open, parse_database};
use super::parse::{self, Statement};
use super::{guard, reply};

pub async fn run(
    access: &impl Access,
    conn: &Connection,
    req: RunRequest,
    check: bool,
) -> Result<QueryRun, AppError> {
    let statements = parse::parse(&req.sql)?;
    if statements.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    if check && !conn.may_write() {
        guard::check_read_only(conn, &statements)?;
    } else {
        guard::check_supported(&statements)?;
    }

    let sortable = statements.len() == 1 && command::classify(&statements[0]) == Kind::Read;
    if req.sort.is_some() && !sortable {
        return Err(AppError::invalid("Sort needs exactly one read command"));
    }
    let cluster = mode(conn) == RedisMode::Cluster;
    let mut db = database_index(&target_database(conn, req.database.as_deref()), cluster)?;
    let limit = limit(req.limit, || None);
    let started = Instant::now();
    let run = access.queries().register(
        &req.query_id,
        &conn.id,
        &db,
        Engine::Redis,
        statements.len(),
    )?;
    let mut results = Vec::with_capacity(statements.len());
    let mut notices = Vec::new();
    for stmt in statements {
        if run.cancelled.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let start = Instant::now();
        let kind = command::classify(&stmt);
        if kind == Kind::Select {
            db = database_index(command::select_database(&stmt)?, cluster)?;
            notices.push(format!("Using database {db} for the rest of this run"));
            run.done.fetch_add(1, Relaxed);
            continue;
        }

        let mut conn_handle = tokio::select! {
            biased;
            () = run.cancelled.cancelled() => return Err(AppError::cancelled()),
            conn = client::connection(access, &conn.id, &db) => conn?,
        };
        let outcome = until_cancelled(&run.cancelled, query(&mut conn_handle, &stmt)).await?;
        let Some(outcome) = outcome else {
            access.redis().evict(&conn.id, &db);
            drop(conn_handle);
            if command::is_script(&stmt) {
                let _ = tokio::time::timeout(KILL_TIMEOUT, stop_script(access, conn, &stmt)).await;
            }
            return Err(if kind == Kind::Read {
                AppError::cancelled()
            } else {
                AppError::cancelled().with_detail("The command may have run on the server.")
            });
        };
        let value = outcome.map_err(|err| {
            let mut err = AppError::from(err);
            if matches!(err.code, ErrorCode::ConnectFailed | ErrorCode::Timeout) {
                access.redis().evict(&conn.id, &db);
                if kind != Kind::Read {
                    err.message = "Connection lost. The command may have run on the server.".into();
                }
            }
            err
        })?;

        let is_read = kind == Kind::Read;
        let affected = match (&value, is_read) {
            (Value::Int(n), false) => u64::try_from(*n).ok(),
            _ => None,
        };
        let mut shaped = reply::shape(&stmt, value);
        if let Some(sort) = req.sort.as_ref() {
            sort_rows(&mut shaped.rows, &shaped.columns, sort)?;
        }
        let truncated = shaped.rows.len() > limit;
        shaped.rows.truncate(limit);
        if let Some(notice) = shaped.notice {
            notices.push(notice);
        }
        results.push(StatementResult {
            sql: stmt.text,
            columns: shaped.columns,
            row_count: shaped.rows.len(),
            rows: shaped.rows,
            truncated,
            limit,
            affected,
            is_read,
            sortable: is_read,
            elapsed_ms: start.elapsed().as_millis() as u64,
            documents: None,
            cell_types: None,
            database: Some(db.clone()),
        });
        run.done.fetch_add(1, Relaxed);
    }
    Ok(QueryRun {
        statements: results,
        notices,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

pub(super) fn database_index(input: &str, cluster: bool) -> Result<String, AppError> {
    let index = parse_database(input)?;
    if cluster && index != 0 {
        return Err(AppError::invalid("Redis Cluster has only database 0"));
    }
    Ok(index.to_string())
}

// A cancellation before the first poll must not evict the connection or send a server-wide KILL.
pub(super) async fn until_cancelled<T>(
    token: &CancellationToken,
    request: impl Future<Output = T>,
) -> Result<Option<T>, AppError> {
    tokio::select! {
        biased;
        reply = async {
            if token.is_cancelled() {
                return Err(AppError::cancelled());
            }
            Ok(Some(request.await))
        } => reply,
        () = token.cancelled() => Ok(None),
    }
}

pub(super) async fn query(conn: &mut RedisConn, stmt: &Statement) -> redis::RedisResult<Value> {
    let mut cmd = Cmd::new();
    for arg in &stmt.args {
        cmd.arg(arg.as_slice());
    }
    if let RedisConn::Cluster(cluster) = conn
        && let Some(key) = stmt.args.get(1)
        && [
            b"MGET".as_slice(),
            b"MSET",
            b"DEL",
            b"EXISTS",
            b"UNLINK",
            b"TOUCH",
            b"JSON.MGET",
            b"JSON.MSET",
        ]
        .iter()
        .any(|name| stmt.args[0].eq_ignore_ascii_case(name))
    {
        // A command entered once must run once. The driver's default routing splits these
        // commands by slot, which would hide CROSSSLOT and can make writes partial.
        let route = Route::with_key(key, SlotAddr::Master);
        let value = cluster
            .route_command(
                cmd,
                RoutingInfo::SingleNode(SingleNodeRoutingInfo::SpecificNode(route)),
            )
            .await?;
        return match value {
            Value::ServerError(err) => Err(err.into()),
            value => Ok(value),
        };
    }
    conn.query(&cmd).await
}

pub(super) async fn stop_script(
    access: &impl Access,
    conn: &Connection,
    stmt: &Statement,
) -> Result<(), AppError> {
    // The run's settings might have changed while it waited. Do not send a new password to the
    // old server, or send KILL to the new server.
    let current = access.connection(&conn.id)?;
    if current != *conn {
        return Ok(());
    }
    let password = access.secrets().get(&password_account(&conn.id))?;
    // Kill is server-wide. A nonzero SELECT during setup fails while the server is busy.
    let mut fresh = open(conn, password.as_deref(), 0).await?;
    let name = &stmt.args[0];
    let command = if name.eq_ignore_ascii_case(b"FCALL") || name.eq_ignore_ascii_case(b"FCALL_RO") {
        "FUNCTION"
    } else {
        "SCRIPT"
    };
    fresh.query(redis::cmd(command).arg("KILL")).await?;
    Ok(())
}

fn sort_rows(
    rows: &mut [Vec<Cell>],
    columns: &[crate::core::result::ColumnMeta],
    sort: &RunSort,
) -> Result<(), AppError> {
    let index = sort.column_index;
    if columns.get(index).is_none_or(|col| col.name != sort.column) {
        return Err(AppError::invalid("Sort column does not exist"));
    }
    // One ordering per column avoids a non-transitive comparison of numbers mixed with text.
    let numeric = rows
        .iter()
        .all(|row| row[index].is_null() || as_number(&row[index]).is_some());
    rows.sort_by(|a, b| {
        let (a, b) = (&a[index], &b[index]);
        match (a.is_null(), b.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let order = if numeric {
                    as_number(a).unwrap().total_cmp(&as_number(b).unwrap())
                } else {
                    text(a).cmp(&text(b))
                };
                if sort.ascending {
                    order
                } else {
                    order.reverse()
                }
            }
        }
    });
    Ok(())
}

fn as_number(cell: &Cell) -> Option<f64> {
    cell.as_f64().or_else(|| cell.as_str()?.parse().ok())
}

fn text(cell: &Cell) -> std::borrow::Cow<'_, str> {
    match cell {
        Cell::String(s) => s.as_str().into(),
        Cell::Bool(true) => "true".into(),
        Cell::Bool(false) => "false".into(),
        _ => crate::core::cell::field(cell).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::result::ColumnMeta;
    use serde_json::json;

    #[tokio::test]
    async fn cancelled_request_never_polls_the_redis_command() {
        let token = CancellationToken::new();
        token.cancel();
        let mut sent = false;
        let err = until_cancelled(&token, async { sent = true })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert!(!sent);
    }

    #[test]
    fn sorts_numeric_text_and_keeps_nulls_last() {
        let columns = [ColumnMeta {
            name: "value".into(),
            data_type: "string".into(),
            mixed: false,
        }];
        let mut sort = RunSort {
            column: "value".into(),
            column_index: 0,
            ascending: true,
        };
        let mut rows = vec![vec![json!("10")], vec![Cell::Null], vec![json!("2")]];
        sort_rows(&mut rows, &columns, &sort).unwrap();
        assert_eq!(
            rows,
            [vec![json!("2")], vec![json!("10")], vec![Cell::Null]]
        );

        // Comparing some pairs as numbers and some as text can produce an ordering cycle.
        rows.push(vec![json!("1a")]);
        sort.ascending = false;
        sort_rows(&mut rows, &columns, &sort).unwrap();
        assert_eq!(
            rows,
            [
                vec![json!("2")],
                vec![json!("1a")],
                vec![json!("10")],
                vec![Cell::Null]
            ]
        );
    }
}
