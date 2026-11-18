//! The HTTP server of MCP and its life cycle.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{RoleServer, ServerHandler};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::error::{AppError, ErrorCode};
use crate::core::mcp::{McpSettings, McpStatus, settings, tools};
use crate::core::secrets::ct_eq;
use crate::core::state::AppState;

/// The app state for the life of the server task. The desktop app passes its `AppHandle`;
/// tests pass `Arc<AppState>`.
pub trait StateHandle: Clone + Send + Sync + 'static {
    fn state(&self) -> &AppState;
}

impl StateHandle for Arc<AppState> {
    fn state(&self) -> &AppState {
        self
    }
}

/// The running server, if any, and the last start failure. `Default` is off.
#[derive(Default)]
pub struct McpServer {
    slot: Mutex<Slot>,
}

#[derive(Default)]
struct Slot {
    running: Option<Running>,
    error: Option<String>,
}

struct Running {
    /// The bound address; its port is the real one, also when `apply` asked for port 0.
    addr: SocketAddr,
    token: String,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

/// The MCP handler: one per session, all reading the same app state.
#[derive(Clone)]
struct Handler<H> {
    handle: H,
}

impl<H: StateHandle> ServerHandler for Handler<H> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sandb", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Read-only access to the databases that the sandb user shares. \
                 Call list_connections first; pass its id as connection on every other tool.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            tools::list()
                .into_iter()
                .map(|t| {
                    let schema = Arc::new(t.schema.as_object().cloned().unwrap_or_default());
                    Tool::new(t.name, t.description, schema)
                        .with_annotations(ToolAnnotations::new().read_only(true))
                })
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let out = tools::call(
            self.handle.state(),
            request.name.as_ref(),
            request.arguments.unwrap_or_default(),
        )
        .await;
        let content = vec![ContentBlock::text(out.text)];
        // A failed tool is a result the client shows, not a protocol error.
        let result = if out.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        };
        Ok(result.into())
    }
}

/// Binds `127.0.0.1:port` and starts serving `/mcp`. `port` 0 lets the OS pick one (tests).
async fn start<H: StateHandle>(handle: H, port: u16, token: String) -> Result<Running, AppError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| AppError::invalid(format!("Could not listen on 127.0.0.1:{port}: {e}")))?;
    let addr = listener.local_addr()?;
    let stop = CancellationToken::new();
    // The default `allowed_hosts` (localhost, 127.0.0.1, ::1) blocks DNS rebinding. An empty
    // origin list with validation on refuses every browser request that carries `Origin`.
    let config = StreamableHttpServerConfig::default()
        .enforce_origin_validation()
        .with_cancellation_token(stop.child_token());
    let service = StreamableHttpService::new(
        move || {
            Ok(Handler {
                handle: handle.clone(),
            })
        },
        LocalSessionManager::default().into(),
        config,
    );
    let router = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            Arc::<str>::from(token.as_str()),
            auth,
        ));
    let shutdown = stop.clone();
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
        if let Err(e) = served {
            eprintln!("sandb: MCP server stopped: {e}");
        }
    });
    Ok(Running {
        addr,
        token,
        stop,
        task,
    })
}

/// Stops the server. The timeout keeps a client that never closes its stream from blocking a
/// restart: after it, the task is detached and the app goes on.
async fn stop(r: Running) {
    r.stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), r.task).await;
}

async fn auth(State(token): State<Arc<str>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| ct_eq(t.as_bytes(), token.as_bytes()));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(AppError::new(
                ErrorCode::Unauthorized,
                "Missing or wrong sandb MCP token",
            )),
        )
            .into_response();
    }
    next.run(req).await
}

/// The settings, the token and whether the server runs. `running` is the slot, not a probe.
pub async fn status(state: &AppState) -> Result<McpStatus, AppError> {
    let settings = settings::load(state)?;
    let token = settings::token(state)?;
    let slot = state.mcp.slot.lock().await;
    Ok(McpStatus {
        url: format!("http://127.0.0.1:{}/mcp", settings.port),
        settings,
        token,
        running: slot.running.is_some(),
        error: slot.error.clone(),
    })
}

/// Makes the server match the saved settings: starts it, stops it, or restarts it when the port
/// or the token changed; an allowlist change needs no restart. The slot lock is held for the
/// whole operation, so two applies cannot overlap.
pub async fn apply<H: StateHandle>(handle: &H) -> Result<McpStatus, AppError> {
    let state = handle.state();
    let mut slot = state.mcp.slot.lock().await;
    let settings = settings::load(state)?;
    let token = settings::token(state)?;
    let same = slot
        .running
        .as_ref()
        .is_some_and(|r| r.addr.port() == settings.port && r.token == token);
    if !settings.enabled || !same {
        if let Some(r) = slot.running.take() {
            stop(r).await;
        }
        slot.error = None;
        if settings.enabled {
            // A failure is shown in the status, not returned: the settings stay saved.
            match start(handle.clone(), settings.port, token.clone()).await {
                Ok(r) => slot.running = Some(r),
                Err(e) => slot.error = Some(e.message),
            }
        }
    }
    // Built from the held slot; `status` would take the same lock twice.
    let url = format!("http://127.0.0.1:{}/mcp", settings.port);
    let running = slot.running.is_some();
    let error = slot.error.clone();
    Ok(McpStatus {
        settings,
        token,
        url,
        running,
        error,
    })
}

/// Saves the settings, then applies them.
pub async fn save<H: StateHandle>(handle: &H, s: McpSettings) -> Result<McpStatus, AppError> {
    let state = handle.state();
    let settings = s.normalized(state)?;
    settings::save(state, &settings)?;
    apply(handle).await
}

/// Replaces the token, so every client that uses the old one gets 401, then applies.
pub async fn new_token<H: StateHandle>(handle: &H) -> Result<McpStatus, AppError> {
    settings::set_new_token(handle.state())?;
    apply(handle).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::secrets::MemoryStore;

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

    fn state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path(), Box::new(MemoryStore::default())).unwrap();
        (dir, Arc::new(state))
    }

    fn post(client: &reqwest::Client, addr: SocketAddr) -> reqwest::RequestBuilder {
        client
            .post(format!("http://{addr}/mcp"))
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(INITIALIZE)
    }

    #[tokio::test]
    async fn the_server_needs_the_token_and_stops_cleanly() {
        // reqwest is built without a bundled provider, as in `AiClient::new`.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (_dir, state) = state();
        let running = start(Arc::clone(&state), 0, "t0k".into()).await.unwrap();
        let addr = running.addr;
        let client = reqwest::Client::new();

        let r = post(&client, addr).send().await.unwrap();
        assert_eq!(r.status(), 401, "no token must be refused");

        let r = post(&client, addr)
            .header(reqwest::header::AUTHORIZATION, "Bearer wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "a wrong token must be refused");

        let r = post(&client, addr)
            .header(reqwest::header::AUTHORIZATION, "Bearer t0k")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = r.text().await.unwrap();
        assert!(body.contains("\"serverInfo\""), "{body}");
        assert!(body.contains("sandb"), "{body}");

        // DNS rebinding: rmcp's default allowed hosts refuse a foreign `Host` even with the token.
        let r = post(&client, addr)
            .header(reqwest::header::AUTHORIZATION, "Bearer t0k")
            .header(reqwest::header::HOST, "evil.example")
            .send()
            .await
            .unwrap();
        assert_ne!(r.status(), 200, "a foreign Host header must be refused");

        stop(running).await;
        tokio::net::TcpListener::bind(addr)
            .await
            .expect("the port is free again after stop");
    }

    #[tokio::test]
    async fn status_reports_the_saved_settings_and_the_generated_token() {
        let (_dir, state) = state();
        let s = status(&state).await.unwrap();
        assert!(!s.running);
        assert!(!s.settings.enabled);
        assert_eq!(s.url, format!("http://127.0.0.1:{}/mcp", s.settings.port));
        assert_eq!(s.token.len(), 64);
        assert!(s.error.is_none());
        assert!(s.settings.connections.is_empty());
    }
}
