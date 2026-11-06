//! Thin `#[tauri::command]` adapters over the core.

use tauri::ipc::{Channel, InvokeBody, Request};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::core::ai::oauth::{LogoutView, SessionView};
use crate::core::ai::session::{self as chat, ChatEvent, ChatRequest};
use crate::core::ai::settings::{self as ai_settings, AiSettings, AiSettingsView, AiTestResult};
use crate::core::connections::{self, Connection, ConnectionInput, Engine, TestResult};
use crate::core::db::dump::{self, TableExportRequest};
use crate::core::db::edit::{
    self, ApplyEditsRequest, ApplyEditsResult, CellValueRequest, EditableRequest, EditableResult,
};
use crate::core::db::export::{self, ExportRequest, ExportResult};
use crate::core::db::introspect::{self, ColumnInfo, SchemaSnapshot, TableInfo, TableProperties};
use crate::core::db::query::{self, QueryProgress, QueryRun, RunRequest};
use crate::core::db::readonly::{self, AiReadResult, ReadLimits};
use crate::core::db::session::{self, TabStatus};
use crate::core::error::AppError;
use crate::core::guard::{self, GuardCheck, WriteInfo};
use crate::core::state::AppState;

type Res<T> = Result<T, AppError>;

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> Res<Vec<Connection>> {
    Ok(connections::list(&state))
}

#[tauri::command]
pub async fn create_connection(
    state: State<'_, AppState>,
    input: ConnectionInput,
    password: Option<String>,
) -> Res<Connection> {
    connections::create(&state, input, password)
}

#[tauri::command]
pub async fn update_connection(
    state: State<'_, AppState>,
    id: String,
    input: ConnectionInput,
    password: Option<String>,
) -> Res<Connection> {
    connections::update(&state, &id, input, password).await
}

#[tauri::command]
pub async fn duplicate_connection(
    state: State<'_, AppState>,
    id: String,
    input: Option<ConnectionInput>,
    password: Option<String>,
) -> Res<Connection> {
    connections::duplicate(&state, &id, input, password)
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: String) -> Res<()> {
    connections::delete(&state, &id).await
}

#[tauri::command]
pub async fn test_connection(
    state: State<'_, AppState>,
    input: ConnectionInput,
    password: Option<String>,
    id: Option<String>,
) -> Res<TestResult> {
    connections::test(&state, input, password, id.as_deref()).await
}

#[tauri::command]
pub async fn list_databases(state: State<'_, AppState>, conn_id: String) -> Res<Vec<String>> {
    introspect::list_databases(&state, &conn_id).await
}

#[tauri::command]
pub async fn list_schemas(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
) -> Res<Vec<String>> {
    introspect::list_schemas(&state, &conn_id, &database).await
}

#[tauri::command]
pub async fn list_tables(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
    schema: Option<String>,
) -> Res<Vec<TableInfo>> {
    introspect::list_tables(&state, &conn_id, &database, schema.as_deref()).await
}

#[tauri::command]
pub async fn list_columns(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
    schema: Option<String>,
    table: String,
) -> Res<Vec<ColumnInfo>> {
    introspect::list_columns(&state, &conn_id, &database, schema.as_deref(), &table).await
}

/// Indexes, constraints, foreign keys and incoming foreign keys of one table.
#[tauri::command]
pub async fn table_properties(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
    schema: Option<String>,
    table: String,
) -> Res<TableProperties> {
    introspect::table_properties(&state, &conn_id, &database, schema.as_deref(), &table).await
}

/// CREATE statements of one table or view (with its indexes, and triggers on SQLite).
#[tauri::command]
pub async fn table_ddl(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
    schema: Option<String>,
    table: String,
) -> Res<String> {
    dump::table_ddl(&state, &conn_id, &database, schema.as_deref(), &table).await
}

#[tauri::command]
pub async fn schema_snapshot(
    state: State<'_, AppState>,
    conn_id: String,
    database: String,
) -> Res<SchemaSnapshot> {
    introspect::schema_snapshot(&state, &conn_id, &database).await
}

#[tauri::command]
pub async fn run_query(state: State<'_, AppState>, request: RunRequest) -> Res<QueryRun> {
    query::run(&state, request).await
}

#[tauri::command]
pub async fn editable_result(
    state: State<'_, AppState>,
    request: EditableRequest,
) -> Res<EditableResult> {
    edit::editable_result(&state, request).await
}

#[tauri::command]
pub async fn apply_edits(
    state: State<'_, AppState>,
    request: ApplyEditsRequest,
) -> Res<ApplyEditsResult> {
    edit::apply_edits(&state, request).await
}

#[tauri::command]
pub async fn cell_value(
    state: State<'_, AppState>,
    request: CellValueRequest,
) -> Res<Option<String>> {
    edit::cell_value(&state, request).await
}

#[tauri::command]
pub async fn cancel_query(state: State<'_, AppState>, query_id: String) -> Res<()> {
    query::cancel(&state, &query_id).await
}

/// Live statement progress of a running query (`done` of `total`); `null` once it ends.
#[tauri::command]
pub async fn query_progress(
    state: State<'_, AppState>,
    query_id: String,
) -> Res<Option<QueryProgress>> {
    Ok(query::progress(&state, &query_id))
}

/// Commits the tab's open transaction. The session closes; the next run opens a new one.
#[tauri::command]
pub async fn commit_transaction(state: State<'_, AppState>, tab_id: String) -> Res<()> {
    session::end_tx_for(&state, &tab_id, true).await
}

/// Rolls back the tab's open transaction. The session closes; the next run opens a new one.
#[tauri::command]
pub async fn rollback_transaction(state: State<'_, AppState>, tab_id: String) -> Res<()> {
    session::end_tx_for(&state, &tab_id, false).await
}

/// Whether the tab has a session and an open transaction with uncommitted changes.
#[tauri::command]
pub async fn tab_status(state: State<'_, AppState>, tab_id: String) -> Res<Option<TabStatus>> {
    session::status(&state, &tab_id).await
}

/// Drops the tab's session, rolling back anything uncommitted.
#[tauri::command]
pub async fn close_tab_session(state: State<'_, AppState>, tab_id: String) -> Res<()> {
    state.sessions.close(&tab_id).await;
    Ok(())
}

/// The user decided the quit prompt: roll back all tab sessions and exit. `app.exit` raises
/// `RunEvent::ExitRequested`, not the window close request, so it cannot loop.
#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    for id in state.sessions.pending_tab_ids() {
        state.sessions.close(&id).await;
    }
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn guard_check(engine: Engine, sql: String) -> Res<GuardCheck> {
    Ok(guard::guard_check(&sql, engine))
}

#[tauri::command]
pub async fn classify_write(engine: Engine, sql: String) -> Res<WriteInfo> {
    Ok(guard::classify_write(&sql, engine))
}

/// AI read path: the guard, then a read-only session with the default row and time limits.
#[tauri::command]
pub async fn run_ai_read(
    state: State<'_, AppState>,
    conn_id: String,
    database: Option<String>,
    sql: String,
) -> Res<AiReadResult> {
    readonly::run_ai_read(
        &state,
        &conn_id,
        database.as_deref(),
        &sql,
        ReadLimits::default(),
    )
    .await
}

#[tauri::command]
pub async fn chatgpt_sign_in(state: State<'_, AppState>, enable_usage: bool) -> Res<SessionView> {
    if enable_usage {
        state
            .chatgpt
            .enable_plan_usage(state.secrets.as_ref())
            .await
    } else {
        state.chatgpt.sign_in(state.secrets.as_ref()).await
    }
}

#[tauri::command]
pub async fn chatgpt_models(
    state: State<'_, AppState>,
) -> Res<Vec<crate::core::ai::responses::Model>> {
    ai_settings::chatgpt_catalog(&state).await
}

#[tauri::command]
pub async fn chatgpt_manage_usage() -> Res<()> {
    open::that("https://chatgpt.com/settings/usage").map_err(|_| {
        AppError::new(
            crate::core::error::ErrorCode::Io,
            "Could not open ChatGPT usage settings",
        )
    })
}

#[tauri::command]
pub async fn chatgpt_cancel_sign_in(state: State<'_, AppState>) -> Res<()> {
    state.chatgpt.cancel_sign_in();
    Ok(())
}

#[tauri::command]
pub async fn chatgpt_logout(state: State<'_, AppState>) -> Res<LogoutView> {
    state.chatgpt.logout(state.secrets.as_ref()).await
}

#[tauri::command]
pub async fn chatgpt_status(state: State<'_, AppState>) -> Res<SessionView> {
    state.chatgpt.status(state.secrets.as_ref())
}

#[tauri::command]
pub async fn get_ai_settings(state: State<'_, AppState>) -> Res<AiSettingsView> {
    ai_settings::get(&state)
}

#[tauri::command]
pub async fn save_ai_settings(
    state: State<'_, AppState>,
    settings: AiSettings,
) -> Res<AiSettingsView> {
    ai_settings::save(&state, settings)
}

/// `null` or an empty key deletes the stored key.
#[tauri::command]
pub async fn set_ai_key(state: State<'_, AppState>, key: Option<String>) -> Res<()> {
    ai_settings::set_key(&state, key)
}

/// Tests the given (maybe unsaved) settings. `key`: `null` uses the stored key, `""` sends none.
#[tauri::command]
pub async fn test_ai(
    state: State<'_, AppState>,
    settings: AiSettings,
    key: Option<String>,
) -> Res<AiTestResult> {
    ai_settings::test(&state, settings, key).await
}

/// Model ids of the given (maybe unsaved) settings. `key` as in `test_ai`.
#[tauri::command]
pub async fn list_ai_models(
    state: State<'_, AppState>,
    settings: AiSettings,
    key: Option<String>,
) -> Res<Vec<String>> {
    ai_settings::models(&state, settings, key).await
}

/// Streams the answer on `on_event`; it always ends with one `done` or `error` event.
#[tauri::command]
pub async fn chat_send(
    state: State<'_, AppState>,
    request: ChatRequest,
    on_event: Channel<ChatEvent>,
) -> Res<()> {
    chat::run(&state, request, |e| on_event.send(e).is_ok()).await;
    Ok(())
}

#[tauri::command]
pub async fn chat_cancel(state: State<'_, AppState>, chat_id: String) -> Res<()> {
    chat::cancel(&state, &chat_id);
    Ok(())
}

/// Asks for the file in a native save dialog here, so the webview never chooses a path.
#[tauri::command]
pub async fn export_csv(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ExportRequest,
) -> Res<ExportResult> {
    export::check(&state, &request)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name("result.csv")
        .add_filter("CSV", &["csv"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let picked = rx.await.ok().flatten();
    let Some(path) = picked else {
        return Ok(ExportResult {
            cancelled: true,
            rows: 0,
            path: String::new(),
        });
    };
    let path = path
        .into_path()
        .map_err(|e| AppError::invalid("Choose a file to export to").with_detail(e.to_string()))?;
    export::export_csv(&state, request, &path).await
}

/// One table as a `.sql` file: its DDL, plus INSERT statements when `request.data`. The file
/// comes from a native save dialog, as for `export_csv`.
#[tauri::command]
pub async fn export_table_sql(
    app: AppHandle,
    state: State<'_, AppState>,
    request: TableExportRequest,
) -> Res<ExportResult> {
    connections::get(&state, &request.conn_id)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(format!("{}.sql", request.table))
        .add_filter("SQL", &["sql"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let picked = rx.await.ok().flatten();
    let Some(path) = picked else {
        return Ok(ExportResult {
            cancelled: true,
            rows: 0,
            path: String::new(),
        });
    };
    let path = path
        .into_path()
        .map_err(|e| AppError::invalid("Choose a file to export to").with_detail(e.to_string()))?;
    dump::export_table_sql(&state, request, &path).await
}

/// Writes a PDF the UI built to a file from a native save dialog. The body is the raw PDF bytes;
/// the `x-file-name` header is the suggested name. Returns the path, or `None` when the user
/// closed the dialog.
#[tauri::command]
pub async fn save_pdf(app: AppHandle, request: Request<'_>) -> Res<Option<String>> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(AppError::invalid("The PDF must be sent as raw bytes"));
    };
    if !bytes.starts_with(b"%PDF-") {
        return Err(AppError::invalid("The data is not a PDF"));
    }
    let name = request
        .headers()
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .filter(|n| n.ends_with(".pdf") && !n.contains(['/', '\\']))
        .unwrap_or("report.pdf");
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(name)
        .add_filter("PDF", &["pdf"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|e| AppError::invalid("Choose a file to save to").with_detail(e.to_string()))?;
    tokio::fs::write(&path, bytes).await?;
    Ok(Some(path.display().to_string()))
}
