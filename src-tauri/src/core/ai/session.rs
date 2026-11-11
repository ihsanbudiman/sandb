//! The chat loop: model call, tool calls, tool results, and again, until the model answers
//! with text only. Events stream to the UI; every run ends with exactly one `done` or `error`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::core::ai::approval::APPROVAL_TIMEOUT;
use crate::core::ai::client::{AiClient, Completion, StreamEvent};
use crate::core::ai::prompt::{self, PromptInput};
use crate::core::ai::settings;
use crate::core::ai::tools::{self, Decision, ToolContext};
use crate::core::connections::{self, Engine};
use crate::core::db::introspect;
use crate::core::error::{AppError, ErrorCode};
use crate::core::guard::WriteInfo;
use crate::core::state::AppState;

/// Model calls in one `chat_send`.
pub const MAX_STEPS: usize = 8;
/// Databases in one chat. Each adds its schema summary to the prompt.
pub const MAX_SOURCES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

/// An earlier turn of the chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

/// One database the chat can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSource {
    pub conn_id: String,
    /// `None`: the connection's own database.
    #[serde(default)]
    pub database: Option<String>,
    /// PG schema for `list_tables` and `describe_table`; `None` is `public`.
    #[serde(default)]
    pub schema: Option<String>,
    /// The chat's write mode for this database. `propose_write` also needs a connection that
    /// is not read-only.
    #[serde(default)]
    pub allow_writes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    /// Key for `chat_cancel`.
    pub chat_id: String,
    /// 1 to `MAX_SOURCES` databases. The model names them `db1`, `db2`, … in this order.
    pub sources: Vec<ChatSource>,
    /// Earlier turns, oldest first, without `message`.
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    pub message: String,
}

/// One streamed event. JSON: `{"type": "text", "delta": "…"}` and so on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Text {
        delta: String,
    },
    /// The model has begun a tool call; its arguments are still streaming. Once per call,
    /// before that call's `tool_call`.
    ToolStarting {
        name: String,
    },
    ToolCall {
        id: String,
        name: String,
        /// The parsed arguments; `{}` when they are not a JSON object.
        args: Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        /// Short text for the UI: `12 tables`, `5 rows · 42 ms`, or the error message.
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        columns: Option<Vec<String>>,
        /// At most `tools::UI_ROWS` rows, for the local UI only.
        #[serde(skip_serializing_if = "Option::is_none")]
        rows: Option<Vec<Vec<Value>>>,
        /// More rows exist than `rows` holds.
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<AppError>,
    },
    /// A proposed write, sent before its `tool_result`. Nothing has run: the `tool_result`
    /// comes after the user approves (`approve_write`) or denies (`deny_write`) it.
    WritePreview {
        id: String,
        sql: String,
        info: WriteInfo,
    },
    /// The chart of a `render_chart` read, sent before its `tool_result`. For the local UI only;
    /// at most `tools::CHART_ROWS` rows.
    Chart {
        id: String,
        spec: tools::ChartSpec,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        /// More rows exist than `rows` holds.
        truncated: bool,
    },
    Error {
        error: AppError,
    },
    Done,
}

/// Running chats by id, for `chat_cancel`.
#[derive(Default)]
pub struct ChatRegistry {
    inner: Mutex<Running>,
}

#[derive(Default)]
struct Running {
    next: u64,
    chats: HashMap<String, (u64, CancellationToken)>,
}

/// Removes its chat from the registry when dropped, unless a newer run took the id.
struct Entry<'a> {
    registry: &'a ChatRegistry,
    id: String,
    generation: u64,
    token: CancellationToken,
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        let mut r = self.registry.lock();
        if r.chats
            .get(&self.id)
            .is_some_and(|(g, _)| *g == self.generation)
        {
            r.chats.remove(&self.id);
        }
    }
}

impl ChatRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Running> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a run. A run that still holds the same id is cancelled.
    fn start(&self, id: &str) -> Entry<'_> {
        let token = CancellationToken::new();
        let mut r = self.lock();
        r.next += 1;
        let generation = r.next;
        if let Some((_, old)) = r.chats.insert(id.to_string(), (generation, token.clone())) {
            old.cancel();
        }
        Entry {
            registry: self,
            id: id.to_string(),
            generation,
            token,
        }
    }

    /// Stops a running chat. An unknown id is not an error.
    pub fn cancel(&self, id: &str) {
        if let Some((_, token)) = self.lock().chats.get(id) {
            token.cancel();
        }
    }

    /// Stops every running chat, e.g. when the page that shows them reloads.
    pub fn cancel_all(&self) {
        for (_, token) in self.lock().chats.values() {
            token.cancel();
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.lock().chats.contains_key(id)
    }
}

/// Stops a running chat; its stream ends with a `cancelled` error.
pub fn cancel(state: &AppState, chat_id: &str) {
    state.chats.cancel(chat_id);
}

/// Runs one user message to the end. `emit` returns false when the receiver is gone; the run
/// then stops as on a cancel. Setup errors also end as an `error` event.
pub async fn run(state: &AppState, req: ChatRequest, mut emit: impl FnMut(ChatEvent) -> bool) {
    let entry = state.chats.start(&req.chat_id);
    let res = converse(state, &req, &entry.token, &mut emit).await;
    drop(entry);
    emit(match res {
        Ok(()) => ChatEvent::Done,
        Err(error) => ChatEvent::Error { error },
    });
}

fn cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Stopped")
}

fn receiver_gone() -> AppError {
    AppError::new(ErrorCode::Cancelled, "The chat was closed")
}

fn push_message(body: &mut Value, message: Value) {
    if let Some(list) = body["messages"].as_array_mut() {
        list.push(message);
    }
}

async fn converse(
    state: &AppState,
    req: &ChatRequest,
    token: &CancellationToken,
    emit: &mut impl FnMut(ChatEvent) -> bool,
) -> Result<(), AppError> {
    if req.message.trim().is_empty() {
        return Err(AppError::invalid("The message is empty"));
    }
    if req.sources.is_empty() {
        return Err(AppError::invalid(
            "Choose at least one database for the chat",
        ));
    }
    if req.sources.len() > MAX_SOURCES {
        return Err(AppError::invalid(format!(
            "A chat can use at most {MAX_SOURCES} databases"
        )));
    }
    let mut sources = Vec::with_capacity(req.sources.len());
    for (i, s) in req.sources.iter().enumerate() {
        let conn = connections::get(state, &s.conn_id)?;
        let database = connections::target_database(&conn, s.database.as_deref());
        if conn.engine == Engine::Mongo && database.is_empty() {
            return Err(AppError::invalid(format!(
                "Choose a database for {}",
                conn.name
            )));
        }
        sources.push(tools::Source {
            id: format!("db{}", i + 1),
            database,
            schema: s
                .schema
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            allow_writes: s.allow_writes,
            conn,
        });
    }
    let settings = settings::load(state)?;
    if settings.model.trim().is_empty() {
        return Err(AppError::invalid("Choose an AI model in Settings first"));
    }
    let client = AiClient::new(
        settings.api(),
        &settings.base_url,
        settings::stored_key(state)?,
    )?;

    // The summaries are a help, not a need: on an error or a slow server the model uses the tools.
    let limit = Duration::from_secs(settings.timeout_sec);
    let reads = sources.iter().map(|s| async move {
        tokio::time::timeout(
            limit,
            introspect::schema_snapshot(state, &s.conn.id, &s.database),
        )
        .await
        .ok()
        .and_then(Result::ok)
    });
    let snapshots = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(cancelled()),
        r = futures_util::future::join_all(reads) => r,
    };
    let today = chrono::Local::now()
        .format("%Y-%m-%d (%A), time zone UTC%:z")
        .to_string();
    let system = prompt::system_prompt(&PromptInput {
        sources: &sources,
        snapshots: &snapshots,
        settings: &settings,
        today: &today,
    });

    let mut body = json!({
        "model": settings.model,
        "messages": [{"role": "system", "content": system}],
        "stream": true,
        "tools": tools::definitions(&sources),
        "tool_choice": "auto",
    });
    for turn in &req.history {
        push_message(
            &mut body,
            json!({"role": turn.role, "content": turn.content}),
        );
    }
    push_message(&mut body, json!({"role": "user", "content": req.message}));

    let ctx = ToolContext {
        state,
        sources: &sources,
        settings: &settings,
    };
    for _ in 0..MAX_STEPS {
        let answer: Completion = client
            .chat_stream(&body, token, |ev| {
                emit(match ev {
                    StreamEvent::Text(delta) => ChatEvent::Text {
                        delta: delta.to_string(),
                    },
                    StreamEvent::ToolStarting(name) => ChatEvent::ToolStarting {
                        name: name.to_string(),
                    },
                })
            })
            .await?;
        if answer.tool_calls.is_empty() {
            return Ok(());
        }
        let calls: Vec<Value> = answer
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments},
                })
            })
            .collect();
        let text = (!answer.text.is_empty()).then_some(answer.text.as_str());
        push_message(
            &mut body,
            json!({"role": "assistant", "content": text, "tool_calls": calls}),
        );
        for call in &answer.tool_calls {
            let args = tools::parse_args(&call.arguments)
                .map(Value::Object)
                .unwrap_or_else(|| json!({}));
            if !emit(ChatEvent::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                args,
            }) {
                return Err(receiver_gone());
            }
            let out = tokio::select! {
                biased;
                _ = token.cancelled() => return Err(cancelled()),
                out = tools::execute(&ctx, &call.id, &call.name, &call.arguments) => out,
            };
            // Registered before the preview goes out, so a fast click finds it.
            let mut waiting = out
                .write
                .as_ref()
                .map(|_| state.approvals.wait(&req.chat_id, &call.id));
            for ev in out.events {
                if !emit(ev) {
                    return Err(receiver_gone());
                }
            }
            let content = match (out.write, waiting.as_mut()) {
                (Some(w), Some(waiting)) => {
                    let decision = tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(cancelled()),
                        d = tokio::time::timeout(APPROVAL_TIMEOUT, waiting.decision()) => match d {
                            Ok(true) => Decision::Approved,
                            Ok(false) => Decision::Denied,
                            Err(_) => Decision::TimedOut,
                        },
                    };
                    let done = tools::finish_write(&ctx, &call.id, &w, decision, token).await;
                    for ev in done.events {
                        if !emit(ev) {
                            return Err(receiver_gone());
                        }
                    }
                    done.content
                }
                _ => out.content,
            };
            drop(waiting);
            push_message(
                &mut body,
                json!({"role": "tool", "tool_call_id": call.id, "content": content}),
            );
        }
    }
    Err(AppError::new(
        ErrorCode::AiProtocol,
        format!("The assistant stopped after {MAX_STEPS} steps without an answer"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ai::client::tests::{done, finish_chunk, head, serve, sse, text_chunk};
    use crate::core::ai::settings::AiSettings;
    use crate::core::connections::{ConnectionInput, Engine, Env, SslMode};
    use crate::core::secrets::MemoryStore;

    fn state_with(base: &str) -> (tempfile::TempDir, AppState, String) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        settings::save(
            &st,
            AiSettings {
                base_url: base.into(),
                model: "m".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let db = dir.path().join("t.db");
        // An empty file is an empty SQLite database.
        std::fs::write(&db, b"").unwrap();
        let conn = connections::create(
            &st,
            ConnectionInput {
                name: "t".into(),
                engine: Engine::Sqlite,
                env: Env::Local,
                host: String::new(),
                port: 0,
                database: "main".into(),
                user: String::new(),
                ssl: SslMode::Disable,
                file: db.to_string_lossy().into_owned(),
                ai_write: false,
                read_only: false,
                version: None,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            None,
        )
        .unwrap();
        (dir, st, conn.id)
    }

    /// A SQLite connection whose table `t` has `rows` rows.
    async fn sqlite_conn(st: &AppState, dir: &std::path::Path, name: &str, rows: u32) -> String {
        use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
        use sqlx::{Connection as _, Executor as _};
        let file = dir.join(format!("{name}.db"));
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        (&mut c)
            .execute(sqlx::raw_sql(&format!(
                "CREATE TABLE t (id INTEGER PRIMARY KEY); \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows}) \
                 INSERT INTO t SELECT i FROM n"
            )))
            .await
            .unwrap();
        c.close().await.unwrap();
        connections::create(
            st,
            ConnectionInput {
                name: name.into(),
                engine: Engine::Sqlite,
                env: Env::Local,
                host: String::new(),
                port: 0,
                database: "main".into(),
                user: String::new(),
                ssl: SslMode::Disable,
                file: file.to_string_lossy().into_owned(),
                ai_write: false,
                read_only: false,
                version: None,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            None,
        )
        .unwrap()
        .id
    }

    /// One chat, two connections: each call runs on the database it names, and the prompt and
    /// the tools tell the model the ids.
    #[tokio::test]
    async fn each_tool_call_runs_on_the_source_it_names() {
        let call = |id: &str, args: Value| json!({"index": 0, "id": id, "function": {"name": "run_read_query", "arguments": args.to_string()}});
        let (base, mut reqs) = serve(vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                call("a", json!({"sql": "SELECT id FROM t", "source": "db1"})),
            ]}}]})),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 1, "id": "b", "function": {"name": "run_read_query", "arguments": json!({"sql": "SELECT id FROM t", "source": "db2"}).to_string()}},
            ]}}]})),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 2, "id": "c", "function": {"name": "run_read_query", "arguments": json!({"sql": "SELECT id FROM t"}).to_string()}},
            ]}, "finish_reason": "tool_calls"}]})),
            done(),
        ])
        .await;
        let (dir, st, _) = state_with(&base);
        let small = sqlite_conn(&st, dir.path(), "small", 3).await;
        let big = sqlite_conn(&st, dir.path(), "big", 5).await;
        let mut req = request(&small, "compare");
        req.sources.push(ChatSource {
            conn_id: big,
            database: None,
            schema: None,
            allow_writes: false,
        });
        let events = collect(&st, req).await;
        let summary = |id: &str| {
            events.iter().find_map(|e| match e {
                ChatEvent::ToolResult {
                    id: i, ok, summary, ..
                } if i == id => Some((*ok, summary.clone())),
                _ => None,
            })
        };
        let (ok, s) = summary("a").unwrap();
        assert!(ok && s.starts_with("3 rows"), "{s}");
        let (ok, s) = summary("b").unwrap();
        assert!(ok && s.starts_with("5 rows"), "{s}");
        // Without a source the call fails; it never falls back to the first database.
        let (ok, s) = summary("c").unwrap();
        assert!(
            !ok && s == "The tool needs \"source\", one of: db1, db2",
            "{s}"
        );

        let raw = reqs.recv().await.unwrap();
        let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.contains("Source db1:\n- Connection: \"small\""),
            "{system}"
        );
        assert!(
            system.contains("Source db2:\n- Connection: \"big\""),
            "{system}"
        );
        assert!(system.contains("Schema summary of db2"), "{system}");
        let read = &body["tools"][2]["function"]["parameters"];
        assert_eq!(read["properties"]["source"]["enum"], json!(["db1", "db2"]));
    }

    fn request(conn_id: &str, message: &str) -> ChatRequest {
        ChatRequest {
            chat_id: "chat-1".into(),
            sources: vec![ChatSource {
                conn_id: conn_id.into(),
                database: None,
                schema: None,
                allow_writes: false,
            }],
            history: vec![ChatTurn {
                role: ChatRole::User,
                content: "earlier".into(),
            }],
            message: message.into(),
        }
    }

    async fn collect(st: &AppState, req: ChatRequest) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        run(st, req, |e| {
            events.push(e);
            true
        })
        .await;
        events
    }

    #[test]
    fn events_serialize_with_snake_case_type() {
        let v = serde_json::to_value(ChatEvent::ToolCall {
            id: "c1".into(),
            name: "list_tables".into(),
            args: json!({}),
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"type": "tool_call", "id": "c1", "name": "list_tables", "args": {}})
        );
        let v = serde_json::to_value(ChatEvent::ToolResult {
            id: "c1".into(),
            ok: true,
            summary: "1 row".into(),
            columns: None,
            rows: None,
            truncated: None,
            error: None,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({"type": "tool_result", "id": "c1", "ok": true, "summary": "1 row"})
        );
        assert_eq!(
            serde_json::to_value(ChatEvent::Done).unwrap(),
            json!({"type": "done"})
        );
        assert_eq!(
            serde_json::to_value(ChatEvent::ToolStarting {
                name: "run_read_query".into()
            })
            .unwrap(),
            json!({"type": "tool_starting", "name": "run_read_query"})
        );
    }

    #[test]
    fn request_parses_camel_case_and_rejects_other_roles() {
        let r: ChatRequest = serde_json::from_value(json!({
            "chatId": "a",
            "sources": [{"connId": "b", "database": null, "schema": null, "allowWrites": true}, {"connId": "c"}],
            "history": [{"role": "assistant", "content": "x"}], "message": "m"
        }))
        .unwrap();
        assert!(r.sources[0].allow_writes);
        assert!(!r.sources[1].allow_writes);
        assert_eq!(r.history[0].role, ChatRole::Assistant);
        let bad = serde_json::from_value::<ChatRequest>(json!({
            "chatId": "a", "sources": [{"connId": "b"}], "history": [{"role": "system", "content": "x"}], "message": "m"
        }));
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn text_answer_ends_with_done_and_sends_history() {
        let (base, mut reqs) = serve(vec![
            head("200 OK", "text/event-stream"),
            text_chunk("Hi "),
            text_chunk("there"),
            finish_chunk("stop"),
            done(),
        ])
        .await;
        let (_d, st, conn) = state_with(&base);
        let events = collect(&st, request(&conn, "hello")).await;
        assert_eq!(
            events,
            vec![
                ChatEvent::Text {
                    delta: "Hi ".into()
                },
                ChatEvent::Text {
                    delta: "there".into()
                },
                ChatEvent::Done,
            ]
        );
        assert!(!st.chats.is_running("chat-1"));
        let raw = reqs.recv().await.unwrap();
        let body: Value = serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1], json!({"role": "user", "content": "earlier"}));
        assert_eq!(msgs[2], json!({"role": "user", "content": "hello"}));
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn endless_tool_calls_stop_after_max_steps() {
        let (base, mut reqs) = serve(vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "c", "function": {"name": "list_tables", "arguments": "{}"}}
            ]}, "finish_reason": "tool_calls"}]})),
            done(),
        ])
        .await;
        let (_d, st, conn) = state_with(&base);
        let events = collect(&st, request(&conn, "loop")).await;
        let last = events.last().unwrap();
        match last {
            ChatEvent::Error { error } => {
                assert_eq!(error.code, ErrorCode::AiProtocol);
                assert_eq!(
                    error.message,
                    "The assistant stopped after 8 steps without an answer"
                );
            }
            other => panic!("{other:?}"),
        }
        let results = events
            .iter()
            .filter(|e| matches!(e, ChatEvent::ToolResult { ok: true, .. }))
            .count();
        assert_eq!(results, MAX_STEPS);
        // Each step: the name while the call streams, then the call, then its result.
        assert!(matches!(
            &events[..3],
            [
                ChatEvent::ToolStarting { name },
                ChatEvent::ToolCall { name: n2, .. },
                ChatEvent::ToolResult { ok: true, .. },
            ] if name == "list_tables" && n2 == "list_tables"
        ));
        let mut calls = 0;
        while reqs.try_recv().is_ok() {
            calls += 1;
        }
        assert_eq!(calls, MAX_STEPS);
    }

    #[tokio::test]
    async fn setup_errors_are_error_events() {
        let (base, _r) = serve(vec![]).await;
        let (_d, st, _conn) = state_with(&base);
        let events = collect(&st, request("missing", "hi")).await;
        assert!(matches!(
            &events[..],
            [ChatEvent::Error { error }] if error.code == ErrorCode::NotFound
        ));
        let events = collect(&st, request("missing", "  ")).await;
        assert!(matches!(
            &events[..],
            [ChatEvent::Error { error }] if error.code == ErrorCode::InvalidInput
        ));
    }

    #[tokio::test]
    async fn source_count_is_bounded() {
        let (base, _r) = serve(vec![]).await;
        let (_d, st, conn) = state_with(&base);
        let mut req = request(&conn, "hi");
        req.sources.clear();
        let events = collect(&st, req.clone()).await;
        assert!(matches!(
            &events[..],
            [ChatEvent::Error { error }] if error.message == "Choose at least one database for the chat"
        ));
        req.sources = vec![request(&conn, "hi").sources[0].clone(); MAX_SOURCES + 1];
        let events = collect(&st, req).await;
        assert!(matches!(
            &events[..],
            [ChatEvent::Error { error }] if error.message == "A chat can use at most 8 databases"
        ));
    }

    #[tokio::test]
    async fn receiver_gone_stops_the_run() {
        let (base, _r) = serve(vec![
            head("200 OK", "text/event-stream"),
            text_chunk("a"),
            (3_000, done().1),
        ])
        .await;
        let (_d, st, conn) = state_with(&base);
        let started = std::time::Instant::now();
        let mut n = 0;
        run(&st, request(&conn, "hi"), |_| {
            n += 1;
            false
        })
        .await;
        // The text delta, then the terminal event that nobody receives.
        assert_eq!(n, 2);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!st.chats.is_running("chat-1"));
    }

    /// Rows of table `t` in the SQLite file that `sqlite_conn` made.
    async fn count_rows(dir: &std::path::Path, name: &str) -> i64 {
        use sqlx::Connection as _;
        use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(dir.join(format!("{name}.db"))),
        )
        .await
        .unwrap();
        let n = sqlx::query_scalar("SELECT count(*) FROM t")
            .fetch_one(&mut c)
            .await
            .unwrap();
        c.close().await.unwrap();
        n
    }

    fn propose(sql: &str) -> Vec<crate::core::ai::client::tests::Part> {
        vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "w", "function": {"name": "propose_write", "arguments": json!({"sql": sql}).to_string()}}
            ]}, "finish_reason": "tool_calls"}]})),
            done(),
        ]
    }

    /// The write waits for the user: Deny runs nothing, Approve runs it. The `tool_result`
    /// comes only after the decision.
    #[tokio::test]
    async fn a_proposed_write_runs_only_after_approval() {
        let (base, _r) = serve(propose("DELETE FROM t WHERE id <= 2")).await;
        let (dir, st, _) = state_with(&base);
        let conn = sqlite_conn(&st, dir.path(), "w", 3).await;
        for (approve, rows_left) in [(false, 3), (true, 1)] {
            let mut req = request(&conn, "delete");
            req.sources[0].allow_writes = true;
            let mut result = None;
            run(&st, req, |e| {
                if let ChatEvent::WritePreview { id, .. } = &e {
                    st.approvals.decide("chat-1", id, approve).unwrap();
                }
                if matches!(e, ChatEvent::ToolResult { .. }) {
                    result = Some(e);
                    return false;
                }
                true
            })
            .await;
            match result.unwrap() {
                ChatEvent::ToolResult {
                    ok, summary, error, ..
                } if approve => {
                    assert!(ok, "{error:?}");
                    assert!(
                        summary.starts_with("2 rows changed · committed"),
                        "{summary}"
                    );
                }
                ChatEvent::ToolResult { ok, error, .. } => {
                    assert!(!ok);
                    assert_eq!(error.unwrap().code, ErrorCode::WriteDenied);
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(count_rows(dir.path(), "w").await, rows_left);
        }
    }

    /// Stop while the write waits: nothing runs, and a late Approve finds nothing.
    #[tokio::test]
    async fn stop_while_a_write_waits_runs_nothing() {
        let (base, _r) = serve(propose("DELETE FROM t")).await;
        let (dir, st, _) = state_with(&base);
        let conn = sqlite_conn(&st, dir.path(), "w", 3).await;
        let mut req = request(&conn, "delete");
        req.sources[0].allow_writes = true;
        let mut events = Vec::new();
        run(&st, req, |e| {
            if matches!(e, ChatEvent::WritePreview { .. }) {
                st.chats.cancel("chat-1");
            }
            events.push(e);
            true
        })
        .await;
        assert!(matches!(
            events.last(),
            Some(ChatEvent::Error { error }) if error.code == ErrorCode::Cancelled
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ChatEvent::ToolResult { .. }))
        );
        let late = st.approvals.decide("chat-1", "w", true).unwrap_err();
        assert_eq!(late.code, ErrorCode::NotFound);
        assert_eq!(count_rows(dir.path(), "w").await, 3);
    }

    #[test]
    fn registry_keeps_a_newer_run() {
        let reg = ChatRegistry::default();
        let first = reg.start("x");
        let second = reg.start("x");
        assert!(first.token.is_cancelled());
        drop(first);
        assert!(reg.is_running("x"));
        reg.cancel("x");
        assert!(second.token.is_cancelled());
        drop(second);
        assert!(!reg.is_running("x"));
        reg.cancel("unknown");
    }

    #[test]
    fn cancel_all_stops_every_run() {
        let reg = ChatRegistry::default();
        let a = reg.start("a");
        let b = reg.start("b");
        reg.cancel_all();
        assert!(a.token.is_cancelled() && b.token.is_cancelled());
    }
}
