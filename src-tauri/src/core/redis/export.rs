//! Export the full reply of one read command with the same columns as the grid.

use std::borrow::Cow;
use std::path::Path;

use tokio::io::AsyncWriteExt;

use crate::core::cell;
use crate::core::connections::{Connection, RedisMode, target_database};
use crate::core::error::{AppError, ErrorCode};
use crate::core::export_file::ExportFile;
use crate::core::result::{ExportRequest, ExportResult};

use super::client::{self, Access};
use super::command::{self, Kind};
use super::conn::mode;
use super::parse::{self, Statement};
use super::{KILL_TIMEOUT, reply, run};

pub fn check(statements: &[Statement]) -> Result<(), AppError> {
    if matches!(statements, [stmt] if command::classify(stmt) == Kind::Read) {
        Ok(())
    } else {
        Err(AppError::invalid("Export needs exactly one read command"))
    }
}

pub async fn export_csv(
    access: &impl Access,
    conn: &Connection,
    req: ExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let statements = parse::parse(&req.sql)?;
    check(&statements)?;
    let stmt = &statements[0];
    let database = run::database_index(
        &target_database(conn, req.database.as_deref()),
        mode(conn) == RedisMode::Cluster,
    )?;
    let mut file =
        ExportFile::for_query(path, access.queries(), &conn.id, &database, conn.engine).await?;
    let stop = file
        .cancellation_token()
        .expect("for_query registers a run")
        .clone();
    let mut handle = tokio::select! {
        biased;
        () = stop.cancelled() => return Err(AppError::cancelled()),
        handle = client::connection(access, &conn.id, &database) => handle?,
    };
    let result = run::until_cancelled(&stop, run::query(&mut handle, stmt)).await?;
    let Some(result) = result else {
        access.redis().evict(&conn.id, &database);
        drop(handle);
        if command::is_script(stmt) {
            let _ = tokio::time::timeout(KILL_TIMEOUT, run::stop_script(access, conn, stmt)).await;
        }
        return Err(AppError::cancelled());
    };
    let value = result.map_err(|err| {
        let err = AppError::from(err);
        if matches!(err.code, ErrorCode::ConnectFailed | ErrorCode::Timeout) {
            access.redis().evict(&conn.id, &database);
        }
        err
    })?;
    if stop.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let shaped = reply::shape_full(stmt, value);
    tokio::select! {
        biased;
        () = stop.cancelled() => return Err(AppError::cancelled()),
        result = write_csv(&mut file, &shaped) => result?,
    }
    file.finish(shaped.rows.len() as u64).await
}

async fn write_csv(file: &mut ExportFile<'_>, shaped: &reply::Shaped) -> Result<(), AppError> {
    let out = file.out();
    out.write_all(cell::line(shaped.columns.iter().map(|c| c.name.as_str())).as_bytes())
        .await?;
    let mut fields = Vec::with_capacity(shaped.columns.len());
    for row in &shaped.rows {
        fields.clear();
        fields.extend(row.iter().map(|value| match value.as_str() {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(cell::field(value)),
        }));
        out.write_all(cell::line(fields.iter().map(|field| field.as_ref())).as_bytes())
            .await?;
    }
    Ok(())
}
