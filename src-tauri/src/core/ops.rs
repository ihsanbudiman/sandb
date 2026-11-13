//! The engine boundary. Every operation both engines implement is routed here once: the command
//! layer calls these functions, and only the SQL side (`core::db`) or the MongoDB side
//! (`core::mongo`) runs. A SQL module never sees a MongoDB connection, and no SQL or Mongo module
//! imports the other.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, read_only_refusal};
use crate::core::db::conn as sql_conn;
use crate::core::db::{dump, edit, export, introspect, query, readonly, write};
use crate::core::engine::{Engine, SqlEngine};
use crate::core::error::AppError;
use crate::core::guard;
use crate::core::mongo;
use crate::core::query_registry::Session;
use crate::core::result::{
    AiReadResult, ApplyEditsRequest, ApplyEditsResult, CellValueRequest, ColumnInfo,
    DocumentExportRequest, EditableRequest, EditableResult, ExportRequest, ExportResult,
    GuardCheck, QueryProgress, QueryRun, ReadLimits, RunRequest, SchemaSnapshot, SqlExportRequest,
    TableExportRequest, TableInfo, TableProperties, WriteInfo, WriteResult,
};
use crate::core::state::AppState;

/// Longest wait of a cancel for the MongoDB server kill.
const MONGO_KILL_TIMEOUT: Duration = Duration::from_secs(2);

/// The SQL engine of a connection; a MongoDB connection is a routing bug here (only SQL paths
/// call this).
fn sql_engine(conn: &Connection) -> Result<SqlEngine, AppError> {
    conn.engine.sql().ok_or_else(AppError::not_sql)
}

pub async fn list_databases(state: &AppState, conn_id: &str) -> Result<Vec<String>, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(_) => introspect::list_databases(&state.db(), &conn).await,
        None => {
            mongo::introspect::databases(
                &mongo::client::client(state, conn_id).await?,
                &conn.database,
            )
            .await
        }
    }
}

pub async fn list_schemas(
    state: &AppState,
    conn_id: &str,
    database: &str,
) -> Result<Vec<String>, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(engine) => introspect::list_schemas(&state.db(), &conn, engine, database).await,
        // MongoDB has no schema level.
        None => Ok(vec![]),
    }
}

pub async fn list_tables(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
) -> Result<Vec<TableInfo>, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(_) => introspect::list_tables(&state.db(), &conn, database, schema).await,
        None => {
            mongo::introspect::tables(&mongo::client::client(state, conn_id).await?, database).await
        }
    }
}

pub async fn list_columns(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<Vec<ColumnInfo>, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(_) => introspect::list_columns(&state.db(), &conn, database, schema, table).await,
        None => {
            // An empty collection yields an empty list; only an unknown name is `not_found`.
            mongo::introspect::columns(
                &mongo::client::client(state, conn_id).await?,
                database,
                table,
            )
            .await
        }
    }
}

pub async fn schema_snapshot(
    state: &AppState,
    conn_id: &str,
    database: &str,
) -> Result<SchemaSnapshot, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(engine) => introspect::schema_snapshot(&state.db(), &conn, engine, database).await,
        None => {
            mongo::introspect::snapshot(&mongo::client::client(state, conn_id).await?, database)
                .await
        }
    }
}

pub async fn table_properties(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<TableProperties, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(_) => introspect::table_properties(&state.db(), &conn, database, schema, table).await,
        None => {
            mongo::introspect::properties(
                &mongo::client::client(state, conn_id).await?,
                database,
                table,
            )
            .await
        }
    }
}

pub async fn table_ddl(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<String, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(_) => dump::table_ddl(&state.db(), &conn, database, schema, table).await,
        None => {
            mongo::dump::collection_ddl(
                &mongo::client::client(state, conn_id).await?,
                database,
                table,
            )
            .await
        }
    }
}

pub async fn run_query(state: &AppState, req: RunRequest) -> Result<QueryRun, AppError> {
    let conn = state.connection(&req.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => query::run(&state.db(), &conn, engine, req).await,
        None => mongo::run::run(state, &conn, req, true).await,
    }
}

/// The editor path without the read-only pre-check, so integration tests can prove that the
/// database itself refuses a write that got past it. Never compiled into a normal build.
#[cfg(feature = "it")]
pub async fn run_editor_unchecked(state: &AppState, req: RunRequest) -> Result<QueryRun, AppError> {
    let conn = state.connection(&req.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => query::run_unchecked(&state.db(), &conn, engine, req).await,
        None => mongo::run::run(state, &conn, req, false).await,
    }
}

pub async fn editable_result(
    state: &AppState,
    request: EditableRequest,
) -> Result<EditableResult, AppError> {
    let conn = state.connection(&request.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => edit::editable_result(&state.db(), &conn, engine, request).await,
        None => mongo::edit::editable_result(state, &conn, request).await,
    }
}

pub async fn apply_edits(
    state: &AppState,
    request: ApplyEditsRequest,
) -> Result<ApplyEditsResult, AppError> {
    let conn = state.connection(&request.conn_id)?;
    if !conn.may_write() {
        return Err(read_only_refusal(&conn, None));
    }
    match conn.engine.sql() {
        Some(engine) => edit::apply_edits(&state.db(), &conn, engine, request).await,
        None => mongo::edit::apply_edits(state, &conn, request).await,
    }
}

pub async fn cell_value(
    state: &AppState,
    request: CellValueRequest,
) -> Result<Option<String>, AppError> {
    let conn = state.connection(&request.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => edit::cell_value(&state.db(), &conn, engine, request).await,
        None => mongo::edit::cell_value(state, &conn, request).await,
    }
}

/// Stops a running query. Unknown or finished ids are not an error: the query may have just
/// ended, or not registered yet, in which case it starts cancelled. Repeats the kill a few times
/// while the run is still registered, which covers a cancel that lands between two statements or
/// before the first one starts.
pub async fn cancel_query(state: &AppState, query_id: &str) -> Result<(), AppError> {
    for attempt in 0..4 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let Some(target) = state.queries().cancel_target(query_id) else {
            if attempt == 0 {
                state.queries().remember_early_cancel(query_id);
            }
            return Ok(());
        };
        let sent = match target.engine.sql() {
            None => {
                // The run already stopped on the flag; the server kill is best effort, and a
                // server that does not answer must not hold the cancel for the driver's monitor
                // timeout.
                Some(
                    tokio::time::timeout(
                        MONGO_KILL_TIMEOUT,
                        mongo::run::kill(state, &target.conn_id, query_id),
                    )
                    .await
                    .unwrap_or(Ok(())),
                )
            }
            Some(_) => match target.session {
                Some(session) => {
                    // SQLite's interrupt is in-process; the SQL engines need the pool their
                    // session belongs to.
                    let pool = async {
                        match session {
                            Session::Sqlite(_) => Ok(None),
                            _ => {
                                let conn = state.connection(&target.conn_id)?;
                                state.pool_for(&conn, Some(&target.database)).map(Some)
                            }
                        }
                    }
                    .await;
                    Some(match pool {
                        Ok(pool) => sql_conn::kill(pool.as_ref(), session).await,
                        Err(e) => Err(e),
                    })
                }
                // Not attached yet: the next attempt finds the session.
                None => None,
            },
        };
        // A kill that races with the end of the query fails harmlessly (e.g. MySQL "unknown thread").
        if let Some(Err(e)) = sent
            && attempt == 0
            && state.queries().is_running(query_id)
        {
            return Err(e);
        }
    }
    Ok(())
}

/// Statement progress of a running query; `None` for an unknown, finished, or not yet
/// registered id (the entry is removed with the run).
pub fn query_progress(state: &AppState, query_id: &str) -> Option<QueryProgress> {
    state.queries().progress(query_id)
}

/// AI read path: the guard, then a read-only session with `limits`.
pub async fn run_ai_read(
    state: &AppState,
    conn_id: &str,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(engine) => {
            readonly::run_ai_read(&state.db(), &conn, engine, database, sql, limits).await
        }
        None => mongo::ai::read(state, &conn, database, sql, limits).await,
    }
}

/// The read-only layers only, so integration tests can prove that the database itself refuses
/// a write that got past layer 1. Never compiled into a normal build.
#[cfg(feature = "it")]
pub async fn run_read_only_unchecked(
    state: &AppState,
    conn_id: &str,
    database: Option<&str>,
    sql: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let conn = state.connection(conn_id)?;
    let engine = sql_engine(&conn)?;
    readonly::run_read_only_unchecked(&state.db(), &conn, engine, database, sql, limits).await
}

/// Runs an approved AI write: one transaction on the selected database, except that on MySQL a
/// statement with an implicit commit ends it early (the approval preview warns about that).
pub async fn run_approved_write(
    state: &AppState,
    conn_id: &str,
    database: &str,
    sql: &str,
    stop: &CancellationToken,
) -> Result<WriteResult, AppError> {
    let conn = state.connection(conn_id)?;
    match conn.engine.sql() {
        Some(engine) => write::run_approved(&state.db(), &conn, engine, database, sql, stop).await,
        None => mongo::ai::run_approved(state, &conn, database, sql, stop).await,
    }
}

pub fn guard_check(sql: &str, engine: Engine) -> GuardCheck {
    match engine.sql() {
        Some(sql_engine) => guard::classify::guard_check(sql, sql_engine),
        None => mongo::guard::guard_check(sql),
    }
}

pub fn classify_write(sql: &str, engine: Engine, database: Option<&str>) -> WriteInfo {
    match engine.sql() {
        Some(sql_engine) => guard::classify::classify_write(sql, sql_engine),
        None => mongo::guard::classify_write(sql, database),
    }
}

/// One write the assistant proposes, checked and classified for the preview. A mongosh statement
/// must parse as exactly one write before the user sees it; SQL text is classified as it is (an
/// unparsable statement is a warning there).
pub fn write_preview(text: &str, engine: Engine, database: &str) -> Result<WriteInfo, AppError> {
    match engine.sql() {
        Some(sql_engine) => Ok(guard::classify::classify_write(text, sql_engine)),
        None => mongo::ai::propose(text, database),
    }
}

/// Rejects anything but one plain read, before the save dialog opens.
pub fn check_export(state: &AppState, req: &ExportRequest) -> Result<(), AppError> {
    let conn = state.connection(&req.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => export::check(engine, &req.sql),
        None => mongo::export::check(&mongo::parse::parse(&req.sql)?),
    }
}

/// Extended JSON is a MongoDB feature.
fn require_mongo(conn: &Connection) -> Result<(), AppError> {
    match conn.engine {
        Engine::Mongo => Ok(()),
        _ => Err(AppError::invalid(
            "Extended JSON export needs a MongoDB connection",
        )),
    }
}

/// `check_export`, plus a MongoDB connection.
pub fn check_documents(state: &AppState, req: &DocumentExportRequest) -> Result<(), AppError> {
    let conn = state.connection(&req.query.conn_id)?;
    require_mongo(&conn)?;
    mongo::export::check(&mongo::parse::parse(&req.query.sql)?)
}

/// `check_export`, plus a table name and options that make sense for the connection's engine.
pub fn check_sql(state: &AppState, req: &SqlExportRequest) -> Result<(), AppError> {
    let conn = state.connection(&req.query.conn_id)?;
    let Some(engine) = conn.engine.sql() else {
        return Err(AppError::invalid(
            "INSERT statement export needs a SQL connection",
        ));
    };
    export::check_sql(engine, req)
}

pub async fn export_csv(
    state: &AppState,
    req: ExportRequest,
    path: &std::path::Path,
) -> Result<ExportResult, AppError> {
    let conn = state.connection(&req.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => export::export_csv(&state.db(), &conn, engine, req, path).await,
        None => mongo::export::export_csv(state, &conn, req, path).await,
    }
}

pub async fn export_documents(
    state: &AppState,
    req: DocumentExportRequest,
    path: &std::path::Path,
) -> Result<ExportResult, AppError> {
    let conn = state.connection(&req.query.conn_id)?;
    require_mongo(&conn)?;
    mongo::export::export_documents(state, &conn, req, path).await
}

pub async fn export_sql(
    state: &AppState,
    req: SqlExportRequest,
    path: &std::path::Path,
) -> Result<ExportResult, AppError> {
    let conn = state.connection(&req.query.conn_id)?;
    let engine = sql_engine(&conn)?;
    export::export_sql(&state.db(), &conn, engine, req, path).await
}

pub async fn export_table_sql(
    state: &AppState,
    req: TableExportRequest,
    path: &std::path::Path,
) -> Result<ExportResult, AppError> {
    let conn = state.connection(&req.conn_id)?;
    match conn.engine.sql() {
        Some(engine) => dump::export_table_sql(&state.db(), &conn, engine, req, path).await,
        None => mongo::dump::export_collection(state, &conn, req, path).await,
    }
}
