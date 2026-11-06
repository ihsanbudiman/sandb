use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::core::ai::session::{ChatEvent, ChatRequest, ChatRole, ChatSource, ChatTurn};
use crate::core::ai::{oauth, session, settings, tools};
use crate::core::connections::{self, ConnectionInput, Engine, Env, SslMode};
use crate::core::secrets::MemoryStore;
use crate::core::state::AppState;

#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Complete,
    EmptyTerminal,
    LateFailure,
    Incomplete,
    Disconnect,
    HeaderWait,
    StreamWait,
    Unicode,
    Schema,
    Safeguards,
}

struct Provider {
    requests: Mutex<Vec<Value>>,
    completed: AtomicUsize,
    mode: Mutex<Mode>,
    started: tokio::sync::Notify,
}

struct Fixture {
    base: String,
    provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let provider = Arc::new(Provider {
            requests: Mutex::new(Vec::new()),
            completed: AtomicUsize::new(0),
            mode: Mutex::new(Mode::Complete),
            started: tokio::sync::Notify::new(),
        });
        let app = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/responses", post(respond))
            .fallback(|| async { axum::http::StatusCode::NOT_FOUND })
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            provider,
            server,
        }
    }
}

fn auth(headers: &HeaderMap) {
    assert_eq!(
        headers["authorization"],
        "Bearer dummy-access-token-not-a-real-credential"
    );
    assert!(!headers.contains_key("x-api-key"));
}

async fn models(headers: HeaderMap) -> Json<Value> {
    auth(&headers);
    Json(json!({"models": [
        {"slug": "z-model", "display_name": "First account model", "visibility": "list"},
        {"slug": "hidden", "display_name": "Not a picker choice", "visibility": "hidden"},
        {"slug": "a-model", "display_name": "Second account model", "visibility": "list"}
    ]}))
}

fn reasoning() -> Value {
    json!({"type": "reasoning", "id": "rs_dummy", "summary": [], "encrypted_content": "dummy-opaque-reasoning"})
}

fn commentary() -> Value {
    json!({"type": "message", "id": "msg_commentary", "role": "assistant", "phase": "commentary", "status": "completed", "content": [{"type": "output_text", "text": "Checking. ", "annotations": []}]})
}

fn guarded_calls() -> Vec<Value> {
    [
        ("read", "run_read_query", "SELECT value FROM guarded"),
        ("blocked", "run_read_query", "DELETE FROM guarded"),
        ("proposal", "propose_write", "UPDATE guarded SET value = 99"),
    ].into_iter().map(|(id, name, sql)| json!({
        "type": "function_call", "id": format!("fc_{id}"), "call_id": format!("call_{id}"),
        "namespace": "sandb", "name": name, "arguments": json!({"sql": sql}).to_string(), "status": "completed"
    })).collect()
}

fn function_call(arguments: &str) -> Value {
    json!({"type": "function_call", "id": "fc_item_distinct", "call_id": "call_select", "namespace": "sandb", "name": "run_read_query", "arguments": arguments, "status": "completed"})
}

async fn respond(
    State(p): State<Arc<Provider>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    auth(&headers);
    assert_eq!(body["model"], "z-model");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    let instructions = body["instructions"]
        .as_str()
        .expect("instructions, not system items");
    assert!(instructions.contains("db1"));
    for field in [
        "messages",
        "previous_response_id",
        "temperature",
        "max_output_tokens",
        "metadata",
        "conversation",
        "tool_choice",
    ] {
        assert!(body.get(field).is_none(), "unexpected field {field}");
    }
    let ids = ["db1"];
    let expected = tools::definitions(&ids);
    let namespace = &body["tools"][0];
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(namespace["type"], "namespace");
    assert_eq!(namespace["name"], "sandb");
    for (actual, old) in namespace["tools"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
    {
        assert_eq!(actual["type"], "function");
        assert_eq!(actual["name"], old["function"]["name"]);
        assert_eq!(actual["strict"], false);
        assert_eq!(actual["parameters"], old["function"]["parameters"]);
    }
    assert_eq!(namespace["tools"].as_array().unwrap().len(), 5);
    assert!(
        namespace["tools"][0]["parameters"]
            .get("required")
            .is_none()
    );
    assert_eq!(
        namespace["tools"][0]["parameters"]["properties"]["schema"]["type"],
        "string"
    );
    assert_eq!(
        namespace["tools"][2]["parameters"]["required"],
        json!(["sql"])
    );
    assert_eq!(
        namespace["tools"][2]["parameters"]["properties"]["source"]["enum"],
        json!(["db1"])
    );
    let input = body["input"].as_array().unwrap();
    assert_eq!(
        &input[..3],
        &[
            json!({"role": "user", "content": "earlier user"}),
            json!({"role": "assistant", "content": "earlier assistant"}),
            json!({"role": "user", "content": "run the harmless query"}),
        ]
    );
    assert!(!input.iter().any(|item| item["role"] == "system"));
    let turn = {
        let mut requests = p.requests.lock().unwrap();
        requests.push(body.clone());
        requests.len()
    };
    let mode = *p.mode.lock().unwrap();
    if matches!(mode, Mode::HeaderWait) {
        p.started.notify_one();
        std::future::pending::<()>().await;
    }
    let arguments = json!({"sql": "SELECT 1 AS ok"}).to_string();
    let mut events = if matches!(mode, Mode::Safeguards) {
        if turn == 1 {
            let mut output = vec![reasoning()];
            output.extend(guarded_calls());
            vec![completed(output)]
        } else {
            assert_eq!(turn, 2);
            assert_eq!(input.len(), 10);
            assert_eq!(&input[4..7], guarded_calls());
            for (item, id) in input[7..]
                .iter()
                .zip(["call_read", "call_blocked", "call_proposal"])
            {
                assert_eq!(item["type"], "function_call_output");
                assert_eq!(item["call_id"], id);
            }
            let read: Value = serde_json::from_str(input[7]["output"].as_str().unwrap()).unwrap();
            assert_eq!(read["rows"], json!([[1]]));
            let blocked: Value =
                serde_json::from_str(input[8]["output"].as_str().unwrap()).unwrap();
            assert_eq!(blocked["error"]["code"], "guard_rejected");
            let proposal: Value =
                serde_json::from_str(input[9]["output"].as_str().unwrap()).unwrap();
            assert_eq!(proposal["status"], "pending_approval");
            let mut message = commentary();
            message["content"][0]["text"] = json!("safeguards kept");
            vec![completed(vec![message])]
        }
    } else if turn == 1 {
        assert_eq!(input.len(), 3);
        vec![
            json!({"type": "response.output_text.delta", "delta": "Checking. "}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_item_distinct", "call_id": "call_select", "namespace": "sandb", "name": "run_read_query", "arguments": "", "status": "in_progress"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_item_distinct", "delta": "{\"sql\":\"SELECT "}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "item_id": "fc_item_distinct", "delta": "1 AS ok\"}"}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": function_call(&arguments)}),
            json!({"type": "response.completed", "response": {"id": "resp_first", "status": "completed", "output": [reasoning(), commentary(), function_call(&arguments)]}}),
        ]
    } else {
        assert_eq!(turn, 2);
        assert_eq!(input.len(), 7);
        assert_eq!(input[3], reasoning());
        assert_eq!(input[4], commentary());
        assert_eq!(input[5], function_call(&arguments));
        assert_eq!(input[6]["type"], "function_call_output");
        assert_eq!(input[6]["call_id"], "call_select");
        let output: Value = serde_json::from_str(input[6]["output"].as_str().unwrap()).unwrap();
        assert_eq!(output["columns"].as_array().unwrap().len(), 1);
        assert_eq!(output["columns"][0]["name"], "ok");
        if matches!(mode, Mode::Schema) {
            assert!(output.get("rows").is_none());
            assert_eq!(output["rowCount"], 1);
        } else {
            assert_eq!(output["rows"], json!([[1]]));
        }
        vec![
            json!({"type": "response.output_text.delta", "delta": "ok=1"}),
            json!({"type": "response.output_text.done", "text": "ok=1"}),
            json!({"type": "response.completed", "response": {"id": "resp_final", "status": "completed", "output": [{"type": "message", "id": "msg_final", "role": "assistant", "phase": "final_answer", "status": "completed", "content": [{"type": "output_text", "text": "ok=1", "annotations": []}]}]}}),
        ]
    };
    match mode {
        Mode::EmptyTerminal => {
            let mut terminal = events.pop().unwrap();
            let output = terminal["response"]["output"].as_array().unwrap().clone();
            let mut streamed = Vec::new();
            for (index, item) in output.iter().enumerate() {
                let mut added = item.clone();
                if added.get("status").is_some() {
                    added["status"] = json!("in_progress");
                }
                if item["type"] == "function_call" {
                    added["arguments"] = json!("");
                }
                if item["type"] == "message" {
                    added["content"] = json!([]);
                }
                streamed.push(json!({"type": "response.output_item.added", "output_index": index, "item": added}));
            }
            streamed.extend(events.into_iter().filter(|event| {
                matches!(
                    event["type"].as_str(),
                    Some("response.output_text.delta" | "response.output_text.done")
                )
            }));
            for (index, item) in output.into_iter().enumerate() {
                streamed.push(json!({"type": "response.output_item.done", "output_index": index, "item": item}));
            }
            terminal["response"]["output"] = json!([]);
            streamed.push(terminal);
            events = streamed;
        }
        Mode::Unicode => {
            let mut message = commentary();
            message["content"][0]["text"] = json!("Café 🦀");
            events = vec![
                json!({"type": "response.output_text.delta", "delta": "Café 🦀"}),
                completed(vec![message]),
            ];
        }
        Mode::LateFailure => {
            *events.last_mut().unwrap() = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "dummy-access-token-not-a-real-credential quota"}}});
        }
        Mode::Incomplete => {
            *events.last_mut().unwrap() =
                json!({"type": "response.incomplete", "response": {"status": "incomplete"}})
        }
        Mode::Disconnect | Mode::StreamWait => {
            events.pop();
        }
        _ => {}
    }
    let chunks = events.into_iter().flat_map(move |event| {
        let terminal = event["type"] == "response.completed";
        let bytes = if matches!(mode, Mode::LateFailure) && event["type"] == "response.failed" {
            let json = serde_json::to_string_pretty(&event).unwrap();
            format!(
                "event: response.failed\r\n{}\r\n",
                json.lines()
                    .map(|line| format!("data: {line}\r\n"))
                    .collect::<String>()
            )
            .into_bytes()
        } else {
            format!(
                "event: {}\r\ndata: {event}\r\n\r\n",
                event["type"].as_str().unwrap()
            )
            .into_bytes()
        };
        if matches!(mode, Mode::Unicode) {
            let last = bytes.len() - 1;
            bytes
                .into_iter()
                .enumerate()
                .map(|(index, byte)| (vec![byte], terminal && index == last))
                .collect()
        } else {
            let split = bytes.len() / 2;
            vec![
                (bytes[..split].to_vec(), false),
                (bytes[split..].to_vec(), terminal),
            ]
        }
    });
    let provider = p.clone();
    let stream = futures_util::stream::iter(chunks).map(move |(bytes, terminal)| {
        if terminal {
            provider.completed.fetch_add(1, Ordering::SeqCst);
        }
        Ok::<_, std::io::Error>(bytes)
    });
    let stalled = p.clone();
    let stream = stream.chain(futures_util::stream::once(async move {
        if matches!(mode, Mode::StreamWait) {
            stalled.started.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(Vec::new())
    }));
    use futures_util::StreamExt;
    (
        [("content-type", "text/event-stream")],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

async fn state(
    fixture: &Fixture,
    oauth: &oauth::tests::Fixture,
) -> (tempfile::TempDir, AppState, ChatRequest) {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    state.chatgpt = oauth.session();
    state.chatgpt.inference_base = Some(fixture.base.clone());
    state.chatgpt.sign_in(state.secrets.as_ref()).await.unwrap();
    oauth.finish_browser().await;
    settings::set_key(&state, Some("dummy-api-key-must-never-be-used".into())).unwrap();
    let mode = *fixture.provider.mode.lock().unwrap();
    settings::save(
        &state,
        settings::AiSettings {
            provider: "chatgpt".into(),
            base_url: format!("{}/ignored-base", fixture.base),
            model: "z-model".into(),
            privacy: if matches!(mode, Mode::Schema) {
                settings::Privacy::Schema
            } else {
                settings::Privacy::Results
            },
            ..Default::default()
        },
    )
    .unwrap();
    let file = dir.path().join("harmless.db");
    std::fs::write(&file, b"").unwrap();
    if matches!(mode, Mode::Safeguards) {
        use sqlx::Connection;
        let mut database = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&file),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE guarded (value INTEGER)")
            .execute(&mut database)
            .await
            .unwrap();
        sqlx::query("INSERT INTO guarded VALUES (1)")
            .execute(&mut database)
            .await
            .unwrap();
        database.close().await.unwrap();
    }
    let conn = connections::create(
        &state,
        ConnectionInput {
            name: "dummy sqlite".into(),
            engine: Engine::Sqlite,
            env: Env::Local,
            host: String::new(),
            port: 0,
            database: "main".into(),
            user: String::new(),
            ssl: SslMode::Disable,
            file: file.to_string_lossy().into_owned(),
            ai_write: matches!(mode, Mode::Safeguards),
            read_only: !matches!(mode, Mode::Safeguards),
            version: None,
        },
        None,
    )
    .unwrap();
    let request = ChatRequest {
        chat_id: "responses-proof".into(),
        sources: vec![ChatSource {
            conn_id: conn.id,
            database: None,
            schema: None,
            allow_writes: matches!(mode, Mode::Safeguards),
        }],
        history: vec![
            ChatTurn {
                role: ChatRole::User,
                content: "earlier user".into(),
            },
            ChatTurn {
                role: ChatRole::Assistant,
                content: "earlier assistant".into(),
            },
        ],
        message: "run the harmless query".into(),
    };
    (dir, state, request)
}

#[tokio::test]
async fn oauth_responses_replays_sqlite_tool_output_and_completes_the_existing_session_loop() {
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        if matches!(event, ChatEvent::ToolCall { .. }) {
            assert!(fixture.provider.completed.load(Ordering::SeqCst) >= 1);
        }
        if matches!(event, ChatEvent::Done) {
            assert_eq!(fixture.provider.completed.load(Ordering::SeqCst), 2);
        }
        events.push(event);
        true
    })
    .await;
    assert!(events.iter().any(|event| matches!(event, ChatEvent::ToolResult { id, ok: true, columns: Some(c), rows: Some(r), .. } if id == "call_select" && c == &["ok"] && r == &vec![vec![json!(1)]])), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ChatEvent::Done))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, ChatEvent::Error { .. })),
        "{events:?}"
    );
    let text = events
        .iter()
        .filter_map(|e| {
            if let ChatEvent::Text { delta } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect::<String>();
    assert_eq!(text, "Checking. ok=1");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("dummy-opaque-reasoning")
    );
    assert_eq!(settings::load(&state).unwrap().provider, "chatgpt");
    assert_eq!(
        settings::models(
            &state,
            settings::load(&state).unwrap(),
            Some("dummy-input-api-key".into())
        )
        .await
        .unwrap(),
        vec!["z-model", "a-model"]
    );
    let catalog = settings::chatgpt_catalog(&state).await.unwrap();
    assert_eq!(
        catalog
            .iter()
            .map(|model| (model.slug.as_str(), model.display_name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("z-model", "First account model"),
            ("a-model", "Second account model")
        ]
    );
}

#[tokio::test]
async fn empty_terminal_output_replays_completed_tools_and_finishes_the_session() {
    let fixture = Fixture::start().await;
    *fixture.provider.mode.lock().unwrap() = Mode::EmptyTerminal;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        if matches!(event, ChatEvent::ToolCall { .. }) {
            assert!(fixture.provider.completed.load(Ordering::SeqCst) >= 1);
        }
        events.push(event);
        true
    })
    .await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ChatEvent::Error { .. })),
        "{events:?}"
    );
    assert!(events.iter().any(|event| matches!(event, ChatEvent::ToolResult { id, ok: true, rows: Some(rows), .. } if id == "call_select" && rows == &vec![vec![json!(1)]])), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ChatEvent::Done))
            .count(),
        1
    );
    assert_eq!(fixture.provider.completed.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.requests.lock().unwrap().len(), 2);
    let text = events
        .iter()
        .filter_map(|event| {
            if let ChatEvent::Text { delta } = event {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect::<String>();
    assert_eq!(text, "Checking. ok=1");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("dummy-opaque-reasoning")
    );
}

#[tokio::test]
async fn late_quota_failure_after_complete_tool_arguments_never_executes_tools_or_emits_done() {
    let fixture = Fixture::start().await;
    *fixture.provider.mode.lock().unwrap() = Mode::LateFailure;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        events.push(event);
        true
    })
    .await;
    assert!(!events.iter().any(|event| matches!(
        event,
        ChatEvent::ToolCall { .. } | ChatEvent::ToolResult { .. } | ChatEvent::Done
    )));
    let errors = events
        .iter()
        .filter_map(|event| {
            if let ChatEvent::Error { error } = event {
                Some(error)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, crate::core::error::ErrorCode::AiUpstream);
    assert_eq!(
        errors[0].server_code.as_deref(),
        Some("subscription_sharing_usage_limit_exceeded")
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("dummy-access-token-not-a-real-credential")
    );
    assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unfinished_responses_never_execute_provisional_tools_or_report_success() {
    for mode in [Mode::Incomplete, Mode::Disconnect] {
        let fixture = Fixture::start().await;
        *fixture.provider.mode.lock().unwrap() = mode;
        let oauth = oauth::tests::Fixture::start().await;
        let (_dir, state, request) = state(&fixture, &oauth).await;
        let mut events = Vec::new();
        session::run(&state, request, |event| {
            events.push(event);
            true
        })
        .await;
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ChatEvent::ToolCall { .. } | ChatEvent::ToolResult { .. } | ChatEvent::Done
            )),
            "{events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ChatEvent::Error { .. }))
                .count(),
            1
        );
        assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn logout_and_user_cancel_stop_header_wait_and_stream_without_tools_or_done() {
    for mode in [Mode::HeaderWait, Mode::StreamWait] {
        for logout in [false, true] {
            let fixture = Fixture::start().await;
            *fixture.provider.mode.lock().unwrap() = mode;
            let oauth = oauth::tests::Fixture::start().await;
            let (_dir, state, request) = state(&fixture, &oauth).await;
            let mut events = Vec::new();
            {
                let chat = session::run(&state, request, |event| {
                    events.push(event);
                    true
                });
                tokio::pin!(chat);
                tokio::select! {
                    _ = fixture.provider.started.notified() => {},
                    _ = &mut chat => panic!("chat ended before cancellation"),
                }
                if logout {
                    state.chatgpt.logout(state.secrets.as_ref()).await.unwrap();
                } else {
                    session::cancel(&state, "responses-proof");
                }
                tokio::time::timeout(std::time::Duration::from_secs(2), chat)
                    .await
                    .unwrap();
            }
            assert!(!state.chats.is_running("responses-proof"));
            assert!(!events.iter().any(|event| matches!(
                event,
                ChatEvent::ToolCall { .. } | ChatEvent::ToolResult { .. } | ChatEvent::Done
            )));
            assert!(
                matches!(events.last(), Some(ChatEvent::Error { error }) if error.code == crate::core::error::ErrorCode::Cancelled),
                "{events:?}"
            );
            assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn cancelled_tool_iteration_never_executes_or_reuses_completed_output() {
    for sign_out in [false, true] {
        let fixture = Fixture::start().await;
        let oauth = oauth::tests::Fixture::start().await;
        let (_dir, state, request) = state(&fixture, &oauth).await;
        let mut events = Vec::new();
        let mut logout = Box::pin(state.chatgpt.logout(state.secrets.as_ref()));
        session::run(&state, request, |event| {
            if matches!(event, ChatEvent::ToolCall { .. }) {
                if sign_out {
                    let mut context =
                        std::task::Context::from_waker(futures_util::task::noop_waker_ref());
                    assert!(std::future::Future::poll(logout.as_mut(), &mut context).is_pending());
                } else {
                    session::cancel(&state, "responses-proof");
                }
            }
            events.push(event);
            true
        })
        .await;
        if sign_out {
            logout.await.unwrap();
        }
        assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ChatEvent::ToolResult { .. } | ChatEvent::Done))
        );
        assert!(
            matches!(events.last(), Some(ChatEvent::Error { error }) if error.code == crate::core::error::ErrorCode::Cancelled)
        );
    }
}

#[tokio::test]
async fn oauth_failure_never_falls_back_to_saved_api_key_or_base_url() {
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    state.chatgpt.logout(state.secrets.as_ref()).await.unwrap();
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        events.push(event);
        true
    })
    .await;
    assert!(
        matches!(events.as_slice(), [ChatEvent::Error { error }] if error.code == crate::core::error::ErrorCode::AuthFailed)
    );
    assert!(
        settings::models(
            &state,
            settings::load(&state).unwrap(),
            Some("dummy-input-api-key".into())
        )
        .await
        .is_err()
    );
    assert!(
        settings::test(
            &state,
            settings::load(&state).unwrap(),
            Some("dummy-input-api-key".into())
        )
        .await
        .is_err()
    );
    assert!(fixture.provider.requests.lock().unwrap().is_empty());
    assert_eq!(
        settings::stored_key(&state).unwrap().as_deref(),
        Some("dummy-api-key-must-never-be-used")
    );
}

#[tokio::test]
async fn chatgpt_logout_does_not_cancel_an_api_key_chat() {
    use crate::core::ai::client::tests::{done, finish_chunk, head, serve, text_chunk};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let (base, mut requests) = serve(vec![
        head("200 OK", "text/event-stream"),
        text_chunk("api-key answer"),
        finish_chunk("stop"),
        done(),
    ])
    .await;
    let mut config = settings::load(&state).unwrap();
    config.provider = "openai".into();
    config.base_url = base;
    settings::save(&state, config).unwrap();
    let mut events = Vec::new();
    let mut logout = Box::pin(state.chatgpt.logout(state.secrets.as_ref()));
    session::run(&state, request, |event| {
        if matches!(event, ChatEvent::Text { .. }) {
            let mut context = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
            assert!(std::future::Future::poll(logout.as_mut(), &mut context).is_pending());
        }
        events.push(event);
        true
    })
    .await;
    logout.await.unwrap();
    assert!(matches!(events.last(), Some(ChatEvent::Done)), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ChatEvent::Error { .. }))
    );
    let raw = requests.recv().await.unwrap();
    assert!(raw.contains("authorization: Bearer dummy-api-key-must-never-be-used"));
    assert!(fixture.provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn prestream_http_errors_preserve_status_request_id_and_redacted_body_shape() {
    use crate::core::ai::client::tests::serve;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, mut state, _) = state(&fixture, &oauth).await;
    for (status, code) in [
        (401, "subscription_sharing_invalid_user"),
        (403, "subscription_sharing_user_not_eligible"),
        (429, "subscription_sharing_usage_limit_exceeded"),
        (503, "subscription_sharing_usage_unavailable"),
    ] {
        let body = json!({"error": {"code": code, "param": "model", "message": "dummy-access-token-not-a-real-credential refused"}, "extra": ["opaque", 7]});
        let (base, mut requests) = serve(vec![(0, format!("HTTP/1.1 {status} Failure\r\ncontent-type: application/json\r\nx-request-id: req-dummy-access-token-not-a-real-credential\r\nconnection: close\r\n\r\n{body}"))]).await;
        state.chatgpt.inference_base = Some(base);
        let error = settings::models(&state, settings::load(&state).unwrap(), None)
            .await
            .unwrap_err();
        let serialized = serde_json::to_value(&error).unwrap();
        assert_eq!(serialized["http_status"], status);
        assert_eq!(serialized["request_id"], "req-***");
        assert_eq!(serialized["upstream_code"], code);
        assert_eq!(serialized["upstream_param"], "model");
        assert_eq!(
            serialized["upstream_body"]["error"]["message"],
            "*** refused"
        );
        assert_eq!(serialized["upstream_body"]["extra"], json!(["opaque", 7]));
        assert!(
            !serialized
                .to_string()
                .contains("dummy-access-token-not-a-real-credential")
        );
        assert!(
            state
                .chatgpt
                .status(state.secrets.as_ref())
                .unwrap()
                .signed_in
        );
        assert!(
            requests
                .recv()
                .await
                .unwrap()
                .contains("Bearer dummy-access-token-not-a-real-credential")
        );
    }
}

#[tokio::test]
async fn every_diagnostic_field_redacts_oauth_tokens_and_saved_or_supplied_api_keys() {
    use crate::core::ai::client::tests::serve;
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    for secret in [
        "dummy-access-token-not-a-real-credential".to_owned(),
        "dummy-refresh-token-not-a-real-credential".to_owned(),
        oauth.issued_id_token(),
        "dummy-api-key-must-never-be-used".to_owned(),
        "dummy-input-api-key".to_owned(),
    ] {
        let error = json!({"code": format!("code-{secret}"), "param": secret, "message": secret});
        let body = json!({"error": error, "nested": [{secret.clone(): secret}]});
        let (base, _) = serve(vec![(0, format!("HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\nx-request-id: {secret}\r\nconnection: close\r\n\r\n{body}"))]).await;
        let client = settings::client(
            &state,
            &settings::load(&state).unwrap(),
            Some("dummy-input-api-key".into()),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .with_test_base(&base);
        let error = client.list_models().await.unwrap_err();
        assert!(
            !format!("{error:?}").contains(&secret),
            "diagnostics leaked a credential"
        );
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["request_id"], "***");
        assert_eq!(value["upstream_code"], "code-***");
        assert_eq!(value["upstream_param"], "***");
        assert_eq!(value["upstream_body"]["nested"][0]["***"], "***");
    }
}

#[tokio::test]
async fn streamed_errors_keep_http200_request_id_code_param_and_original_event_shape() {
    use crate::core::ai::client::tests::{serve, sse};
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    for event in [
        json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "subscription_sharing_usage_limit_exceeded", "param": "model", "message": "quota dummy-refresh-token-not-a-real-credential"}}}),
        json!({"type": "error", "code": "subscription_sharing_usage_limit_exceeded", "param": "model", "message": "quota dummy-refresh-token-not-a-real-credential"}),
    ] {
        let (base, _) = serve(vec![(0, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-request-id: req-stream\r\nconnection: close\r\n\r\n".into()), sse(event.clone())]).await;
        let client = settings::client(
            &state,
            &settings::load(&state).unwrap(),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .with_test_base(&base);
        let error = client
            .chat_stream(&json!({}), &CancellationToken::new(), |_| true)
            .await
            .unwrap_err();
        let body = serde_json::to_value(&error).unwrap();
        assert_eq!(body["http_status"], 200);
        assert_eq!(body["request_id"], "req-stream");
        assert_eq!(
            body["upstream_code"],
            "subscription_sharing_usage_limit_exceeded"
        );
        assert_eq!(body["upstream_param"], "model");
        assert_eq!(body["upstream_body"]["type"], event["type"]);
        assert!(!format!("{error:?}").contains("dummy-refresh-token-not-a-real-credential"));
    }
    for raw in ["region not available", "{\"detail\":\"route unavailable\"}"] {
        let (base, _) = serve(vec![(
            0,
            format!("HTTP/1.1 503 Unavailable\r\nconnection: close\r\n\r\n{raw}"),
        )])
        .await;
        let client = settings::client(
            &state,
            &settings::load(&state).unwrap(),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .with_test_base(&base);
        let error = client.list_models().await.unwrap_err();
        let expected = serde_json::from_str(raw).unwrap_or(json!(raw));
        assert_eq!(error.upstream_body, Some(expected));
        assert_eq!(error.http_status, Some(503));
        assert!(
            state
                .chatgpt
                .status(state.secrets.as_ref())
                .unwrap()
                .signed_in
        );
    }
}

async fn wire(
    state: &AppState,
    parts: Vec<crate::core::ai::client::tests::Part>,
) -> (
    Result<crate::core::ai::client::Completion, crate::core::error::AppError>,
    String,
) {
    use tokio_util::sync::CancellationToken;
    let (base, _) = crate::core::ai::client::tests::serve(parts).await;
    let client = settings::client(
        state,
        &settings::load(state).unwrap(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .with_test_base(&base)
    .with_idle_timeout(std::time::Duration::from_millis(200));
    let mut text = String::new();
    let result = client
        .chat_stream(&json!({}), &CancellationToken::new(), |delta| {
            text.push_str(delta);
            true
        })
        .await;
    (result, text)
}

fn completed(output: Vec<Value>) -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_matrix", "status": "completed", "output": output}})
}

#[tokio::test]
async fn empty_terminal_output_preserves_fully_done_items_at_dense_and_sparse_indices() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    for item in [
        commentary(),
        function_call("{\"sql\":\"SELECT 1\"}"),
        reasoning(),
    ] {
        for index in [0, 7] {
            let mut added = item.clone();
            if added.get("status").is_some() {
                added["status"] = json!("in_progress");
            }
            if item["type"] == "message" {
                added["content"] = json!([]);
            }
            if item["type"] == "function_call" {
                added["arguments"] = json!("");
            }
            let mut parts = vec![
                head("200 OK", "text/event-stream"),
                sse(
                    json!({"type": "response.output_item.added", "output_index": index, "item": added}),
                ),
            ];
            if item["type"] == "message" {
                parts.push(sse(json!({"type": "response.output_text.delta", "output_index": index, "content_index": 0, "delta": "Checking. "})));
                parts.push(sse(json!({"type": "response.output_text.done", "output_index": index, "content_index": 0, "text": "Checking. "})));
            }
            if item["type"] == "function_call" {
                parts.push(sse(json!({"type": "response.function_call_arguments.delta", "output_index": index, "item_id": item["id"], "delta": item["arguments"]})));
            }
            parts.push(sse(json!({"type": "response.output_item.done", "output_index": index, "item": item.clone()})));
            parts.push(sse(completed(vec![])));
            let (result, text) = wire(&state, parts).await;
            let result = result.unwrap();
            assert_eq!(result.output, vec![item.clone()]);
            assert_eq!(result.finish_reason.as_deref(), Some("completed"));
            assert_eq!(
                result.tool_calls.len(),
                usize::from(item["type"] == "function_call")
            );
            assert_eq!(
                text,
                if item["type"] == "message" {
                    "Checking. "
                } else {
                    ""
                }
            );
        }
    }
}

#[tokio::test]
async fn provisional_output_error_reproduces_after_visible_greeting() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let mut message = commentary();
    message["content"][0]["text"] = json!("Halo!");
    for (output_index, output, rejected) in [
        (0, vec![message.clone()], false),
        (0, vec![], true),
        (1, vec![message.clone()], true),
    ] {
        let (result, text) = wire(&state, vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"type": "response.output_item.added", "output_index": output_index, "item": message.clone()})),
            sse(json!({"type": "response.output_text.delta", "delta": "Halo!"})),
            sse(completed(output)),
        ]).await;
        assert_eq!(text, "Halo!", "text is visible before terminal validation");
        if rejected {
            let error = result.unwrap_err();
            assert_eq!(error.code, crate::core::error::ErrorCode::AiProtocol);
            assert_eq!(error.message, "ChatGPT omitted provisional output");
        } else {
            let result = result.unwrap();
            assert_eq!(result.text, "Halo!");
            assert!(result.tool_calls.is_empty());
        }
    }
}

#[tokio::test]
async fn empty_terminal_output_rejects_unfinished_or_contradictory_streams() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let message = commentary();
    let added =
        json!({"type": "response.output_item.added", "output_index": 0, "item": message.clone()});
    let done =
        json!({"type": "response.output_item.done", "output_index": 0, "item": message.clone()});
    let pending = json!({"type": "response.output_item.added", "output_index": 1, "item": function_call("{\"sql\":\"SELECT 1\"}")});
    let mut changed = message.clone();
    changed["content"][0]["text"] = json!("changed");
    let mut missing = completed(vec![]);
    missing["response"]
        .as_object_mut()
        .unwrap()
        .remove("output");
    let mut null = missing.clone();
    null["response"]["output"] = Value::Null;
    let mut failed = completed(vec![]);
    failed["response"]["status"] = json!("failed");
    let mut error = completed(vec![]);
    error["response"]["error"] = json!({"code": "failed"});
    for events in [
        vec![added.clone(), completed(vec![])],
        vec![added.clone(), done.clone(), pending, completed(vec![])],
        vec![added.clone(), done.clone()],
        vec![
            added.clone(),
            done.clone(),
            json!({"type": "response.incomplete", "response": {"status": "incomplete"}}),
        ],
        vec![added.clone(), done.clone(), failed],
        vec![added.clone(), done.clone(), error],
        vec![added.clone(), done.clone(), missing],
        vec![added.clone(), done.clone(), null],
        vec![added.clone(), done.clone(), completed(vec![changed])],
        vec![
            added,
            json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "wrong"}),
            done,
            completed(vec![]),
        ],
    ] {
        let mut parts = vec![head("200 OK", "text/event-stream")];
        parts.extend(events.into_iter().map(sse));
        let (result, _) = wire(&state, parts).await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol)
        );
    }
}

#[tokio::test]
async fn contradictory_provisional_function_calls_cannot_be_hidden_by_completed_output() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let added =
        json!({"type": "response.output_item.added", "output_index": 0, "item": function_call("")});
    for provisional in [
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_item_distinct", "delta": 42}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "wrong-item", "delta": "{}"}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_item_distinct", "delta": "{\"sql\":\"DELETE FROM t\"}"}),
    ] {
        let (result, _) = wire(
            &state,
            vec![
                head("200 OK", "text/event-stream"),
                sse(added.clone()),
                sse(provisional),
                sse(completed(vec![function_call("{\"sql\":\"SELECT 1\"}")])),
            ],
        )
        .await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol),
            "contradictory provisional call was accepted"
        );
    }
}

#[tokio::test]
async fn malformed_or_unsupported_outputs_are_protocol_errors_at_the_public_client() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let valid = function_call("{\"sql\":\"SELECT 1\"}");
    let mut missing_call = valid.clone();
    missing_call.as_object_mut().unwrap().remove("call_id");
    let mut foreign = valid.clone();
    foreign["namespace"] = json!("foreign");
    let mut unknown = valid.clone();
    unknown["name"] = json!("execute_shell");
    let mut nonobject = valid.clone();
    nonobject["arguments"] = json!("[]");
    let mut badargs = valid.clone();
    badargs["arguments"] = json!("{");
    let mut duplicate = valid.clone();
    duplicate["id"] = json!("fc_other");
    let mut duplicate_item = valid.clone();
    duplicate_item["call_id"] = json!("call_other");
    let mut badcontent = commentary();
    badcontent["content"][0]["text"] = json!(17);
    let mut badreasoning = reasoning();
    badreasoning["encrypted_content"] = json!({"unexpected": true});
    let mut unfinished = valid.clone();
    unfinished["status"] = json!("incomplete");
    let too_many = (0..65)
        .map(|i| {
            let mut call = valid.clone();
            call["id"] = json!(format!("fc_{i}"));
            call["call_id"] = json!(format!("call_{i}"));
            call
        })
        .collect();
    let mut events = vec![
        completed(vec![missing_call]),
        completed(vec![valid.clone(), duplicate]),
        completed(vec![valid.clone(), duplicate_item]),
        completed(vec![foreign]),
        completed(vec![unknown]),
        completed(vec![nonobject]),
        completed(vec![badargs]),
        completed(vec![badcontent]),
        completed(vec![badreasoning]),
        completed(vec![unfinished]),
        completed(vec![
            json!({"id": "web", "type": "web_search_call", "status": "completed"}),
        ]),
        completed(too_many),
        completed(vec![reasoning(); 257]),
        json!({"type": "response.output_text.delta", "delta": 17}),
        json!({"type": "response.output_text.delta", "output_index": -1, "delta": "x"}),
        json!({"type": "response.output_item.added", "output_index": 256, "item": valid}),
        json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "output": {}}}),
        json!({"type": "response.completed", "response": {"id": "r", "status": "failed", "output": []}}),
    ];
    for (number, event) in events.drain(..).enumerate() {
        let (result, _) = wire(
            &state,
            vec![head("200 OK", "text/event-stream"), sse(event)],
        )
        .await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol),
            "accepted invalid output #{number}"
        );
    }
    for raw in ["data: {not-json}\r\n\r\n", "data: [DONE]\n\n"] {
        let (result, _) = wire(
            &state,
            vec![head("200 OK", "text/event-stream"), (0, raw.into())],
        )
        .await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol)
        );
    }
}

#[tokio::test]
async fn valid_multiple_calls_and_unknown_optional_fields_are_retained_without_loss() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let mut first = function_call("{\"sql\":\"SELECT 1\"}");
    first["future_optional"] = json!({"keep": [1, true]});
    first["created_by"] = json!("output-only-provenance");
    let mut second = first.clone();
    second["id"] = json!("fc_second");
    second["call_id"] = json!("call_second");
    let mut added = first.clone();
    added["arguments"] = json!("");
    added["status"] = json!("in_progress");
    let parts = vec![
        head("200 OK", "text/event-stream"),
        sse(json!({"type": "response.future_optional_event", "anything": true})),
        sse(json!({"type": "response.output_item.added", "output_index": 1, "item": added})),
        sse(
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_item_distinct", "delta": "{\"sql\":"}),
        ),
        sse(
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_item_distinct", "delta": "\"SELECT 1\"}"}),
        ),
        sse(
            json!({"type": "response.function_call_arguments.done", "output_index": 1, "item_id": "fc_item_distinct", "arguments": "{\"sql\":\"SELECT 1\"}"}),
        ),
        sse(json!({"type": "response.output_item.done", "output_index": 1, "item": first.clone()})),
        sse(completed(vec![reasoning(), first.clone(), second.clone()])),
    ];
    let (result, text) = wire(&state, parts).await;
    let result = result.unwrap();
    assert!(text.is_empty());
    assert_eq!(
        result
            .tool_calls
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["call_select", "call_second"]
    );
    first.as_object_mut().unwrap().remove("created_by");
    second.as_object_mut().unwrap().remove("created_by");
    assert_eq!(result.output, vec![reasoning(), first, second]);
}

#[tokio::test]
async fn completed_text_events_cannot_disagree_with_deltas_or_terminal_content() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let mut message = commentary();
    message["content"][0]["text"] = json!("safe");
    for (delta, done, final_text) in [
        ("safe", "different", "safe"),
        ("safe", "safe", "safe but changed"),
    ] {
        message["content"][0]["text"] = json!(final_text);
        let (result, _) = wire(&state, vec![head("200 OK", "text/event-stream"),
            sse(json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": delta})),
            sse(json!({"type": "response.output_text.done", "output_index": 0, "content_index": 0, "text": done})),
            sse(completed(vec![message.clone()])),
        ]).await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol),
            "accepted inconsistent completed text"
        );
    }
}

#[tokio::test]
async fn split_utf8_crlf_and_sse_frames_preserve_exact_text() {
    let fixture = Fixture::start().await;
    *fixture.provider.mode.lock().unwrap() = Mode::Unicode;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        events.push(event);
        true
    })
    .await;
    let text = events
        .iter()
        .filter_map(|event| {
            if let ChatEvent::Text { delta } = event {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect::<String>();
    assert_eq!(text, "Café 🦀");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ChatEvent::Done))
            .count(),
        1
    );
}

#[tokio::test]
async fn schema_privacy_keeps_local_rows_but_hides_them_from_responses_tool_output() {
    let fixture = Fixture::start().await;
    *fixture.provider.mode.lock().unwrap() = Mode::Schema;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        events.push(event);
        true
    })
    .await;
    assert!(events.iter().any(|event| matches!(event, ChatEvent::ToolResult { rows: Some(rows), ok: true, .. } if rows == &vec![vec![json!(1)]])));
    assert!(matches!(events.last(), Some(ChatEvent::Done)), "{events:?}");
    let requests = fixture.provider.requests.lock().unwrap();
    let output: Value =
        serde_json::from_str(requests[1]["input"][6]["output"].as_str().unwrap()).unwrap();
    assert!(output.get("rows").is_none());
    assert_eq!(output["rowCount"], 1);
}

#[tokio::test]
async fn responses_tools_keep_read_guard_and_write_approval_without_changing_sqlite() {
    let fixture = Fixture::start().await;
    *fixture.provider.mode.lock().unwrap() = Mode::Safeguards;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, request) = state(&fixture, &oauth).await;
    let conn_id = request.sources[0].conn_id.clone();
    let mut events = Vec::new();
    session::run(&state, request, |event| {
        events.push(event);
        true
    })
    .await;
    assert!(matches!(events.last(), Some(ChatEvent::Done)), "{events:?}");
    assert!(events.iter().any(|event| matches!(event, ChatEvent::ToolResult { id, ok: false, error: Some(error), .. } if id == "call_blocked" && error.code == crate::core::error::ErrorCode::GuardRejected)));
    assert!(events.iter().any(|event| matches!(event, ChatEvent::WritePreview { id, sql, .. } if id == "call_proposal" && sql == "UPDATE guarded SET value = 99")));
    let result = crate::core::db::readonly::run_ai_read(
        &state,
        &conn_id,
        None,
        "SELECT value FROM guarded",
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.rows, vec![vec![json!(1)]]);
    assert_eq!(fixture.provider.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn responses_respect_header_idle_and_receiver_gone_limits() {
    use crate::core::ai::client::tests::{head, serve, sse};
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    for parts in [
        vec![(1000, String::new())],
        vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"type": "response.output_text.delta", "delta": "first"})),
            (1000, String::new()),
        ],
    ] {
        let start = std::time::Instant::now();
        let (result, _) = wire(&state, parts).await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::Timeout)
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
    let (base, _) = serve(vec![
        head("200 OK", "text/event-stream"),
        sse(json!({"type": "response.output_text.delta", "delta": "first"})),
        (1000, String::new()),
    ])
    .await;
    let client = settings::client(
        &state,
        &settings::load(&state).unwrap(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .with_test_base(&base);
    let error = client
        .chat_stream(&json!({}), &CancellationToken::new(), |_| false)
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::core::error::ErrorCode::Cancelled);
}

#[tokio::test]
async fn oversized_error_bodies_cannot_leak_a_secret_cut_at_the_read_limit() {
    use crate::core::ai::client::tests::serve;
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let body = format!(
        "{}dummy-refresh-token-not-a-real-credential",
        "x".repeat(64 * 1024 - 10)
    );
    let (base, _) = serve(vec![(
        0,
        format!("HTTP/1.1 503 Unavailable\r\nconnection: close\r\n\r\n{body}"),
    )])
    .await;
    let client = settings::client(
        &state,
        &settings::load(&state).unwrap(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .with_test_base(&base);
    let error = client.list_models().await.unwrap_err();
    assert_eq!(error.http_status, Some(503));
    assert_eq!(
        error.upstream_body,
        Some(json!("[AI error body exceeded 64 KiB]"))
    );
}

#[tokio::test]
async fn json_escaped_api_keys_are_redacted_from_raw_diagnostic_text_too() {
    use crate::core::ai::client::tests::serve;
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let secret = "dummy-\"quoted-key\\tail";
    settings::set_key(&state, Some(secret.into())).unwrap();
    let body = json!({"error": {"code": secret, "message": secret, "param": secret}});
    let (base, _) = serve(vec![(
        0,
        format!("HTTP/1.1 503 Unavailable\r\nconnection: close\r\n\r\n{body}"),
    )])
    .await;
    let client = settings::client(
        &state,
        &settings::load(&state).unwrap(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .with_test_base(&base);
    let error = client.list_models().await.unwrap_err();
    let escaped = serde_json::to_string(secret).unwrap();
    assert!(
        !error
            .detail
            .as_deref()
            .unwrap()
            .contains(&escaped[1..escaped.len() - 1])
    );
    assert_eq!(error.upstream_body.unwrap()["error"]["message"], "***");
}

#[tokio::test]
async fn responses_bound_lines_multiline_events_text_and_total_stream_bytes() {
    use crate::core::ai::client::tests::{head, sse};
    let fixture = Fixture::start().await;
    let oauth = oauth::tests::Fixture::start().await;
    let (_dir, state, _) = state(&fixture, &oauth).await;
    let long = "x".repeat(8 * 1024 * 1024 + 1);
    let half = "x".repeat(4 * 1024 * 1024);
    let optional = sse(
        json!({"type": "response.future_optional_event", "payload": "x".repeat(7 * 1024 * 1024)}),
    );
    let mut total = vec![head("200 OK", "text/event-stream")];
    total.extend(std::iter::repeat_n(optional, 5));
    for parts in [
        vec![
            head("200 OK", "text/event-stream"),
            (0, format!("data: {long}\n\n")),
        ],
        vec![
            head("200 OK", "text/event-stream"),
            (0, format!("data: {half}\ndata: {half}\n\n")),
        ],
        vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"type": "response.output_text.delta", "delta": long})),
        ],
        vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"type": "response.output_text.delta", "delta": half.clone()})),
            sse(json!({"type": "response.output_text.delta", "delta": half})),
            sse(json!({"type": "response.output_text.delta", "delta": "x"})),
        ],
        total,
    ] {
        let (result, _) = wire(&state, parts).await;
        assert!(
            matches!(result, Err(error) if error.code == crate::core::error::ErrorCode::AiProtocol)
        );
    }
}
