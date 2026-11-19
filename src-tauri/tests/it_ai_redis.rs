//! Chat against the isolated Redis, PostgreSQL, and mock LLM fixtures.
#![cfg(feature = "it")]
mod common;

use common::{Ctx, assert_done, tool, tools};
use sandb_lib::core::ai::event::ChatEvent;
use sandb_lib::core::ai::session::{self, ChatRequest, ChatSource};
use sandb_lib::core::ai::settings::{self, AiSettings};
use sandb_lib::core::connections::{Connection, RedisMode};
use sandb_lib::core::engine::Engine;
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::ops;
use sandb_lib::core::result::ReadLimits;
use serde_json::{Value, json};

fn setup() -> Ctx {
    let mut input = common::redis_input(
        "ai-redis",
        common::redis(),
        &common::rw_user(),
        RedisMode::Standalone,
        false,
    );
    input.ai_write = true;
    let ctx = common::ctx("ai-redis", input, Some(common::rw_password()));
    settings::save(
        &ctx.st,
        AiSettings {
            base_url: common::mock_url(),
            model: "mock-gpt".into(),
            ..Default::default()
        },
    )
    .unwrap();
    ctx
}

fn source(conn: &Connection, allow_writes: bool) -> ChatSource {
    ChatSource {
        conn_id: conn.id.clone(),
        database: None,
        schema: None,
        allow_writes,
    }
}

fn request(message: &str, sources: Vec<ChatSource>) -> ChatRequest {
    ChatRequest {
        chat_id: uuid::Uuid::new_v4().to_string(),
        sources,
        history: Vec::new(),
        message: message.into(),
        skills: Vec::new(),
    }
}

async fn chat(ctx: &Ctx, req: ChatRequest) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    session::run(&ctx.st, req, |event| {
        events.push(event);
        true
    })
    .await;
    assert_done(&events);
    events
}

async fn read_cell(ctx: &Ctx, command: &str) -> Value {
    let out = ops::run_query(&ctx.st, common::req(&ctx.conn, command))
        .await
        .unwrap();
    common::cell_named(&out, "value", 0)
}

#[tokio::test]
async fn redis_chat_reads_a_hash_and_blocks_keys() {
    let ctx = setup();
    let events = chat(&ctx, request("redis read", vec![source(&ctx.conn, false)])).await;
    let (name, _, result) = tool(&events);
    assert_eq!(name, "run_redis_read");
    let ChatEvent::ToolResult {
        ok: true,
        columns: Some(columns),
        rows: Some(rows),
        ..
    } = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(columns, &["field", "value"]);
    assert!(rows.contains(&vec![json!("name"), json!("User 1")]));

    let events = chat(&ctx, request("redis keys", vec![source(&ctx.conn, false)])).await;
    let (_, _, result) = tool(&events);
    let ChatEvent::ToolResult {
        ok: false,
        error: Some(error),
        ..
    } = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(error.code, ErrorCode::GuardRejected);
    assert!(error.message.contains("Use SCAN"));
}

#[tokio::test]
async fn redis_chat_does_not_forward_server_configuration() {
    let ctx = setup();
    let events = chat(
        &ctx,
        request("redis settings", vec![source(&ctx.conn, false)]),
    )
    .await;
    let (name, _, result) = tool(&events);
    assert_eq!(name, "run_redis_read");
    assert!(matches!(
        result,
        ChatEvent::ToolResult { ok: false, rows: None, error: Some(error), .. }
            if error.code == ErrorCode::GuardRejected
    ));
    assert!(common::text(&events).contains("CONFIG GET can expose private server details"));
}

#[tokio::test]
async fn redis_chat_charts_leaderboard_scores_in_rank_order() {
    let ctx = setup();
    let events = chat(&ctx, request("redis chart", vec![source(&ctx.conn, false)])).await;
    let (spec, columns, rows) = events
        .iter()
        .find_map(|event| match event {
            ChatEvent::Chart {
                spec,
                columns,
                rows,
                ..
            } => Some((spec, columns, rows)),
            _ => None,
        })
        .expect("chart with rows");
    assert_eq!(spec.x, "member");
    assert_eq!(spec.y, ["score"]);
    assert_eq!(columns, &["member", "score"]);
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0][0], "player:1000");
    assert_eq!(rows[0][1].as_f64(), Some(1000.0));
    assert_eq!(rows[9][0], "player:991");
    assert_eq!(rows[9][1].as_f64(), Some(991.0));
}

#[tokio::test]
async fn redis_chat_write_waits_for_approval() {
    let ctx = setup();
    let key = common::scratch();
    let req = request(&format!("redis write {key}"), vec![source(&ctx.conn, true)]);
    let id = req.chat_id.clone();
    let read = format!("HGET {key} name");
    let (send, recv) = tokio::sync::oneshot::channel();
    let mut send = Some(send);
    let mut events = Vec::new();
    let chat = session::run(&ctx.st, req, |event| {
        if let ChatEvent::WritePreview { id, sql, .. } = &event {
            assert_eq!(sql, &format!("HSET {key} name Mock"));
            send.take().unwrap().send(id.clone()).unwrap();
        }
        events.push(event);
        true
    });
    let approval = async {
        let call = tokio::time::timeout(std::time::Duration::from_secs(15), recv)
            .await
            .expect("write preview before approval")
            .unwrap();
        assert!(read_cell(&ctx, &read).await.is_null());
        session::decide(&ctx.st, &id, &call, true).unwrap();
    };
    tokio::join!(chat, approval);
    let value = read_cell(&ctx, &read).await;
    ops::run_query(&ctx.st, common::req(&ctx.conn, &format!("DEL {key}")))
        .await
        .unwrap();
    assert_done(&events);
    assert_eq!(value, "Mock");
    let (name, _, result) = tool(&events);
    assert_eq!(name, "propose_redis_write");
    assert!(matches!(
        result,
        ChatEvent::ToolResult { ok: true, result: Some(data), .. }
            if data.committed == Some(true) && data.rows.is_none()
    ));
}

#[tokio::test]
async fn mixed_chat_reads_the_intended_source_for_each_tool() {
    let ctx = setup();
    let pg = ctx
        .st
        .create_connection(
            common::sql_input("ai-redis-pg", Engine::Pg, common::pg16(), false),
            Some(common::rw_password()),
        )
        .unwrap();
    let events = chat(
        &ctx,
        request(
            "redis mixed",
            vec![source(&pg, false), source(&ctx.conn, false)],
        ),
    )
    .await;
    let calls = tools(&events);
    assert_eq!(calls.len(), 2);
    let (sql, sql_args, sql_result) = calls[0];
    assert_eq!(sql, "run_read_query");
    assert_eq!(sql_args["source"], "db1");
    assert!(matches!(sql_result, ChatEvent::ToolResult { ok: true, .. }));
    let (redis, redis_args, redis_result) = calls[1];
    assert_eq!(redis, "run_redis_read");
    assert_eq!(redis_args["source"], "db2");
    assert!(
        matches!(redis_result, ChatEvent::ToolResult { ok: true, rows: Some(rows), .. }
        if rows == &vec![vec![json!("greeting"), json!("hello")]])
    );
}

#[tokio::test]
async fn redis_ai_limits_rows_and_the_acl_refuses_a_write_without_the_guard() {
    let ctx = common::ctx(
        "ai-redis-acl",
        common::redis_input(
            "ai-redis-acl",
            common::redis(),
            &common::ro_user(),
            RedisMode::Standalone,
            true,
        ),
        Some(common::ro_password()),
    );
    let limits = ReadLimits {
        max_rows: 2,
        ..Default::default()
    };
    let page = ops::run_ai_read(&ctx.st, &ctx.conn.id, None, "HGETALL user:1", limits)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert!(page.truncated);
    let cmd = format!("SET {} forbidden", common::scratch());
    let error = ops::run_read_only_unchecked(&ctx.st, &ctx.conn.id, None, &cmd, limits)
        .await
        .unwrap_err();
    assert_eq!(error.server_code.unwrap().code, "NOPERM");
}
