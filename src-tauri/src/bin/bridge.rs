//! Dev-only HTTP bridge: serves the same command table over `127.0.0.1` so the browser UI
//! (Vite) can be tested end to end without the Tauri shell. Never part of a release build
//! (`dev-bridge` feature). It adds only its own concerns: a bearer token, CORS for the dev
//! origin, and the chat stream as Server-Sent Events.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tower_http::cors::{AllowOrigin, CorsLayer};

use sandb_lib::commands;
use sandb_lib::core::ai::event::ChatEvent;
use sandb_lib::core::ai::session as chat;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

// Dev config, one place per side: the TS defaults live in `src/lib/api/devConfig.ts`.
/// The Vite dev server's port; `vite.config.ts` reads the same variable.
const DEV_PORT_ENV: &str = "SANDB_DEV_PORT";
const DEFAULT_DEV_PORT: u16 = 5199;
/// The bridge's port; it must match the default in `src/lib/api/devConfig.ts`.
const PORT_ENV: &str = "SANDB_BRIDGE_PORT";
const DEFAULT_PORT: u16 = 58123;
/// The bearer token the frontend sends; the client's variable is `VITE_BRIDGE_TOKEN`.
const TOKEN_ENV: &str = "SANDB_BRIDGE_TOKEN";

#[tokio::main]
async fn main() {
    let token: Arc<str> = match std::env::var(TOKEN_ENV) {
        Ok(t) if !t.trim().is_empty() => Arc::from(t),
        _ => {
            eprintln!("bridge: {TOKEN_ENV} is not set; refusing to start");
            std::process::exit(2);
        }
    };
    let port = env_port(PORT_ENV, DEFAULT_PORT);
    let dev_port = env_port(DEV_PORT_ENV, DEFAULT_DEV_PORT);
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

    let cors = CorsLayer::new()
        // `list` (not a single exact value) so other origins get no allow-origin header at all.
        .allow_origin(AllowOrigin::list([dev_origin(dev_port)]))
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    // CORS is outermost so preflights are answered and even a 401 is readable by the allowed origin.
    let app = Router::new()
        .route("/api/{command}", post(route))
        .layer(middleware::from_fn_with_state(token, auth))
        .layer(cors)
        .with_state(Arc::new(state));

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

fn env_port(name: &str, default: u16) -> u16 {
    match std::env::var(name) {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("bridge: {name} must be a port number");
            std::process::exit(2);
        }),
        Err(_) => default,
    }
}

fn dev_origin(dev_port: u16) -> HeaderValue {
    HeaderValue::from_str(&format!("http://localhost:{dev_port}"))
        .expect("the dev origin is a valid header value")
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(token): State<Arc<str>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| ct_eq(t.as_bytes(), token.as_bytes()));
    if !ok {
        return reply::<()>(Err(AppError::new(
            ErrorCode::Unauthorized,
            "Missing or wrong bridge token",
        )));
    }
    next.run(req).await
}

fn reply<T: serde::Serialize>(r: Result<T, AppError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let status = StatusCode::from_u16(e.code.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(e)).into_response()
        }
    }
}

/// `chat_send` as `text/event-stream`: one `data: <ChatEvent JSON>` per event. The chat runs in
/// its own task; a closed response drops the receiver, which stops the chat.
fn chat_stream(state: Arc<AppState>, request: chat::ChatRequest) -> Response {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
    tokio::spawn(async move {
        chat::run(&state, request, move |e| tx.send(e).is_ok()).await;
    });
    let events = futures_util::stream::unfold(rx, |mut rx| async move {
        let ev = rx.recv().await?;
        Some((Ok::<_, Infallible>(Event::default().data(frame(&ev))), rx))
    });
    Sse::new(events).into_response()
}

/// One SSE data frame. An event that cannot be serialized becomes an `ai_protocol` error event
/// instead of an empty frame the client would report as a protocol error.
fn frame(ev: &ChatEvent) -> String {
    serde_json::to_string(ev).unwrap_or_else(|e| {
        let fallback = ChatEvent::Error {
            error: AppError::new(
                ErrorCode::AiProtocol,
                "The bridge could not encode a chat event",
            )
            .with_detail(e.to_string()),
        };
        serde_json::to_string(&fallback).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "error",
                "error": {
                    "code": "ai_protocol",
                    "message": "The bridge could not encode a chat event",
                },
            })
            .to_string()
        })
    })
}

async fn route(
    State(state): State<Arc<AppState>>,
    Path(command): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(Json(body)) = body else {
        return reply::<()>(Err(AppError::invalid(
            "The request needs a JSON body with the command's arguments",
        )));
    };
    if command == commands::CHAT_SEND {
        return match serde_json::from_value::<commands::ChatSendArgs>(body) {
            Ok(args) => chat_stream(state, args.request),
            Err(e) => reply::<()>(Err(AppError::invalid(format!("Invalid arguments: {e}")))),
        };
    }
    reply(commands::dispatch(&state, &command, body).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_use_the_one_status_mapping() {
        let unauthorized = reply::<()>(Err(AppError::new(
            ErrorCode::Unauthorized,
            "Missing or wrong bridge token",
        )));
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let native = reply::<()>(Err(AppError::new(
            ErrorCode::Unsupported,
            "save_pdf is only available in the desktop app",
        )));
        assert_eq!(native.status(), StatusCode::BAD_REQUEST);
    }
}
