//! Phase 5 integration tests: the chat loop on MongoDB sources against `mock-llm`, the Mongo
//! replica set `shop_staging` database and (for the mixed chat) the pg16 `shop` database, logged
//! in as `sandb_rw`. Needs the sandb-test docker stack (`pnpm db:up`) and a mock LLM with the
//! Mongo scenarios at `SANDB_MOCK_LLM_URL` (default `http://127.0.0.1:58080/v1`).
//! Unreachable services FAIL; nothing is skipped. Scratch collections are named `ai_it_*` and
//! dropped before the asserts.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;

use mongodb::bson::{Document, doc};
use serde_json::Value;

use sandb_lib::core::ai::session::{self, ChatEvent, ChatRequest, ChatRole, ChatSource, ChatTurn};
use sandb_lib::core::ai::settings::{self, AiSettings, Privacy};
use sandb_lib::core::connections::{self, Connection, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::db::readonly::ReadLimits;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

const MONGO_RS: u16 = 57017;
const MONGO_STANDALONE: u16 = 57018;
const PG16: u16 = 55416;
const MONGO_DB: &str = "shop_staging";

/// Every chat builds the prompt's schema snapshot of `shop_staging`, which samples `big_docs`
/// (about 200 MB). Parallel samples run the small test Mongo out of memory, so the tests take
/// turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn mock_url() -> String {
    std::env::var("SANDB_MOCK_LLM_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:58080/v1".into())
        .trim_end_matches('/')
        .to_string()
}

fn env_test() -> HashMap<String, String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdb/.env.test");
    let raw =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    raw.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

struct Ctx {
    _dir: tempfile::TempDir,
    st: AppState,
    /// The Mongo connection (`shop_staging`).
    mongo: Connection,
}

fn ai_settings(privacy: Privacy) -> AiSettings {
    AiSettings {
        provider: "mock".into(),
        base_url: mock_url(),
        model: "mock-gpt".into(),
        privacy,
        ..Default::default()
    }
}

fn input(name: &str, engine: Engine, port: u16, database: &str) -> ConnectionInput {
    let e = env_test();
    let mongo = engine == Engine::Mongo;
    ConnectionInput {
        name: name.into(),
        engine,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: database.into(),
        user: e.get("SANDB_RW_USER").cloned().unwrap_or("sandb_rw".into()),
        ssl: if mongo {
            SslMode::Disable
        } else {
            SslMode::Prefer
        },
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: if mongo { "admin".into() } else { String::new() },
        topology: None,
    }
}

fn password() -> String {
    env_test()
        .get("SANDB_RW_PASSWORD")
        .cloned()
        .expect("SANDB_RW_PASSWORD in testdb/.env.test")
}

/// The stack's root user at `port`, whose roles reach every command: `sandb_rw` cannot run
/// `serverStatus`, so the read-command sweep needs root.
fn root(port: u16) -> ConnectionInput {
    let mut input = input("it-ai-mongo-root", Engine::Mongo, port, MONGO_DB);
    input.name = format!("it-ai-mongo-root-{}", uuid::Uuid::new_v4().simple());
    input.user = "root".into();
    input
}

fn root_password() -> String {
    env_test()
        .get("MONGO_INITDB_ROOT_PASSWORD")
        .cloned()
        .expect("MONGO_INITDB_ROOT_PASSWORD in testdb/.env.test")
}

fn setup(privacy: Privacy) -> Ctx {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    let mongo = connections::create(
        &st,
        input("it-ai-mongo", Engine::Mongo, MONGO_RS, MONGO_DB),
        Some(password()),
    )
    .unwrap();
    settings::save(&st, ai_settings(privacy)).unwrap();
    Ctx {
        _dir: dir,
        st,
        mongo,
    }
}

fn source(conn: &Connection, allow_writes: bool) -> ChatSource {
    ChatSource {
        conn_id: conn.id.clone(),
        database: None,
        schema: None,
        allow_writes,
    }
}

/// Runs one chat on `sources`; every write preview is answered with `approve`.
async fn chat_on(
    ctx: &Ctx,
    sources: Vec<ChatSource>,
    message: &str,
    approve: bool,
) -> Vec<ChatEvent> {
    let chat_id = uuid::Uuid::new_v4().to_string();
    let mut events = Vec::new();
    session::run(
        &ctx.st,
        ChatRequest {
            chat_id: chat_id.clone(),
            sources,
            history: vec![ChatTurn {
                role: ChatRole::Assistant,
                content: "Hello, how can I help?".into(),
            }],
            message: message.into(),
        },
        |e| {
            if let ChatEvent::WritePreview { id, .. } = &e {
                ctx.st.approvals.decide(&chat_id, id, approve).unwrap();
            }
            events.push(e);
            true
        },
    )
    .await;
    assert!(!ctx.st.chats.is_running(&chat_id), "registry entry left");
    events
}

/// A chat on the Mongo source only.
async fn chat(ctx: &Ctx, message: &str, allow_writes: bool, approve: bool) -> Vec<ChatEvent> {
    chat_on(
        ctx,
        vec![source(&ctx.mongo, allow_writes)],
        message,
        approve,
    )
    .await
}

/// Exactly one terminal event, it is the last one, and it is `Done`.
fn assert_done(events: &[ChatEvent]) {
    let terminal = events
        .iter()
        .filter(|e| matches!(e, ChatEvent::Done | ChatEvent::Error { .. }))
        .count();
    assert_eq!(terminal, 1, "{events:#?}");
    assert_eq!(events.last(), Some(&ChatEvent::Done), "{events:#?}");
}

fn text(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

/// Each tool call with its result, in call order.
fn tools(events: &[ChatEvent]) -> Vec<(&str, &Value, &ChatEvent)> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::ToolCall { id, name, args } => Some((id, name, args)),
            _ => None,
        })
        .map(|(id, name, args)| {
            let result = events
                .iter()
                .find(|e| matches!(e, ChatEvent::ToolResult { id: rid, .. } if rid == id))
                .unwrap_or_else(|| panic!("no tool_result for {id}: {events:#?}"));
            (name.as_str(), args, result)
        })
        .collect()
}

/// The single tool call and its result.
fn tool(events: &[ChatEvent]) -> (&str, &Value, &ChatEvent) {
    let all = tools(events);
    assert_eq!(all.len(), 1, "{events:#?}");
    all[0]
}

/// A fresh scratch collection name, `ai_it_` + 32 lowercase hex characters.
fn scratch() -> String {
    format!("ai_it_{}", uuid::Uuid::new_v4().simple())
}

async fn client(ctx: &Ctx) -> mongodb::Client {
    mongo::client::client(&ctx.st, &ctx.mongo.id).await.unwrap()
}

async fn exists(client: &mongodb::Client, coll: &str) -> bool {
    client
        .database(MONGO_DB)
        .list_collection_names()
        .filter(doc! { "name": coll })
        .await
        .unwrap()
        .contains(&coll.to_string())
}

async fn count(client: &mongodb::Client, coll: &str) -> u64 {
    client
        .database(MONGO_DB)
        .collection::<Document>(coll)
        .count_documents(doc! {})
        .await
        .unwrap()
}

/// Drops the scratch names a test made when the test ends, also while a failed test unwinds.
/// `Drop` cannot use the test's client or runtime (both are being torn down after a panic), so it
/// opens its own root client on its own thread.
struct Cleanup {
    uri: String,
    collections: Vec<String>,
}

impl Cleanup {
    /// A guard for scratch collections on the server at `port`.
    fn new(port: u16) -> Self {
        let password = env_test()
            .get("MONGO_INITDB_ROOT_PASSWORD")
            .cloned()
            .expect("MONGO_INITDB_ROOT_PASSWORD in testdb/.env.test");
        Self {
            uri: format!("mongodb://root:{password}@127.0.0.1:{port}/admin"),
            collections: Vec::new(),
        }
    }

    /// Tracks `coll` and returns it.
    fn collection(&mut self, coll: impl Into<String>) -> String {
        let coll = coll.into();
        self.collections.push(coll.clone());
        coll
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.collections.is_empty() {
            return;
        }
        let uri = self.uri.clone();
        let collections = std::mem::take(&mut self.collections);
        let joined = std::thread::Builder::new()
            .name("it-mongo-cleanup".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                rt.block_on(async move {
                    let Ok(client) = mongodb::Client::with_uri_str(uri).await else {
                        return;
                    };
                    for coll in collections {
                        let _ = client
                            .database(MONGO_DB)
                            .collection::<Document>(&coll)
                            .drop()
                            .await;
                    }
                });
            })
            .map(|h| h.join());
        let _ = joined;
    }
}

/// Drops a scratch collection; one that was never created is fine.
async fn drop_scratch(client: &mongodb::Client, coll: &str) {
    if let Err(e) = client
        .database(MONGO_DB)
        .collection::<Document>(coll)
        .drop()
        .await
    {
        let e = AppError::from(e);
        assert_eq!(
            e.server_code.as_deref(),
            Some("MongoDB error 26"), // NamespaceNotFound
            "drop {coll}: {e:?}"
        );
    }
}

/// Request bodies the mock recorded whose user messages contain `marker`.
async fn recorded_with(marker: &str) -> Vec<Value> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = mock_url();
    let root = root.strip_suffix("/v1").unwrap_or(&root);
    let bytes = reqwest::get(format!("{root}/_requests"))
        .await
        .expect("mock /_requests")
        .error_for_status()
        .expect("mock /_requests status (is the mock up to date?)")
        .bytes()
        .await
        .unwrap();
    let all: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    all.into_iter()
        .filter(|b| {
            b["messages"].as_array().is_some_and(|m| {
                m.iter().any(|m| {
                    m["role"] == "user" && m["content"].as_str().is_some_and(|c| c.contains(marker))
                })
            })
        })
        .collect()
}

/// The `tool` message contents of the last recorded request.
fn tool_contents(bodies: &[Value]) -> Vec<Value> {
    bodies
        .last()
        .and_then(|b| b["messages"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| serde_json::from_str(m["content"].as_str().unwrap()).unwrap())
        .collect()
}

/// The tool names offered in the first recorded request.
fn offered_tools(bodies: &[Value]) -> Vec<String> {
    bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn mongo_read_runs_and_sends_documents() {
    let _serial = SERIAL.lock().await;
    for privacy in [Privacy::Results, Privacy::Schema] {
        let ctx = setup(privacy);
        let marker = format!("marker-{}", uuid::Uuid::new_v4().simple());
        let events = chat(&ctx, &format!("mongo read {marker}"), false, false).await;
        assert_done(&events);
        let (name, args, result) = tool(&events);
        assert_eq!(name, "run_mongo_read");
        assert!(
            args["query"]
                .as_str()
                .unwrap()
                .starts_with("db.orders.find("),
            "{args}"
        );
        let ChatEvent::ToolResult {
            ok,
            summary,
            columns,
            rows,
            truncated,
            ..
        } = result
        else {
            unreachable!()
        };
        assert!(ok, "{result:?}");
        assert!(summary.starts_with("5 rows · "), "{summary}");
        let columns = columns.as_deref().unwrap();
        for c in ["_id", "status", "total"] {
            assert!(columns.iter().any(|x| x == c), "{c}: {columns:?}");
        }
        let status = columns.iter().position(|c| c == "status").unwrap();
        let rows = rows.as_ref().unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| r[status] == "paid"), "{rows:?}");
        assert_eq!(*truncated, Some(false));

        let bodies = recorded_with(&marker).await;
        assert_eq!(
            bodies.len(),
            2,
            "{privacy:?}: one call, then one after the tool"
        );
        let offered = offered_tools(&bodies);
        for t in [
            "run_mongo_read",
            "render_mongo_chart",
            "propose_mongo_write",
        ] {
            assert!(offered.iter().any(|x| x == t), "{t}: {offered:?}");
        }
        for t in ["run_read_query", "render_chart", "propose_write"] {
            assert!(!offered.iter().any(|x| x == t), "{t}: {offered:?}");
        }
        let contents = tool_contents(&bodies);
        assert_eq!(contents.len(), 1);
        let content = &contents[0];
        assert_eq!(content["rowCount"], 5, "{content}");
        match privacy {
            Privacy::Results => {
                let docs = content["documents"].as_array().expect("documents");
                assert_eq!(docs.len(), 5, "{content}");
                assert!(docs.iter().all(|d| d["status"] == "paid"), "{content}");
                assert!(content.get("rows").is_none(), "{content}");
            }
            Privacy::Schema => {
                assert!(content.get("documents").is_none(), "{content}");
                assert!(content.get("rows").is_none(), "{content}");
                assert!(!content.to_string().contains("paid"), "{content}");
            }
        }
    }
}

#[tokio::test]
async fn mongo_chart_has_numeric_y() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Schema);
    let events = chat(&ctx, "mongo chart please", false, false).await;
    assert_done(&events);
    let (name, _, result) = tool(&events);
    assert_eq!(name, "render_mongo_chart");
    match result {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{result:?}");
            assert!(summary.starts_with("Bar chart · 5 rows"), "{summary}");
        }
        _ => unreachable!(),
    }
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
    assert_eq!(spec.x, "_id");
    assert_eq!(rows.len(), 5);
    assert!(!truncated);
    let col = |n: &str| {
        columns
            .iter()
            .position(|c| c == n)
            .unwrap_or_else(|| panic!("{n}: {columns:?}"))
    };
    let (x, orders, revenue) = (col("_id"), col("orders"), col("revenue"));
    let mut total = 0.0;
    for r in rows {
        assert!(r[x].is_string(), "{r:?}");
        // Int32 counts and the Decimal128 sums both reach the chart as JSON numbers.
        total += r[orders]
            .as_f64()
            .unwrap_or_else(|| panic!("orders: {r:?}"));
        assert!(r[revenue].as_f64().is_some(), "revenue: {r:?}");
    }
    let docs = count(&client(&ctx).await, "orders").await;
    assert_eq!(total as u64, docs);
    // `schema` privacy: the user sees the values, the model only names and counts.
    let t = text(&events);
    assert!(t.contains("\"rowCount\""), "{t}");
    assert!(!t.contains("shipped"), "{t}");
}

/// The preview comes before the result and shows the mongosh text.
fn preview_then_result<'a>(events: &'a [ChatEvent], coll: &str) -> &'a ChatEvent {
    assert_done(events);
    let (name, args, result) = tool(events);
    assert_eq!(name, "propose_mongo_write");
    let query = format!("db.{coll}.insertOne({{item: \"mock\", qty: 1}})");
    assert_eq!(args["query"], query.as_str());
    let preview = events
        .iter()
        .position(|e| matches!(e, ChatEvent::WritePreview { .. }))
        .expect("write_preview");
    let result_at = events.iter().position(|e| e == result).unwrap();
    assert!(preview < result_at, "preview must come before the result");
    match &events[preview] {
        ChatEvent::WritePreview { sql, info, .. } => {
            assert_eq!(sql, &query);
            assert_eq!(info.statements[0].kind, "insertOne", "{info:?}");
        }
        _ => unreachable!(),
    }
    result
}

#[tokio::test]
async fn an_approved_mongo_write_commits() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Results);
    let client = client(&ctx).await;
    let mut clean = Cleanup::new(MONGO_RS);
    let coll = clean.collection(scratch());
    let events = chat(&ctx, &format!("mongo write {coll}"), true, true).await;
    let docs = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    match preview_then_result(&events, &coll) {
        ChatEvent::ToolResult { ok, summary, .. } => {
            assert!(ok, "{summary}");
            assert!(
                summary.starts_with("1 row changed · committed"),
                "{summary}"
            );
        }
        _ => unreachable!(),
    }
    assert_eq!(docs, 1);
    assert!(text(&events).contains("committed"), "{}", text(&events));
}

#[tokio::test]
async fn a_denied_mongo_write_runs_nothing() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Results);
    let client = client(&ctx).await;
    let mut clean = Cleanup::new(MONGO_RS);
    let coll = clean.collection(scratch());
    let events = chat(&ctx, &format!("mongo write {coll}"), true, false).await;
    let created = exists(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    match preview_then_result(&events, &coll) {
        ChatEvent::ToolResult { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::WriteDenied);
        }
        _ => unreachable!(),
    }
    assert!(!created, "a denied write created {coll}");
}

#[tokio::test]
async fn a_mongo_write_with_write_mode_off_is_blocked() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Results);
    let client = client(&ctx).await;
    let mut clean = Cleanup::new(MONGO_RS);
    let coll = clean.collection(scratch());
    let events = chat(&ctx, &format!("mongo write {coll}"), false, true).await;
    let created = exists(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_done(&events);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::WritePreview { .. })),
        "{events:#?}"
    );
    let (name, _, result) = tool(&events);
    assert_eq!(name, "propose_mongo_write");
    match result {
        ChatEvent::ToolResult { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::WriteBlocked);
        }
        _ => unreachable!(),
    }
    assert!(!created, "a blocked write created {coll}");
}

#[tokio::test]
async fn out_stage_is_refused_by_the_guard() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Results);
    let client = client(&ctx).await;
    let mut clean = Cleanup::new(MONGO_RS);
    let coll = clean.collection(scratch());
    let events = chat(&ctx, &format!("mongo out {coll}"), true, true).await;
    let created = exists(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_done(&events);
    let (name, args, result) = tool(&events);
    assert_eq!(name, "run_mongo_read");
    assert!(args["query"].as_str().unwrap().contains("$out"), "{args}");
    match result {
        ChatEvent::ToolResult {
            ok, error, summary, ..
        } => {
            assert!(!ok);
            let e = error.as_ref().unwrap();
            assert_eq!(e.code, ErrorCode::GuardRejected, "{e:?}");
            assert_eq!(summary, &e.message);
        }
        _ => unreachable!(),
    }
    assert!(!created, "$out wrote {coll}");
    assert!(text(&events).contains("guard_rejected"), "{events:#?}");
}

#[tokio::test]
async fn mixed_chat_runs_each_call_on_its_own_source() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Results);
    let pg = connections::create(
        &ctx.st,
        input("it-ai-mixed-pg16", Engine::Pg, PG16, "shop"),
        Some(password()),
    )
    .unwrap();
    let marker = format!("marker-{}", uuid::Uuid::new_v4().simple());
    let events = chat_on(
        &ctx,
        vec![source(&pg, false), source(&ctx.mongo, false)],
        &format!("mixed {marker}"),
        false,
    )
    .await;
    assert_done(&events);
    let calls = tools(&events);
    assert_eq!(calls.len(), 2, "{events:#?}");

    let (name, args, result) = calls[0];
    assert_eq!(name, "run_read_query");
    assert_eq!(args["source"], "db1");
    let ChatEvent::ToolResult {
        ok, columns, rows, ..
    } = result
    else {
        unreachable!()
    };
    assert!(ok, "{result:?}");
    assert_eq!(columns.as_deref(), Some(&["orders".to_string()][..]));
    let pg_count = rows.as_ref().unwrap()[0][0].clone();

    let (name, args, result) = calls[1];
    assert_eq!(name, "run_mongo_read");
    assert_eq!(args["source"], "db2");
    let ChatEvent::ToolResult { ok, rows, .. } = result else {
        unreachable!()
    };
    assert!(ok, "{result:?}");
    let docs = count(&client(&ctx).await, "orders").await;
    let row = &rows.as_ref().unwrap()[0];
    assert!(
        row.iter()
            .any(|c| c.as_u64() == Some(docs) || c.as_str() == Some(&docs.to_string())),
        "countDocuments {docs}: {row:?}"
    );

    // Both results reach the model, in order, and the model offered tools for both engines.
    let t = text(&events);
    assert!(t.starts_with("Mixed results received: "), "{t}");
    let bodies = recorded_with(&marker).await;
    assert_eq!(bodies.len(), 3, "one call per step");
    let offered = offered_tools(&bodies);
    for tn in ["run_read_query", "run_mongo_read"] {
        assert!(offered.iter().any(|x| x == tn), "{tn}: {offered:?}");
    }
    let contents = tool_contents(&bodies);
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["rows"], serde_json::json!([[pg_count]]));
    assert!(contents[1].get("documents").is_some(), "{}", contents[1]);
}

/// The AI read puts `maxTimeMS` into an allow-listed read `runCommand`, and both local servers
/// accept the field for every command of the guard's read list: the replica set and the
/// standalone. The list mirrors `READ_COMMANDS` in `core/mongo/guard.rs`. A command that
/// rejected the field would appear in the failure list, and `core/mongo/ai.rs` would need a
/// skip list for it.
#[tokio::test]
async fn read_run_commands_accept_the_ai_max_time() {
    let _serial = SERIAL.lock().await;
    let ctx = setup(Privacy::Schema);
    let commands = [
        "db.runCommand({ ping: 1 })",
        "db.runCommand({ buildInfo: 1 })",
        "db.runCommand({ hello: 1 })",
        "db.runCommand({ isMaster: 1 })",
        "db.adminCommand({ serverStatus: 1 })",
        "db.runCommand({ hostInfo: 1 })",
        "db.runCommand({ connectionStatus: 1 })",
        "db.runCommand({ dbStats: 1 })",
        "db.runCommand({ collStats: 'orders' })",
        "db.adminCommand({ listDatabases: 1 })",
        "db.runCommand({ listCollections: 1 })",
        "db.runCommand({ listIndexes: 'orders' })",
        "db.adminCommand({ getParameter: '*' })",
        "db.adminCommand({ getCmdLineOpts: 1 })",
        "db.adminCommand({ currentOp: 1 })",
        "db.adminCommand({ connPoolStats: 1 })",
        "db.adminCommand({ top: 1 })",
        "db.adminCommand({ listCommands: 1 })",
        "db.runCommand({ whatsmyuri: 1 })",
        "db.runCommand({ explain: { find: 'orders' } })",
    ];
    let mut failures = Vec::new();
    for port in [MONGO_RS, MONGO_STANDALONE] {
        let conn = connections::create(&ctx.st, root(port), Some(root_password())).unwrap();
        for text in commands {
            let r =
                mongo::ai::read(&ctx.st, &conn, Some(MONGO_DB), text, ReadLimits::default()).await;
            match r {
                Ok(r) if !r.rows.is_empty() => {}
                Ok(r) => failures.push(format!("{port} {text}: no rows ({r:?})")),
                Err(e) => failures.push(format!("{port} {text}: {e:?}")),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
