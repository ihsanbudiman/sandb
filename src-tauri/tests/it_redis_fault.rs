//! Cut a Redis connection after an operation sends its next request.
#![cfg(feature = "it")]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sandb_lib::core::ai::event::ChatEvent;
use sandb_lib::core::ai::session::{self, ChatRequest, ChatSource};
use sandb_lib::core::ai::settings::{self, AiSettings};
use sandb_lib::core::connections::RedisMode;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::ops;
use sandb_lib::core::result::{ApplyEditsRequest, QueryRun};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

struct FaultProxy {
    port: u16,
    cut_next: Arc<AtomicBool>,
    listener: JoinHandle<()>,
}

impl FaultProxy {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cut_next = Arc::new(AtomicBool::new(false));
        let flag = cut_next.clone();
        let listener = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let flag = flag.clone();
                tokio::spawn(async move {
                    let server = TcpStream::connect(("127.0.0.1", common::redis()))
                        .await
                        .unwrap();
                    let (mut from_client, mut to_client) = client.into_split();
                    let (mut from_server, mut to_server) = server.into_split();
                    let forward = async {
                        let mut block = [0; 8192];
                        loop {
                            let n = from_client.read(&mut block).await?;
                            if n == 0 {
                                return Ok::<(), std::io::Error>(());
                            }
                            let cut = flag.swap(false, Ordering::AcqRel);
                            to_server.write_all(&block[..n]).await?;
                            if cut {
                                return Ok(());
                            }
                        }
                    };
                    // Cut before sending a reply, even when Redis already ran the command.
                    tokio::select! {
                        biased;
                        _ = forward => (),
                        _ = tokio::io::copy(&mut from_server, &mut to_client) => (),
                    }
                });
            }
        });
        Self {
            port,
            cut_next,
            listener,
        }
    }

    fn arm(&self) {
        self.cut_next.store(true, Ordering::Release);
    }

    fn assert_cut(&self) {
        assert!(!self.cut_next.load(Ordering::Acquire), "proxy did not cut");
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

fn setup(port: u16) -> common::Ctx {
    common::ctx(
        "redis-fault",
        common::redis_input(
            "fault",
            port,
            &common::rw_user(),
            RedisMode::Standalone,
            false,
        ),
        Some(common::rw_password()),
    )
}

async fn query(ctx: &common::Ctx, text: &str) -> Result<QueryRun, AppError> {
    tokio::time::timeout(
        Duration::from_secs(5),
        ops::run_query(&ctx.st, common::req(&ctx.conn, text)),
    )
    .await
    .expect("Redis operation returned after the cut")
}

async fn ping(ctx: &common::Ctx) {
    let out = query(ctx, "PING").await.unwrap();
    assert_eq!(common::cell(&out, 0, 0), "PONG");
}

#[tokio::test]
async fn editor_read_recovers_on_the_next_command() {
    let proxy = FaultProxy::start().await;
    let ctx = setup(proxy.port);
    ping(&ctx).await;
    proxy.arm();
    let error = query(&ctx, "GET greeting").await.unwrap_err();
    proxy.assert_cut();
    assert_eq!(error.code, ErrorCode::ConnectFailed);
    ping(&ctx).await;
}

#[tokio::test]
async fn grid_save_reports_an_unknown_outcome_and_recovers() {
    let proxy = FaultProxy::start().await;
    let ctx = setup(proxy.port);
    let key = common::scratch();
    query(&ctx, &format!("SET {key} before PX 60000"))
        .await
        .unwrap();
    let op = serde_json::from_value(json!({
        "key": "row", "kind": "update", "pk": {"key": key}, "set": {"value": "after"},
        "old": {"value": {"type": "string", "value": "before"}}
    }))
    .unwrap();
    let req = ApplyEditsRequest {
        conn_id: ctx.conn.id.clone(),
        database: "0".into(),
        sql: format!("GET {key}"),
        tab_id: None,
        query_id: Some(uuid::Uuid::new_v4().to_string()),
        operations: vec![op],
    };
    proxy.arm();
    let error = ops::apply_edits(&ctx.st, req).await.unwrap_err();
    proxy.assert_cut();
    assert_eq!(error.code, ErrorCode::ConnectFailed);
    assert!(error.detail.unwrap().contains("may have run"));
    let out = query(&ctx, &format!("GET {key}")).await.unwrap();
    let value = common::cell_named(&out, "value", 0);
    assert!(value == "before" || value == "after");
    query(&ctx, &format!("UNLINK {key}")).await.unwrap();
}

#[tokio::test]
async fn assistant_read_reports_connection_loss_and_recovers() {
    let proxy = FaultProxy::start().await;
    let ctx = setup(proxy.port);
    ping(&ctx).await;
    settings::save(
        &ctx.st,
        AiSettings {
            base_url: common::mock_url(),
            model: "mock-gpt".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let request = || ChatRequest {
        chat_id: uuid::Uuid::new_v4().to_string(),
        sources: vec![ChatSource {
            conn_id: ctx.conn.id.clone(),
            database: None,
            schema: None,
            allow_writes: false,
        }],
        history: Vec::new(),
        message: "redis read".into(),
        skills: Vec::new(),
    };
    let mut events = Vec::new();
    session::run(&ctx.st, request(), |event| {
        if matches!(&event, ChatEvent::ToolCall { name, .. } if name == "run_redis_read") {
            proxy.arm();
        }
        events.push(event);
        true
    })
    .await;
    proxy.assert_cut();
    common::assert_done(&events);
    let (_, _, result) = common::tool(&events);
    assert!(
        matches!(result, ChatEvent::ToolResult { ok: false, error: Some(error), .. }
        if error.code == ErrorCode::ConnectFailed)
    );

    events.clear();
    session::run(&ctx.st, request(), |event| {
        events.push(event);
        true
    })
    .await;
    common::assert_done(&events);
    let (_, _, result) = common::tool(&events);
    assert!(
        matches!(result, ChatEvent::ToolResult { ok: true, rows: Some(rows), .. }
        if rows.iter().any(|row| row == &[json!("name"), json!("User 1")]))
    );
}
