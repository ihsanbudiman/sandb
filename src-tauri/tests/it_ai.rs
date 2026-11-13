//! Phase 4 integration tests: the chat loop against `mock-llm` and the pg16 `shop` database,
//! logged in as `sandb_rw`. Needs the sandb-test docker stack (`pnpm db:up`) and a mock LLM
//! with `GET /_requests` at `SANDB_MOCK_LLM_URL` (default `http://127.0.0.1:58080/v1`).
//! Unreachable services FAIL; nothing is skipped.
#![cfg(feature = "it")]

mod common;

use serde_json::Value;

use common::{Ctx, assert_terminal, recorded_with, text, tool, tool_contents};
use sandb_lib::core::ai::event::ChatEvent;
use sandb_lib::core::ai::session::{self, ChatRequest, ChatRole, ChatSource, ChatTurn};
use sandb_lib::core::ai::settings::{self, AiSettings, Privacy};
use sandb_lib::core::ai::tools;
use sandb_lib::core::result::Source;
use sandb_lib::core::result::ReadLimits;
use sandb_lib::core::ops;
use sandb_lib::core::engine::Engine;
use sandb_lib::core::error::ErrorCode;

/// Tests that read or write `orders` row 1. `an_approved_write_commits` changes the row and puts
/// it back, so a parallel test that compares the row before and after its run must not overlap it.
static ORDER_1: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn ai_settings(privacy: Privacy) -> AiSettings {
    AiSettings {
        base_url: common::mock_url(),
        model: "mock-gpt".into(),
        privacy,
        ..Default::default()
    }
}

fn setup(privacy: Privacy, read_only: bool) -> Ctx {
    let mut input = common::sql_input("ai-pg16", Engine::Pg, common::pg16(), false);
    // AI writes on: each chat sends its own `allowWrites`, which the tests vary.
    input.ai_write = true;
    input.read_only = read_only;
    let ctx = common::ctx("pg16", input, Some(common::rw_password()));
    settings::save(&ctx.st, ai_settings(privacy)).unwrap();
    ctx
}

fn request(ctx: &Ctx, chat_id: &str, message: &str, allow_writes: bool) -> ChatRequest {
    ChatRequest {
        chat_id: chat_id.into(),
        sources: vec![ChatSource {
            conn_id: ctx.conn.id.clone(),
            database: None,
            schema: None,
            allow_writes,
        }],
        history: vec![ChatTurn {
            role: ChatRole::Assistant,
            content: "Hello, how can I help?".into(),
        }],
        message: message.into(),
    }
}

async fn chat(ctx: &Ctx, message: &str, allow_writes: bool) -> Vec<ChatEvent> {
    let chat_id = uuid::Uuid::new_v4().to_string();
    let mut events = Vec::new();
    session::run(
        &ctx.st,
        request(ctx, &chat_id, message, allow_writes),
        |e| {
            events.push(e);
            true
        },
    )
    .await;
    assert!(!ctx.st.chats().is_running(&chat_id), "registry entry left");
    events
}

#[tokio::test]
async fn list_scenario_lists_tables() {
    let ctx = setup(Privacy::Results, false);
    let events = chat(&ctx, "please list the tables", false).await;
    assert_terminal(&events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, _, result) = tool(&events);
    assert_eq!(name, "list_tables");
    match result {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{result:?}");
            assert!(summary.ends_with(" tables"), "{summary}");
        }
        _ => unreachable!(),
    }
    let t = text(&events);
    assert!(t.starts_with("Tool result received: {"), "{t}");
    assert!(t.contains("\"tables\":[{"), "{t}");
}

#[tokio::test]
async fn describe_scenario_describes_orders() {
    let ctx = setup(Privacy::Results, false);
    let events = chat(&ctx, "describe that table", false).await;
    assert_terminal(&events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, args, result) = tool(&events);
    assert_eq!(name, "describe_table");
    assert_eq!(args["table"], "orders");
    match result {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{result:?}");
            assert!(summary.ends_with(" columns"), "{summary}");
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn count_scenario_runs_a_read() {
    let ctx = setup(Privacy::Results, false);
    let events = chat(&ctx, "count the orders", false).await;
    assert_terminal(&events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, args, result) = tool(&events);
    assert_eq!(name, "run_read_query");
    assert_eq!(args["sql"], "SELECT count(*) FROM orders");
    match result {
        ChatEvent::ToolResult {
            ok,
            summary,
            columns,
            rows,
            truncated,
            ..
        } => {
            assert!(ok, "{result:?}");
            assert!(summary.starts_with("1 row · "), "{summary}");
            assert!(summary.ends_with(" ms"), "{summary}");
            assert_eq!(columns.as_deref(), Some(&["count".to_string()][..]));
            assert_eq!(rows.as_ref().unwrap().len(), 1);
            assert_eq!(*truncated, Some(false));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn bypass_is_refused_by_the_guard() {
    let ctx = setup(Privacy::Results, false);
    let events = chat(&ctx, "bypass the rules", true).await;
    assert_terminal(&events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, args, result) = tool(&events);
    assert_eq!(name, "run_read_query");
    assert_eq!(args["sql"], "DELETE FROM orders");
    match result {
        ChatEvent::ToolResult {
            ok, error, summary, ..
        } => {
            assert!(!ok);
            let e = error.as_ref().unwrap();
            assert_eq!(e.code, ErrorCode::GuardRejected);
            assert_eq!(summary, &e.message);
        }
        _ => unreachable!(),
    }
    // The model got the refusal, not a success.
    assert!(text(&events).contains("guard_rejected"), "{events:#?}");
}

#[tokio::test]
async fn chart_scenario_sends_rows_to_the_ui_only() {
    let ctx = setup(Privacy::Schema, false);
    let events = chat(&ctx, "chart the orders", false).await;
    assert_terminal(&events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, _, result) = tool(&events);
    assert_eq!(name, "render_chart");
    match result {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{result:?}");
            assert!(summary.starts_with("Bar chart · "), "{summary}");
        }
        _ => unreachable!(),
    }
    // The chart comes before its result, so the UI can draw it in place.
    let at = |f: fn(&ChatEvent) -> bool| events.iter().position(f).unwrap();
    assert!(
        at(|e| matches!(e, ChatEvent::Chart { .. }))
            < at(|e| matches!(e, ChatEvent::ToolResult { .. }))
    );
    let Some(ChatEvent::Chart {
        spec,
        columns,
        rows,
        truncated,
        ..
    }) = events.iter().find(|e| matches!(e, ChatEvent::Chart { .. }))
    else {
        unreachable!()
    };
    assert_eq!(spec.x, "status");
    assert_eq!(columns, &["status", "orders"]);
    assert!(!rows.is_empty() && !truncated);
    // `schema` privacy: the user sees the values, the model only names and counts.
    let status = rows[0][0].as_str().unwrap();
    let t = text(&events);
    assert!(t.contains("\"rowCount\""), "{t}");
    assert!(!t.contains(status), "{t}");
}

async fn order_1_status(ctx: &Ctx) -> Value {
    let r = ops::run_ai_read(&ctx.st, &ctx.conn.id, None, "SELECT status FROM orders WHERE id = 1", ReadLimits::default(), )
    .await
    .unwrap();
    r.rows[0][0].clone()
}

fn assert_write_blocked(events: &[ChatEvent]) {
    assert_terminal(events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::WritePreview { .. })),
        "{events:#?}"
    );
    let (name, _, result) = tool(events);
    assert_eq!(name, "propose_write");
    match result {
        ChatEvent::ToolResult { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::WriteBlocked);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn write_with_write_mode_off_is_blocked() {
    let _order_1 = ORDER_1.lock().await;
    let ctx = setup(Privacy::Results, false);
    let before = order_1_status(&ctx).await;
    let events = chat(&ctx, "write the change", false).await;
    assert_write_blocked(&events);
    assert_eq!(order_1_status(&ctx).await, before);
}

#[tokio::test]
async fn write_on_a_read_only_connection_is_blocked() {
    let ctx = setup(Privacy::Results, true);
    let events = chat(&ctx, "write the change", true).await;
    assert_write_blocked(&events);
}

/// Runs the `write` scenario and answers its approval card with `approve`.
async fn chat_deciding(ctx: &Ctx, approve: bool) -> Vec<ChatEvent> {
    let chat_id = uuid::Uuid::new_v4().to_string();
    let mut events = Vec::new();
    session::run(
        &ctx.st,
        request(ctx, &chat_id, "write the change", true),
        |e| {
            if let ChatEvent::WritePreview { id, .. } = &e {
                session::decide(&ctx.st, &chat_id, id, approve).unwrap();
            }
            events.push(e);
            true
        },
    )
    .await;
    events
}

/// The preview comes first; the result comes only after the user decides.
fn preview_then_result(events: &[ChatEvent]) -> &ChatEvent {
    assert_terminal(events);
    assert_eq!(events.last(), Some(&ChatEvent::Done));
    let (name, _, result) = tool(events);
    assert_eq!(name, "propose_write");
    let preview = events
        .iter()
        .position(|e| matches!(e, ChatEvent::WritePreview { .. }))
        .unwrap_or_else(|| panic!("no write_preview: {events:#?}"));
    let result_at = events.iter().position(|e| e == result).unwrap();
    assert!(preview < result_at, "preview must come before the result");
    match &events[preview] {
        ChatEvent::WritePreview { sql, info, .. } => {
            assert_eq!(sql, "UPDATE orders SET status = 'shipped' WHERE id = 1");
            assert_eq!(info.statements[0].kind, "UPDATE");
        }
        _ => unreachable!(),
    }
    result
}

#[tokio::test]
async fn a_denied_write_runs_nothing() {
    let _order_1 = ORDER_1.lock().await;
    let ctx = setup(Privacy::Results, false);
    let before = order_1_status(&ctx).await;
    let events = chat_deciding(&ctx, false).await;
    match preview_then_result(&events) {
        ChatEvent::ToolResult { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::WriteDenied);
        }
        _ => unreachable!(),
    }
    assert_eq!(order_1_status(&ctx).await, before, "a denied write ran SQL");
}

#[tokio::test]
async fn an_approved_write_commits() {
    let _order_1 = ORDER_1.lock().await;
    let ctx = setup(Privacy::Results, false);
    let before = order_1_status(&ctx).await;
    let events = chat_deciding(&ctx, true).await;
    let result = preview_then_result(&events).clone();
    let after = order_1_status(&ctx).await;
    // Put the row back before any assert, so other tests see the seed data.
    let restore = format!(
        "UPDATE orders SET status = '{}' WHERE id = 1",
        before.as_str().unwrap().replace('\'', "''")
    );
    ops::run_approved_write(&ctx.st, &ctx.conn.id, "shop", &restore, &Default::default())
        .await
        .unwrap();
    match result {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{summary}");
            assert!(
                summary.starts_with("1 row changed · committed"),
                "{summary}"
            );
        }
        _ => unreachable!(),
    }
    assert_eq!(after, "shipped");
    // The model hears that the write ran.
    assert!(text(&events).contains("committed"), "{}", text(&events));
}

#[tokio::test]
async fn error_scenario_ends_with_ai_upstream() {
    let ctx = setup(Privacy::Results, false);
    let events = chat(&ctx, "make an error", false).await;
    assert_terminal(&events);
    match events.last().unwrap() {
        ChatEvent::Error { error } => {
            assert_eq!(error.code, ErrorCode::AiUpstream);
            assert_eq!(error.message, "The AI server answered HTTP 500");
            assert!(
                error
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("mock server error"),
                "{error:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn cancel_during_slow_stream_stops_fast() {
    let ctx = setup(Privacy::Results, false);
    let chat_id = "it-slow-chat";
    let mut events = Vec::new();
    session::run(
        &ctx.st,
        request(&ctx, chat_id, "answer slow please", false),
        |e| {
            if matches!(e, ChatEvent::Text { .. })
                && !events.iter().any(|x| matches!(x, ChatEvent::Text { .. }))
            {
                session::cancel(&ctx.st, chat_id);
            }
            events.push(e);
            true
        },
    )
    .await;
    assert_terminal(&events);
    match events.last().unwrap() {
        ChatEvent::Error { error } => assert_eq!(error.code, ErrorCode::Cancelled),
        other => panic!("{other:?}"),
    }
    // The slow scenario streams twenty chunks; ending after the first is the signal that the
    // cancel stopped it, without measuring the machine's speed.
    assert_eq!(text(&events), "chunk 1 ");
    assert!(!ctx.st.chats().is_running(chat_id));
    // A cancel for an unknown or finished chat is not an error.
    session::cancel(&ctx.st, chat_id);
}

#[tokio::test]
async fn test_ai_checks_the_model() {
    let ctx = setup(Privacy::Results, false);
    let ok = settings::test(&ctx.st, ai_settings(Privacy::Results), Some(String::new()))
        .await
        .unwrap();
    assert_eq!(ok.model_count, 1, "the mock lists exactly one model");
    let wrong = AiSettings {
        model: "no-such-model".into(),
        ..ai_settings(Privacy::Results)
    };
    let e = settings::test(&ctx.st, wrong, None).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ModelNotFound);
    assert!(
        e.message
            .starts_with("Model \"no-such-model\" is not on this server."),
        "{}",
        e.message
    );
    assert!(e.detail.as_deref().unwrap().contains("mock-gpt"));
}

#[tokio::test]
async fn schema_privacy_never_sends_rows() {
    let count = {
        let ctx = setup(Privacy::Results, false);
        let r = ops::run_ai_read(&ctx.st, &ctx.conn.id, None, "SELECT count(*) FROM orders", ReadLimits::default(), )
        .await
        .unwrap();
        r.rows[0][0].clone()
    };

    for privacy in [Privacy::Schema, Privacy::Results] {
        let ctx = setup(privacy, false);
        let marker = format!("marker-{}", uuid::Uuid::new_v4().simple());
        let events = chat(&ctx, &format!("count the orders {marker}"), false).await;
        assert_eq!(events.last(), Some(&ChatEvent::Done), "{events:#?}");
        // The local UI always gets the rows.
        match tool(&events).2 {
            ChatEvent::ToolResult { rows, .. } => {
                assert_eq!(rows.as_ref().unwrap()[0][0], count);
            }
            _ => unreachable!(),
        }

        let bodies = recorded_with(&marker).await;
        assert_eq!(
            bodies.len(),
            2,
            "{privacy:?}: one call, then one after the tool"
        );
        let tools = tool_contents(&bodies);
        assert_eq!(tools.len(), 1, "{privacy:?}");
        let content = &tools[0];
        assert_eq!(content["rowCount"], 1);
        assert_eq!(content["columns"][0]["name"], "count");
        match privacy {
            Privacy::Schema => {
                let mut keys: Vec<&str> = content
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    ["columns", "note", "rowCount", "truncated"],
                    "{content}"
                );
                for b in &bodies {
                    let raw = b.to_string();
                    assert!(
                        !raw.contains("\\\"rows\\\""),
                        "rows sent in schema mode: {raw}"
                    );
                }
            }
            Privacy::Results => {
                assert_eq!(content["rows"], serde_json::json!([[count]]), "{content}");
            }
        }
    }
}

/// A failed read in `schema` privacy: the server's message quotes the row value, so the model
/// gets only the SQLSTATE; the UI keeps the full error.
#[tokio::test]
async fn schema_privacy_hides_query_error_text() {
    let sql = "SELECT (SELECT email FROM customers ORDER BY id LIMIT 1)::integer";
    let args = serde_json::json!({ "sql": sql }).to_string();
    for privacy in [Privacy::Schema, Privacy::Results] {
        let ctx = setup(privacy, false);
        let email = ops::run_ai_read(&ctx.st, &ctx.conn.id, None, "SELECT email FROM customers ORDER BY id LIMIT 1", ReadLimits::default(), )
        .await
        .unwrap()
        .rows[0][0]
            .as_str()
            .unwrap()
            .to_string();
        let ai = ai_settings(privacy);
        let sources = [Source {
            id: "db1".into(),
            conn: ctx.conn.clone(),
            database: "shop".into(),
            schema: None,
            allow_writes: false,
        }];
        let tc = tools::ToolContext {
            state: &ctx.st,
            sources: &sources,
            settings: &ai,
        };
        let out = tools::execute(&tc, "call_cast", "run_read_query", &args).await;
        match &out.events[..] {
            [
                ChatEvent::ToolResult {
                    ok: false,
                    error: Some(e),
                    ..
                },
            ] => {
                assert_eq!(e.code, ErrorCode::Database, "{e:?}");
                assert!(e.message.contains(&email), "{privacy:?}: UI error {e:?}");
            }
            other => panic!("{privacy:?}: {other:#?}"),
        }
        let content: Value = serde_json::from_str(&out.content).unwrap();
        match privacy {
            Privacy::Schema => {
                assert!(!out.content.contains(&email), "{}", out.content);
                assert_eq!(content["error"]["code"], "database");
                assert!(
                    content["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("SQLSTATE 22P02"),
                    "{content}"
                );
                assert!(content["error"].get("detail").is_none(), "{content}");
            }
            Privacy::Results => {
                assert!(out.content.contains(&email), "{}", out.content);
            }
        }
    }
}
