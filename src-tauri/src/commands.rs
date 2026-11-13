//! The command table: one declaration per command, and the two servers over it.
//!
//! A `sync`/`async` entry produces the `#[tauri::command]` function, its argument struct (the
//! wire shape both servers use) and its `dispatch` arm, so the desktop app and the dev bridge
//! cannot drift. `native` entries are the ones only the desktop app can run (save dialogs, quit,
//! the unsaved-edits flag, the chat channel); `dispatch` answers them with `unsupported`, and the
//! bridge serves `chat_send` on its own SSE route.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;
use tauri::ipc::{Channel, Invoke, InvokeBody, Request};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::core::ai;
use crate::core::ai::event::ChatEvent;
use crate::core::ai::session::{self as chat, ChatRequest};
use crate::core::ai::settings::{self as ai_settings, AiSettings, AiSettingsView, AiTestResult};
use crate::core::connections::{Connection, ConnectionInput, SslMode, TestResult};
use crate::core::db::session::{self, TabStatus};
use crate::core::engine::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::export_file::ExportFile;
use crate::core::mongo::conn::parse_uri;
use crate::core::ops;
use crate::core::result::{
    AiReadResult, ApplyEditsRequest, ApplyEditsResult, CellValueRequest, ColumnInfo,
    DocumentExportRequest, DocumentFormat, EditableRequest, EditableResult, ExportRequest,
    ExportResult, GuardCheck, QueryProgress, QueryRun, RunRequest, SchemaSnapshot,
    SqlExportRequest, TableExportRequest, TableInfo, TableProperties, WriteInfo,
};
use crate::core::state::AppState;

type Res<T> = Result<T, AppError>;

/// Declares each command once. `sync` and `async` are the two execution modes Tauri offers:
/// sync commands run on the main thread, so only commands that do no I/O belong there. The
/// `native` list names commands whose `#[tauri::command]` function is written by hand because
/// its signature is not `(state, fields)`; it is still part of this table for the handler list
/// and the bridge's answer.
macro_rules! command_table {
    (
        sync {
            $(
                $(#[$sync_doc:meta])*
                $sync_name:ident $SyncArgs:ident { $( $sync_field:ident: $sync_ty:ty ),* $(,)? }
                    -> $sync_ret:ty => $sync_call:expr
            );* $(;)?
        }
        async {
            $(
                $(#[$async_doc:meta])*
                $async_name:ident $AsyncArgs:ident { $( $async_field:ident: $async_ty:ty ),* $(,)? }
                    -> $async_ret:ty => $async_call:expr
            );* $(;)?
        }
        native {
            $( $(#[$native_doc:meta])* $native_name:ident ),* $(,)?
        }
    ) => {
        /// Every command name, for the table tests and the generated `CommandName` union.
        #[cfg(test)]
        pub fn command_names() -> Vec<&'static str> {
            vec![
                $( stringify!($sync_name), )*
                $( stringify!($async_name), )*
                $( stringify!($native_name), )*
            ]
        }

        #[cfg(test)]
        pub const DESKTOP_ONLY: &[&str] = &[ $( stringify!($native_name) ),* ];

        /// The TypeScript union of the command names, emitted into the wire contract.
        #[cfg(test)]
        pub fn command_name_declaration() -> String {
            let names: Vec<String> = command_names()
                .iter()
                .map(|name| format!("  | \"{name}\""))
                .collect();
            format!("export type CommandName =\n{};\n", names.join("\n"))
        }

        /// The TypeScript declaration of every generated argument struct, for the wire test.
        #[cfg(test)]
        pub fn arg_declarations(cfg: &ts_rs::Config) -> Vec<(&'static str, String)> {
            let mut out = vec![
                $( (stringify!($SyncArgs), <$SyncArgs as ts_rs::TS>::decl(cfg)) ),*
            ];
            out.extend([
                $( (stringify!($AsyncArgs), <$AsyncArgs as ts_rs::TS>::decl(cfg)) ),*
            ]);
            out
        }

        $(
            #[cfg_attr(test, derive(ts_rs::TS))]
            #[derive(Debug, serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            pub struct $SyncArgs {
                $( pub $sync_field: $sync_ty, )*
            }

            $(#[$sync_doc])*
            #[tauri::command]
            pub fn $sync_name(state: State<'_, AppState>, $($sync_field: $sync_ty),*) -> Res<$sync_ret> {
                ($sync_call)(&*state, $SyncArgs { $($sync_field),* })
            }
        )*

        $(
            #[cfg_attr(test, derive(ts_rs::TS))]
            #[derive(Debug, serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            pub struct $AsyncArgs {
                $( pub $async_field: $async_ty, )*
            }

            $(#[$async_doc])*
            #[tauri::command]
            pub async fn $async_name(
                state: State<'_, AppState>,
                $($async_field: $async_ty),*
            ) -> Res<$async_ret> {
                ($async_call)(&*state, $AsyncArgs { $($async_field),* }).await
            }
        )*

        /// The Tauri `invoke_handler` list, built from the same names as `dispatch` below.
        pub fn handler() -> impl Fn(Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
            tauri::generate_handler![
                $( $crate::commands::$sync_name, )*
                $( $crate::commands::$async_name, )*
                $( $crate::commands::$native_name, )*
            ]
        }

        /// One command by name, with its JSON arguments (the bridge's route body).
        pub async fn dispatch(state: &AppState, name: &str, body: Value) -> Res<Value> {
            match name {
                $(
                    stringify!($sync_name) => {
                        let args: $SyncArgs = from_json(body)?;
                        let out: Res<$sync_ret> = ($sync_call)(state, args);
                        to_json(out?)
                    }
                )*
                $(
                    stringify!($async_name) => {
                        let args: $AsyncArgs = from_json(body)?;
                        let out: Res<$async_ret> = ($async_call)(state, args).await;
                        to_json(out?)
                    }
                )*
                $(
                    stringify!($native_name) => Err(desktop_only(stringify!($native_name))),
                )*
                _ => Err(AppError::not_found(format!("Unknown command {name}"))),
            }
        }
    };
}

command_table! {
    sync {
        /// The saved connections, in their display order.
        list_connections ListConnectionsArgs {} -> Vec<Connection>
            => |st: &AppState, _| Ok(st.list_connections());

        /// Live statement progress of a running query (`done` of `total`); `null` once it ends.
        query_progress QueryProgressArgs { query_id: String } -> Option<QueryProgress>
            => |st: &AppState, a: QueryProgressArgs| Ok(ops::query_progress(st, &a.query_id));

        guard_check GuardCheckArgs { engine: Engine, sql: String } -> GuardCheck
            => |_, a: GuardCheckArgs| Ok(ops::guard_check(&a.sql, a.engine));

        classify_write ClassifyWriteArgs { engine: Engine, sql: String, database: Option<String> } -> WriteInfo
            => |_, a: ClassifyWriteArgs| Ok(ops::classify_write(&a.sql, a.engine, a.database.as_deref()));

        chat_cancel ChatCancelArgs { chat_id: String } -> ()
            => |st: &AppState, a: ChatCancelArgs| {
                chat::cancel(st, &a.chat_id);
                Ok(())
            };

        approve_write ApproveWriteArgs { chat_id: String, id: String } -> ()
            => |st: &AppState, a: ApproveWriteArgs| chat::approve(st, &a.chat_id, &a.id);

        deny_write DenyWriteArgs { chat_id: String, id: String } -> ()
            => |st: &AppState, a: DenyWriteArgs| chat::deny(st, &a.chat_id, &a.id);

        /// Splits a MongoDB connection string as the backend saves it; `null` when it is not one.
        parse_mongo_uri ParseMongoUriArgs { uri: String } -> Option<MongoUriParts>
            => |_, a: ParseMongoUriArgs| Ok(mongo_uri_parts(&a.uri));
    }

    async {
        create_connection CreateConnectionArgs { input: ConnectionInput, password: Option<String> } -> Connection
            => async |st: &AppState, a: CreateConnectionArgs| { st.create_connection(a.input, a.password) };

        update_connection UpdateConnectionArgs { id: String, input: ConnectionInput, password: Option<String> } -> Connection
            => async |st: &AppState, a: UpdateConnectionArgs| { st.update_connection(&a.id, a.input, a.password).await };

        duplicate_connection DuplicateConnectionArgs { id: String, input: Option<ConnectionInput>, password: Option<String> } -> Connection
            => async |st: &AppState, a: DuplicateConnectionArgs| { st.duplicate_connection(&a.id, a.input, a.password) };

        delete_connection DeleteConnectionArgs { id: String } -> ()
            => async |st: &AppState, a: DeleteConnectionArgs| { st.delete_connection(&a.id).await };

        reorder_connections ReorderConnectionsArgs { ids: Vec<String> } -> Vec<Connection>
            => async |st: &AppState, a: ReorderConnectionsArgs| { st.reorder_connections(&a.ids) };

        test_connection TestConnectionArgs { input: ConnectionInput, password: Option<String>, id: Option<String> } -> TestResult
            => async |st: &AppState, a: TestConnectionArgs| { st.test_connection(a.input, a.password, a.id.as_deref()).await };

        list_databases ListDatabasesArgs { conn_id: String } -> Vec<String>
            => async |st: &AppState, a: ListDatabasesArgs| { ops::list_databases(st, &a.conn_id).await };

        list_schemas ListSchemasArgs { conn_id: String, database: String } -> Vec<String>
            => async |st: &AppState, a: ListSchemasArgs| { ops::list_schemas(st, &a.conn_id, &a.database).await };

        list_tables ListTablesArgs { conn_id: String, database: String, schema: Option<String> } -> Vec<TableInfo>
            => async |st: &AppState, a: ListTablesArgs| { ops::list_tables(st, &a.conn_id, &a.database, a.schema.as_deref()).await };

        list_columns ListColumnsArgs { conn_id: String, database: String, schema: Option<String>, table: String } -> Vec<ColumnInfo>
            => async |st: &AppState, a: ListColumnsArgs| {
                ops::list_columns(st, &a.conn_id, &a.database, a.schema.as_deref(), &a.table).await
            };

        /// Indexes, constraints, foreign keys and incoming foreign keys of one table.
        table_properties TablePropertiesArgs { conn_id: String, database: String, schema: Option<String>, table: String } -> TableProperties
            => async |st: &AppState, a: TablePropertiesArgs| {
                ops::table_properties(st, &a.conn_id, &a.database, a.schema.as_deref(), &a.table).await
            };

        /// CREATE statements of one table or view (with its indexes, and triggers on SQLite).
        table_ddl TableDdlArgs { conn_id: String, database: String, schema: Option<String>, table: String } -> String
            => async |st: &AppState, a: TableDdlArgs| {
                ops::table_ddl(st, &a.conn_id, &a.database, a.schema.as_deref(), &a.table).await
            };

        schema_snapshot SchemaSnapshotArgs { conn_id: String, database: String } -> SchemaSnapshot
            => async |st: &AppState, a: SchemaSnapshotArgs| { ops::schema_snapshot(st, &a.conn_id, &a.database).await };

        run_query RunQueryArgs { request: RunRequest } -> QueryRun
            => async |st: &AppState, a: RunQueryArgs| { ops::run_query(st, a.request).await };

        editable_result EditableResultArgs { request: EditableRequest } -> EditableResult
            => async |st: &AppState, a: EditableResultArgs| { ops::editable_result(st, a.request).await };

        apply_edits ApplyEditsArgs { request: ApplyEditsRequest } -> ApplyEditsResult
            => async |st: &AppState, a: ApplyEditsArgs| { ops::apply_edits(st, a.request).await };

        cell_value CellValueArgs { request: CellValueRequest } -> Option<String>
            => async |st: &AppState, a: CellValueArgs| { ops::cell_value(st, a.request).await };

        cancel_query CancelQueryArgs { query_id: String } -> ()
            => async |st: &AppState, a: CancelQueryArgs| { ops::cancel_query(st, &a.query_id).await };

        /// Commits the tab's open transaction. The session closes; the next run opens a new one.
        commit_transaction CommitTransactionArgs { tab_id: String } -> ()
            => async |st: &AppState, a: CommitTransactionArgs| { session::end_tx_for(st, &a.tab_id, true).await };

        /// Rolls back the tab's open transaction. The session closes; the next run opens a new one.
        rollback_transaction RollbackTransactionArgs { tab_id: String } -> ()
            => async |st: &AppState, a: RollbackTransactionArgs| { session::end_tx_for(st, &a.tab_id, false).await };

        /// Whether the tab has a session and an open transaction with uncommitted changes.
        tab_status TabStatusArgs { tab_id: String } -> Option<TabStatus>
            => async |st: &AppState, a: TabStatusArgs| { session::status(st, &a.tab_id).await };

        /// Drops the tab's session, rolling back anything uncommitted.
        close_tab_session CloseTabSessionArgs { tab_id: String } -> ()
            => async |st: &AppState, a: CloseTabSessionArgs| {
                session::close_tab(st, &a.tab_id).await;
                Ok(())
            };

        run_ai_read RunAiReadArgs { conn_id: String, database: Option<String>, sql: String } -> AiReadResult
            => async |st: &AppState, a: RunAiReadArgs| {
                let limits = ai::read_limits(st)?;
                ops::run_ai_read(st, &a.conn_id, a.database.as_deref(), &a.sql, limits).await
            };

        get_ai_settings GetAiSettingsArgs {} -> AiSettingsView
            => async |st: &AppState, _| { ai_settings::get(st) };

        save_ai_settings SaveAiSettingsArgs { settings: AiSettings } -> AiSettingsView
            => async |st: &AppState, a: SaveAiSettingsArgs| { ai_settings::save(st, a.settings) };

        set_ai_key SetAiKeyArgs { key: Option<String> } -> ()
            => async |st: &AppState, a: SetAiKeyArgs| { ai_settings::set_key(st, a.key) };

        /// Tests the given (maybe unsaved) settings. `key`: `null` uses the stored key, `""` sends none.
        test_ai TestAiArgs { settings: AiSettings, key: Option<String> } -> AiTestResult
            => async |st: &AppState, a: TestAiArgs| { ai_settings::test(st, a.settings, a.key).await };

        /// Model ids of the given (maybe unsaved) settings. `key` as in `test_ai`.
        list_ai_models ListAiModelsArgs { settings: AiSettings, key: Option<String> } -> Vec<String>
            => async |st: &AppState, a: ListAiModelsArgs| { ai_settings::models(st, a.settings, a.key).await };
    }

    native {
        /// The user decided the quit prompt: roll back all tab sessions and exit. `app.exit`
        /// raises `RunEvent::ExitRequested`, not the window close request, so it cannot loop.
        quit_app,

        /// The frontend reports unsaved grid edits (live or in an inactive tab), so the window close asks first.
        set_unsaved_edits,

        /// Streams the answer on `on_event`; it always ends with one `done` or `error` event.
        chat_send,

        /// Asks for the file in a native save dialog here, so the webview never chooses a path.
        export_csv,

        /// The full result of one read as INSERT statements into `request.table`, for copying rows to
        /// another environment. The file comes from a native save dialog, as for `export_csv`.
        export_result_sql,

        /// The documents of one MongoDB read as Extended JSON: a JSON array or one document per line.
        /// The file comes from a native save dialog, as for `export_csv`.
        export_documents,

        /// One table as a `.sql` file: its DDL, plus INSERT statements when `request.data`. The file
        /// comes from a native save dialog, as for `export_csv`.
        export_table_sql,

        /// Writes a PDF the UI built to a file from a native save dialog. The body is the raw PDF bytes;
        /// the `x-file-name` header is the suggested name. Returns the path, or `None` when the user
        /// closed the dialog.
        save_pdf,
    }
}

fn desktop_only(name: &str) -> AppError {
    AppError::new(
        ErrorCode::Unsupported,
        format!("{name} is only available in the desktop app"),
    )
}

/// The bridge's argument parser. Serde's message names the field, which is the whole point.
fn from_json<T: serde::de::DeserializeOwned>(body: Value) -> Res<T> {
    serde_json::from_value(body).map_err(|e| AppError::invalid(format!("Invalid arguments: {e}")))
}

fn to_json<T: Serialize>(value: T) -> Res<Value> {
    serde_json::to_value(value).map_err(|e| {
        AppError::internal("Could not serialize the command result").with_detail(e.to_string())
    })
}

/// One MongoDB connection string split into the fields the connection dialog shows.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoUriParts {
    pub redacted: String,
    /// Comma-joined host list, or the SRV host.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub auth_source: String,
    pub ssl: SslMode,
}

/// `None` for anything the driver's parser refuses; the dialog treats both as "not a URI".
fn mongo_uri_parts(uri: &str) -> Option<MongoUriParts> {
    let parsed = parse_uri(uri.trim()).ok()?;
    Some(MongoUriParts {
        redacted: parsed.redacted,
        host: parsed.host,
        port: parsed.port,
        user: parsed.user,
        database: parsed.database,
        auth_source: parsed.auth_source,
        ssl: parsed.ssl,
    })
}

fn cancelled_export() -> ExportResult {
    ExportResult {
        cancelled: true,
        rows: 0,
        path: String::new(),
    }
}

/// Asks for the destination in a native save dialog here, so the webview never chooses a path.
/// `Ok(None)` is the user closing the dialog. A dialog that never answers (its channel was
/// dropped) is an error instead of a silent cancel; a dialog that answers after the command is
/// gone leaves a trace on stderr.
async fn pick_save(
    app: &AppHandle,
    name: String,
    label: &'static str,
    extension: &'static str,
) -> Res<Option<PathBuf>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(name)
        .add_filter(label, &[extension])
        .save_file(move |p| {
            if tx.send(p).is_err() {
                eprintln!("sandb: the save dialog answered after its export command stopped");
            }
        });
    match rx.await {
        Ok(None) => Ok(None),
        Ok(Some(path)) => path
            .into_path()
            .map(Some)
            .map_err(|e| AppError::invalid("Choose a file to save to").with_detail(e.to_string())),
        Err(e) => Err(
            AppError::internal("The save dialog closed without an answer")
                .with_detail(e.to_string()),
        ),
    }
}

#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    session::close_all(&state).await;
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn set_unsaved_edits(state: State<'_, AppState>, value: bool) {
    state.set_unsaved_edits(value);
}

/// The name of the streaming chat command. The table owns the route; the bridge's SSE route and
/// the desktop command both key on this one name.
pub const CHAT_SEND: &str = "chat_send";

/// The arguments of `chat_send`, shared by the desktop command and the bridge's SSE route. The
/// command is hand-signed because of the `Channel`, so its shape lives next to it.
#[derive(Debug, serde::Deserialize)]
pub struct ChatSendArgs {
    pub request: ChatRequest,
}

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
pub async fn export_csv(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ExportRequest,
) -> Res<ExportResult> {
    ops::check_export(&state, &request)?;
    let Some(path) = pick_save(&app, "result.csv".into(), "CSV", "csv").await? else {
        return Ok(cancelled_export());
    };
    ops::export_csv(&state, request, &path).await
}

#[tauri::command]
pub async fn export_result_sql(
    app: AppHandle,
    state: State<'_, AppState>,
    request: SqlExportRequest,
) -> Res<ExportResult> {
    ops::check_sql(&state, &request)?;
    let name = format!("{}.sql", request.table);
    let Some(path) = pick_save(&app, name, "SQL", "sql").await? else {
        return Ok(cancelled_export());
    };
    ops::export_sql(&state, request, &path).await
}

#[tauri::command]
pub async fn export_documents(
    app: AppHandle,
    state: State<'_, AppState>,
    request: DocumentExportRequest,
) -> Res<ExportResult> {
    ops::check_documents(&state, &request)?;
    let (name, label, extension) = match request.format {
        DocumentFormat::Json => ("result.json", "JSON", "json"),
        DocumentFormat::Ndjson => ("result.ndjson", "NDJSON", "ndjson"),
    };
    let Some(path) = pick_save(&app, name.into(), label, extension).await? else {
        return Ok(cancelled_export());
    };
    ops::export_documents(&state, request, &path).await
}

#[tauri::command]
pub async fn export_table_sql(
    app: AppHandle,
    state: State<'_, AppState>,
    request: TableExportRequest,
) -> Res<ExportResult> {
    let conn = state.connection(&request.conn_id)?;
    // A MongoDB export is a mongosh script, not SQL.
    let (name, label, extension) = match conn.engine {
        Engine::Mongo => (format!("{}.js", request.table), "JavaScript", "js"),
        _ => (format!("{}.sql", request.table), "SQL", "sql"),
    };
    let Some(path) = pick_save(&app, name, label, extension).await? else {
        return Ok(cancelled_export());
    };
    ops::export_table_sql(&state, request, &path).await
}

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
        .ok_or_else(|| {
            AppError::invalid("The PDF needs a file name ending in .pdf without a path")
        })?;
    let Some(path) = pick_save(&app, name.to_string(), "PDF", "pdf").await? else {
        return Ok(None);
    };
    let mut file = ExportFile::create(&path).await?;
    tokio::io::AsyncWriteExt::write_all(file.out(), bytes).await?;
    let out = file.finish(0).await?;
    Ok(Some(out.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::secrets::MemoryStore;
    use serde_json::json;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        (dir, state)
    }

    #[test]
    fn command_names_are_unique() {
        let mut names = command_names();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        // The bridge's dispatch match and the Tauri handler list are generated from these names.
        assert!(total > DESKTOP_ONLY.len());
    }

    /// The bridge's SSE route keys on `CHAT_SEND`, so the table must declare that name.
    #[test]
    fn chat_send_is_declared() {
        assert!(command_names().contains(&CHAT_SEND));
        assert!(DESKTOP_ONLY.contains(&CHAT_SEND));
    }

    /// Every declared name reaches its own arm: `{}` may fail to parse (that names the missing
    /// field) or run, but it must never fall through to the unknown-command answer.
    #[tokio::test]
    async fn every_declared_command_dispatches() {
        let (_dir, state) = state();
        for name in command_names() {
            let outcome = dispatch(&state, name, json!({})).await;
            if DESKTOP_ONLY.contains(&name) {
                let err = outcome.expect_err("a desktop-only command must be refused by dispatch");
                assert_eq!(err.code, ErrorCode::Unsupported, "{name}: {}", err.message);
                continue;
            }
            if let Err(err) = outcome {
                assert_ne!(
                    err.code,
                    ErrorCode::NotFound,
                    "{name} fell through to the unknown-command arm"
                );
                assert!(
                    err.message.starts_with("Invalid arguments:"),
                    "{name}: {}",
                    err.message
                );
            }
        }
    }
}
