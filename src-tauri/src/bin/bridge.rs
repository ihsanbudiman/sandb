//! Dev-only HTTP bridge: serves the same core over `127.0.0.1` so the browser UI (Vite) can be
//! tested end to end without the Tauri shell. Never part of a release build (`dev-bridge` feature).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tower_http::cors::{AllowOrigin, CorsLayer};

use sandb_lib::core::ai::session::{self as chat, ChatEvent, ChatRequest};
use sandb_lib::core::ai::settings::{self as ai_settings, AiSettings};
use sandb_lib::core::connections::{self, ConnectionInput, Engine};
use sandb_lib::core::db::edit::{self, ApplyEditsRequest, CellValueRequest, EditableRequest};
use sandb_lib::core::db::query::{self, RunRequest};
use sandb_lib::core::db::readonly::{self, ReadLimits};
use sandb_lib::core::db::session;
use sandb_lib::core::db::{dump, introspect};
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::guard;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

const ALLOWED_ORIGIN: &str = "http://localhost:5199";
const DEFAULT_PORT: u16 = 58123;

struct Ctx {
    state: AppState,
    token: String,
}

#[tokio::main]
async fn main() {
    let token = match std::env::var("SANDB_BRIDGE_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            eprintln!("bridge: SANDB_BRIDGE_TOKEN is not set; refusing to start");
            std::process::exit(2);
        }
    };
    let port = match std::env::var("SANDB_BRIDGE_PORT") {
        Ok(p) => p.parse::<u16>().unwrap_or_else(|_| {
            eprintln!("bridge: SANDB_BRIDGE_PORT must be a port number");
            std::process::exit(2);
        }),
        Err(_) => DEFAULT_PORT,
    };
    // A temp dir unless told otherwise, so the bridge never touches the app's real data.
    let (data_dir, _tmp_guard) = match std::env::var_os("SANDB_DATA_DIR") {
        Some(d) => (std::path::PathBuf::from(d), None),
        None => {
            let t = tempfile::tempdir().expect("create temp data dir");
            (t.path().to_path_buf(), Some(t))
        }
    };
    let state = AppState::new(&data_dir, Box::new(MemoryStore::new())).unwrap_or_else(|e| {
        eprintln!("bridge: cannot open data dir {}: {e}", data_dir.display());
        std::process::exit(1);
    });
    let ctx = Arc::new(Ctx { state, token });

    let cors = CorsLayer::new()
        // `list` (not a single exact value) so other origins get no allow-origin header at all.
        .allow_origin(AllowOrigin::list([HeaderValue::from_static(
            ALLOWED_ORIGIN,
        )]))
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    // CORS is outermost so preflights are answered and even a 401 is readable by the allowed origin.
    let app = Router::new()
        .route("/api/{command}", post(dispatch))
        .layer(middleware::from_fn_with_state(ctx.clone(), auth))
        .layer(cors)
        .with_state(ctx);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("bridge: cannot bind {addr}: {e}");
            std::process::exit(1);
        });
    println!(
        "bridge: listening on http://{addr} (data dir {})",
        data_dir.display()
    );
    axum::serve(listener, app)
        .await
        .expect("bridge server failed");
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(ctx): State<Arc<Ctx>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| ct_eq(t.as_bytes(), ctx.token.as_bytes()));
    if !ok {
        let body =
            serde_json::json!({"code": "unauthorized", "message": "Missing or wrong bridge token"});
        return (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
    }
    next.run(req).await
}

fn status_of(e: &AppError) -> StatusCode {
    match e.code {
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorCode::AuthFailed | ErrorCode::Database | ErrorCode::ModelNotFound => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ErrorCode::ConnectFailed | ErrorCode::AiUpstream | ErrorCode::AiProtocol => {
            StatusCode::BAD_GATEWAY
        }
        ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
        ErrorCode::Cancelled => StatusCode::CONFLICT,
        ErrorCode::GuardRejected
        | ErrorCode::ReadOnlyViolation
        | ErrorCode::ConnectionReadOnly
        | ErrorCode::WriteBlocked => StatusCode::FORBIDDEN,
        ErrorCode::SecretStore | ErrorCode::Io | ErrorCode::Internal => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn reply<T: serde::Serialize>(r: Result<T, AppError>) -> Response {
    match r {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (status_of(&e), axum::Json(e)).into_response(),
    }
}

fn args<T: DeserializeOwned>(v: Value) -> Result<T, AppError> {
    serde_json::from_value(v)
        .map_err(|e| AppError::invalid("Invalid arguments").with_detail(e.to_string()))
}

#[derive(Deserialize)]
struct Create {
    input: ConnectionInput,
    password: Option<String>,
}

#[derive(Deserialize)]
struct Update {
    id: String,
    input: ConnectionInput,
    password: Option<String>,
}

#[derive(Deserialize)]
struct Duplicate {
    id: String,
    input: Option<ConnectionInput>,
    password: Option<String>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Test {
    input: ConnectionInput,
    password: Option<String>,
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnArgs {
    conn_id: String,
    #[serde(default)]
    database: String,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    table: String,
}

#[derive(Deserialize)]
struct RunArgs {
    request: RunRequest,
}

#[derive(Deserialize)]
struct EditableArgs {
    request: EditableRequest,
}

#[derive(Deserialize)]
struct ApplyEditsArgs {
    request: ApplyEditsRequest,
}

#[derive(Deserialize)]
struct CellValueArgs {
    request: CellValueRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TabArgs {
    tab_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelArgs {
    query_id: String,
}

#[derive(Deserialize)]
struct SqlArgs {
    engine: Engine,
    sql: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiReadArgs {
    conn_id: String,
    #[serde(default)]
    database: Option<String>,
    sql: String,
}

#[derive(Deserialize)]
struct AiSettingsArgs {
    settings: AiSettings,
}

#[derive(Deserialize)]
struct AiKeyArgs {
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct TestAiArgs {
    settings: AiSettings,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct ChatArgs {
    request: ChatRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatCancelArgs {
    chat_id: String,
}

/// `chat_send` as `text/event-stream`: one `data: <ChatEvent JSON>` per event. The chat runs in
/// its own task; a closed response drops the receiver, which stops the chat.
fn chat_stream(ctx: Arc<Ctx>, request: ChatRequest) -> Response {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
    tokio::spawn(async move {
        chat::run(&ctx.state, request, move |e| tx.send(e).is_ok()).await;
    });
    let events = futures_util::stream::unfold(rx, |mut rx| async move {
        let ev = rx.recv().await?;
        let data = serde_json::to_string(&ev).unwrap_or_default();
        Some((Ok::<_, Infallible>(Event::default().data(data)), rx))
    });
    Sse::new(events).into_response()
}

async fn dispatch(
    State(ctx): State<Arc<Ctx>>,
    Path(command): Path<String>,
    body: Option<axum::Json<Value>>,
) -> Response {
    let body = body
        .map(|b| b.0)
        .unwrap_or(Value::Object(Default::default()));
    let st = &ctx.state;
    match command.as_str() {
        "list_connections" => reply(Ok(connections::list(st))),
        "create_connection" => reply(
            async {
                let a: Create = args(body)?;
                connections::create(st, a.input, a.password)
            }
            .await,
        ),
        "update_connection" => reply(
            async {
                let a: Update = args(body)?;
                connections::update(st, &a.id, a.input, a.password).await
            }
            .await,
        ),
        "duplicate_connection" => reply(
            async {
                let a: Duplicate = args(body)?;
                connections::duplicate(st, &a.id, a.input, a.password)
            }
            .await,
        ),
        "delete_connection" => reply(
            async {
                let a: Id = args(body)?;
                connections::delete(st, &a.id).await
            }
            .await,
        ),
        "test_connection" => reply(
            async {
                let a: Test = args(body)?;
                connections::test(st, a.input, a.password, a.id.as_deref()).await
            }
            .await,
        ),
        "list_databases" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::list_databases(st, &a.conn_id).await
            }
            .await,
        ),
        "list_schemas" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::list_schemas(st, &a.conn_id, &a.database).await
            }
            .await,
        ),
        "list_tables" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::list_tables(st, &a.conn_id, &a.database, a.schema.as_deref()).await
            }
            .await,
        ),
        "list_columns" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::list_columns(st, &a.conn_id, &a.database, a.schema.as_deref(), &a.table)
                    .await
            }
            .await,
        ),
        "table_properties" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::table_properties(
                    st,
                    &a.conn_id,
                    &a.database,
                    a.schema.as_deref(),
                    &a.table,
                )
                .await
            }
            .await,
        ),
        "schema_snapshot" => reply(
            async {
                let a: ConnArgs = args(body)?;
                introspect::schema_snapshot(st, &a.conn_id, &a.database).await
            }
            .await,
        ),
        "table_ddl" => reply(
            async {
                let a: ConnArgs = args(body)?;
                dump::table_ddl(st, &a.conn_id, &a.database, a.schema.as_deref(), &a.table).await
            }
            .await,
        ),
        "run_query" => reply(
            async {
                let a: RunArgs = args(body)?;
                query::run(st, a.request).await
            }
            .await,
        ),
        "editable_result" => reply(
            async {
                let a: EditableArgs = args(body)?;
                edit::editable_result(st, a.request).await
            }
            .await,
        ),
        "apply_edits" => reply(
            async {
                let a: ApplyEditsArgs = args(body)?;
                edit::apply_edits(st, a.request).await
            }
            .await,
        ),
        "cell_value" => reply(
            async {
                let a: CellValueArgs = args(body)?;
                edit::cell_value(st, a.request).await
            }
            .await,
        ),
        "cancel_query" => reply(
            async {
                let a: CancelArgs = args(body)?;
                query::cancel(st, &a.query_id).await
            }
            .await,
        ),
        "query_progress" => {
            reply(args::<CancelArgs>(body).map(|a| query::progress(st, &a.query_id)))
        }
        "commit_transaction" => reply(
            async {
                let a: TabArgs = args(body)?;
                session::end_tx_for(st, &a.tab_id, true).await
            }
            .await,
        ),
        "rollback_transaction" => reply(
            async {
                let a: TabArgs = args(body)?;
                session::end_tx_for(st, &a.tab_id, false).await
            }
            .await,
        ),
        "tab_status" => reply(
            async {
                let a: TabArgs = args(body)?;
                session::status(st, &a.tab_id).await
            }
            .await,
        ),
        "close_tab_session" => reply(
            async {
                let a: TabArgs = args(body)?;
                st.sessions.close(&a.tab_id).await;
                Ok(())
            }
            .await,
        ),
        // Same session cleanup as the desktop quit, but the bridge keeps serving: other
        // browser tabs may still use it.
        "quit_app" => {
            for id in st.sessions.pending_tab_ids() {
                st.sessions.close(&id).await;
            }
            reply(Ok::<_, AppError>(()))
        }
        "guard_check" => reply(args::<SqlArgs>(body).map(|a| guard::guard_check(&a.sql, a.engine))),
        "classify_write" => {
            reply(args::<SqlArgs>(body).map(|a| guard::classify_write(&a.sql, a.engine)))
        }
        "run_ai_read" => reply(
            async {
                let a: AiReadArgs = args(body)?;
                readonly::run_ai_read(
                    st,
                    &a.conn_id,
                    a.database.as_deref(),
                    &a.sql,
                    ReadLimits::default(),
                )
                .await
            }
            .await,
        ),
        "chatgpt_status" => reply(Ok::<_, AppError>(sandb_lib::core::ai::oauth::SessionView {
            signed_in: false,
            account_label: None,
            usage_enabled: false,
        })),
        "chatgpt_sign_in"
        | "chatgpt_cancel_sign_in"
        | "chatgpt_logout"
        | "chatgpt_models"
        | "chatgpt_manage_usage" => reply::<()>(Err(AppError::invalid(
            "ChatGPT sign-in and plan usage require the native desktop app",
        ))),
        "get_ai_settings" => reply(ai_settings::get(st)),
        "save_ai_settings" => {
            reply(args::<AiSettingsArgs>(body).and_then(|a| ai_settings::save(st, a.settings)))
        }
        "set_ai_key" => {
            reply(args::<AiKeyArgs>(body).and_then(|a| ai_settings::set_key(st, a.key)))
        }
        "test_ai" => reply(
            async {
                let a: TestAiArgs = args(body)?;
                ai_settings::test(st, a.settings, a.key).await
            }
            .await,
        ),
        "list_ai_models" => reply(
            async {
                let a: TestAiArgs = args(body)?;
                ai_settings::models(st, a.settings, a.key).await
            }
            .await,
        ),
        "chat_send" => match args::<ChatArgs>(body) {
            Ok(a) => chat_stream(ctx.clone(), a.request),
            Err(e) => reply::<()>(Err(e)),
        },
        "chat_cancel" => reply(args::<ChatCancelArgs>(body).map(|a| chat::cancel(st, &a.chat_id))),
        // No export_csv, export_table_sql or save_pdf: the bridge never writes files on request;
        // the browser UI downloads the file client-side instead.
        _ => reply::<()>(Err(AppError::not_found(format!(
            "Unknown command {command}"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chatgpt_rpc_is_explicitly_unavailable_without_native_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Arc::new(Ctx {
            state: AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap(),
            token: "dummy-bridge-token".into(),
        });
        let status = dispatch(State(ctx.clone()), Path("chatgpt_status".into()), None).await;
        assert_eq!(status.status(), StatusCode::OK);
        let body = axum::body::to_bytes(status.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            serde_json::json!({
                "signedIn": false, "accountLabel": null, "usageEnabled": false
            })
        );
        for command in [
            "chatgpt_sign_in",
            "chatgpt_cancel_sign_in",
            "chatgpt_logout",
            "chatgpt_models",
            "chatgpt_manage_usage",
        ] {
            let response = dispatch(State(ctx.clone()), Path(command.into()), None).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert!(String::from_utf8_lossy(&body).contains("native desktop app"));
        }
    }
}
