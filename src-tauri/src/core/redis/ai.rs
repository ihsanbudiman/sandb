//! Guarded Redis reads and writes that the user approved.

use std::time::Instant;

use redis::Value;
use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, RedisMode, read_only_refusal, target_database};
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{AiReadResult, ReadLimits, WriteResult, timeout_error};

use super::client::{self, Access};
use super::conn::mode;
use super::parse::Statement;
use super::{KILL_TIMEOUT, command, guard, reply, run};

pub(crate) enum ReadOrigin {
    Assistant,
    Schedule,
}

pub(crate) async fn read(
    access: &impl Access,
    conn: &Connection,
    database: Option<&str>,
    text: &str,
    limits: ReadLimits,
    origin: ReadOrigin,
) -> Result<AiReadResult, AppError> {
    let stmt = match origin {
        ReadOrigin::Assistant => guard::ai_read_statement(text),
        ReadOrigin::Schedule => guard::read_statement(text),
    };
    let stmt = stmt.map_err(|reason| {
        AppError::new(
            ErrorCode::GuardRejected,
            format!("Blocked by the read-only guard: {reason}"),
        )
    })?;
    read_reply(access, conn, database, &stmt, limits).await
}

#[cfg(feature = "it")]
pub async fn read_unchecked(
    access: &impl Access,
    conn: &Connection,
    database: Option<&str>,
    text: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let statements = super::parse::parse(text)?;
    let [stmt] = statements.as_slice() else {
        return Err(AppError::invalid("Exactly one Redis command is allowed"));
    };
    guard::check_supported(&statements)?;
    if command::classify(stmt) == command::Kind::Select {
        return Err(AppError::invalid("Choose the database in the request"));
    }
    read_reply(access, conn, database, stmt, limits).await
}

struct Dispatch<'a, A: Access> {
    access: &'a A,
    id: &'a str,
    db: &'a str,
    evict: bool,
}

impl<A: Access> Drop for Dispatch<'_, A> {
    fn drop(&mut self) {
        if self.evict {
            self.access.redis().evict(self.id, self.db);
        }
    }
}

async fn read_reply(
    access: &impl Access,
    conn: &Connection,
    database: Option<&str>,
    stmt: &Statement,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let db = run::database_index(
        &target_database(conn, database),
        mode(conn) == RedisMode::Cluster,
    )?;
    let limits = limits.sanitized();
    let started = Instant::now();
    let mut sent = false;
    let value = tokio::time::timeout(limits.timeout, async {
        let mut handle = client::connection(access, &conn.id, &db).await?;
        let mut dispatch = Dispatch {
            access,
            id: &conn.id,
            db: &db,
            evict: true,
        };
        sent = true;
        let result = run::query(&mut handle, stmt).await.map_err(AppError::from);
        dispatch.evict = result.as_ref().is_err_and(transport_error);
        result
    })
    .await;
    let value = match value {
        Ok(result) => result?,
        Err(_) => {
            if sent && command::is_script(stmt) {
                let _ =
                    tokio::time::timeout(KILL_TIMEOUT, run::stop_script(access, conn, stmt)).await;
            }
            return Err(timeout_error(limits.timeout));
        }
    };
    let mut shaped = reply::shape(stmt, value);
    let truncated = shaped.rows.len() > limits.max_rows;
    shaped.rows.truncate(limits.max_rows);
    Ok(AiReadResult {
        columns: shaped.columns,
        rows: shaped.rows,
        truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
        notice: shaped.notice,
        documents: None,
    })
}

pub async fn run_approved(
    access: &impl Access,
    conn: &Connection,
    database: &str,
    text: &str,
    stop: &CancellationToken,
) -> Result<WriteResult, AppError> {
    if !conn.may_write() {
        return Err(read_only_refusal(conn, None));
    }
    if !conn.ai_write {
        return Err(AppError::new(
            ErrorCode::WriteDenied,
            "AI write access is off for this connection",
        ));
    }
    let stmt = guard::write_statement(text)?;
    let db = run::database_index(
        &target_database(conn, Some(database)),
        mode(conn) == RedisMode::Cluster,
    )?;
    let started = Instant::now();
    let mut handle = tokio::select! {
        biased;
        () = stop.cancelled() => return Err(AppError::cancelled()),
        handle = client::connection(access, &conn.id, &db) => handle?,
    };
    let result = run::until_cancelled(stop, run::query(&mut handle, &stmt)).await?;
    let Some(result) = result else {
        access.redis().evict(&conn.id, &db);
        drop(handle);
        if command::is_script(&stmt) {
            let _ = tokio::time::timeout(KILL_TIMEOUT, run::stop_script(access, conn, &stmt)).await;
        }
        return Err(AppError::cancelled().with_detail("The command may have run on the server."));
    };
    let value = result.map_err(|err| {
        let mut err = AppError::from(err);
        if transport_error(&err) {
            access.redis().evict(&conn.id, &db);
            err.message = "Connection lost. The command may have run on the server.".into();
        }
        err
    })?;
    let affected = affected_count(&stmt, value);
    Ok(WriteResult {
        affected,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn affected_count(stmt: &Statement, value: Value) -> Option<u64> {
    let Value::Int(n) = value else { return None };
    match command::known_name(stmt)? {
        "DEL" | "UNLINK" | "HDEL" | "LREM" | "SADD" | "SREM" | "XDEL" | "ZREM"
        | "ZREMRANGEBYLEX" | "ZREMRANGEBYRANK" | "ZREMRANGEBYSCORE" => u64::try_from(n).ok(),
        _ => None,
    }
}

fn transport_error(err: &AppError) -> bool {
    matches!(err.code, ErrorCode::ConnectFailed | ErrorCode::Timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_change_counts_are_reported_as_affected() {
        for (text, reply, expected) in [
            ("INCR counter", 5001, None),
            ("RPUSH jobs item", 42, None),
            ("HSET user name value", 0, None),
            ("DEL first second", 2, Some(2)),
            ("srem tags absent", 0, Some(0)),
        ] {
            let stmt = guard::write_statement(text).unwrap();
            assert_eq!(affected_count(&stmt, Value::Int(reply)), expected, "{text}");
        }
    }
}
