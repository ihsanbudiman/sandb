//! Shared harness for the `it_*` suites: the stack's addresses, fixture values, a test state,
//! connection builders, scratch names, the Mongo cleanup guard and the chat helpers.
//!
//! A suite opts in with `mod common;`. The kit is compiled into every test binary, so not every
//! item is used by every suite.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use mongodb::bson::Document;
use serde_json::{Map, Value};

use sandb_lib::core::ai::event::ChatEvent;
use sandb_lib::core::connections::{Connection, ConnectionInput, Env, SslMode};
use sandb_lib::core::engine::Engine;
use sandb_lib::core::error::AppError;
use sandb_lib::core::result::{QueryRun, RunRequest};
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

/// The seed's fixed sizes and names, shared by every assertion.
pub mod seed {
    pub const SHOP: &str = "shop";
    pub const STAGING: &str = "shop_staging";
    pub const ANALYTICS: &str = "analytics";
    pub const MAIN: &str = "main";

    pub const CUSTOMERS: u64 = 500;
    pub const ORDERS: u64 = 500;
    pub const PRODUCTS: u64 = 500;
    pub const EVENTS: u64 = 10_000;
    pub const BIG_DOCS: u64 = 100;
    /// Customers whose `plan` is `pro`: one in three, starting at id 1.
    pub const CUSTOMERS_ON_PRO: i64 = 167;

    pub const TABLES: [&str; 8] = [
        "customers",
        "products",
        "orders",
        "order_items",
        "notes",
        "note_tags",
        "order_status_history",
        "type_zoo",
    ];
    pub const MONGO_COLLECTIONS: [&str; 3] = ["customers", "orders", "products"];
    pub const TYPE_ZOO_ROWS: usize = 3;
    /// `big_docs` documents land in a column sample before the 8 MiB cap stops it.
    pub const BIG_DOC_SAMPLE: usize = 4;
}

/// The repository root (`src-tauri/..`).
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// `testdb/.env.test`, parsed once: comments, `k=v`, trimmed values.
pub fn env_test() -> &'static HashMap<String, String> {
    static ENV: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let path = root().join("testdb/.env.test");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        raw.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect()
    });
    &ENV
}

pub fn env(key: &str) -> String {
    env_test()
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("{key} is missing from testdb/.env.test"))
}

pub fn port(key: &str) -> u16 {
    env(key)
        .parse()
        .unwrap_or_else(|e| panic!("{key} is not a port: {e}"))
}

pub fn pg16() -> u16 {
    port("PG16_PORT")
}

pub fn pg13() -> u16 {
    port("PG13_PORT")
}

pub fn mysql() -> u16 {
    port("MYSQL_PORT")
}

pub fn mariadb() -> u16 {
    port("MARIADB_PORT")
}

pub fn mongo_rs() -> u16 {
    port("MONGO_RS_PORT")
}

pub fn mongo_standalone() -> u16 {
    port("MONGO_STANDALONE_PORT")
}

pub fn rw_user() -> String {
    env("SANDB_RW_USER")
}

pub fn rw_password() -> String {
    env("SANDB_RW_PASSWORD")
}

pub fn ro_user() -> String {
    env("SANDB_RO_USER")
}

pub fn ro_password() -> String {
    env("SANDB_RO_PASSWORD")
}

pub fn root_password() -> String {
    env("MONGO_INITDB_ROOT_PASSWORD")
}

/// The password for a seed user.
pub fn password(user: &str) -> String {
    if user == "sandb_rw" {
        rw_password()
    } else if user == "sandb_ro" {
        ro_password()
    } else {
        root_password()
    }
}

/// The mock LLM's `/v1` base URL.
pub fn mock_url() -> String {
    std::env::var("SANDB_MOCK_LLM_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}/v1", port("MOCK_LLM_PORT")))
        .trim_end_matches('/')
        .to_string()
}

/// The SQLite fixture, copied by every suite that needs a writable file.
pub fn sqlite_fixture() -> PathBuf {
    let path = root().join("testdb/sqlite/shop.db");
    assert!(
        path.is_file(),
        "missing fixture {} (run `pnpm db:sqlite`)",
        path.display()
    );
    path
}

/// A private app state in a fresh temp directory.
pub fn state() -> (tempfile::TempDir, Arc<AppState>) {
    let dir = tempfile::tempdir().unwrap();
    let st = state_in(dir.path());
    (dir, st)
}

pub fn state_in(dir: &Path) -> Arc<AppState> {
    Arc::new(AppState::new(dir, Box::new(MemoryStore::new())).unwrap())
}

/// One SQL service of the stack.
pub struct Net {
    pub label: &'static str,
    pub engine: Engine,
    pub port: u16,
}

/// The four network SQL services, in report order.
pub fn sqlnet() -> Vec<Net> {
    vec![
        Net {
            label: "pg16",
            engine: Engine::Pg,
            port: pg16(),
        },
        Net {
            label: "pg13",
            engine: Engine::Pg,
            port: pg13(),
        },
        Net {
            label: "mysql",
            engine: Engine::Mysql,
            port: mysql(),
        },
        Net {
            label: "mariadb",
            engine: Engine::Mysql,
            port: mariadb(),
        },
    ]
}

fn blank(engine: Engine, port: u16) -> ConnectionInput {
    ConnectionInput {
        name: String::new(),
        engine,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: String::new(),
        user: String::new(),
        ssl: SslMode::Prefer,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    }
}

/// A saved SQLite connection for `file`; only the file matters for this engine.
pub fn sqlite_input(file: &Path, read_only: bool) -> ConnectionInput {
    let mut i = blank(Engine::Sqlite, 0);
    i.name = if read_only {
        "it-sqlite-ro".into()
    } else {
        "it-sqlite".into()
    };
    i.host = String::new();
    i.file = file.to_string_lossy().into_owned();
    i.read_only = read_only;
    i
}

/// A saved connection on a network SQL engine as `sandb_rw`; only host/port/database/user/ssl
/// matter for these engines.
pub fn sql_input(label: &str, engine: Engine, port: u16, read_only: bool) -> ConnectionInput {
    let mut i = blank(engine, port);
    i.name = format!("it-{label}");
    i.database = seed::SHOP.into();
    i.user = rw_user();
    i.read_only = read_only;
    i
}

/// A saved Mongo connection; the user lives in `admin` and no TLS is used.
pub fn mongo_input(
    label: &str,
    port: u16,
    user: &str,
    database: &str,
    read_only: bool,
) -> ConnectionInput {
    let mut i = blank(Engine::Mongo, port);
    i.name = format!("it-{label}");
    i.database = database.into();
    i.user = user.into();
    i.ssl = SslMode::Disable;
    i.auth_source = "admin".into();
    i.read_only = read_only;
    i
}

/// A saved connection plus the private state it lives in.
pub struct Ctx {
    pub _dir: tempfile::TempDir,
    pub st: Arc<AppState>,
    pub conn: Connection,
    pub label: String,
}

/// Saves `input` in a new private state.
pub fn ctx(label: &str, input: ConnectionInput, password: Option<String>) -> Ctx {
    let (dir, st) = state();
    ctx_in(dir, st, label, input, password)
}

/// Saves `input` in the given state (for two connections in one state, or a shared file).
pub fn ctx_in(
    dir: tempfile::TempDir,
    st: Arc<AppState>,
    label: &str,
    input: ConnectionInput,
    password: Option<String>,
) -> Ctx {
    let conn = st
        .create_connection(input, password)
        .unwrap_or_else(|e| panic!("{label}: saving the connection failed: {e:?}"));
    Ctx {
        _dir: dir,
        st,
        conn,
        label: label.into(),
    }
}

/// A `sandb_rw` connection on a network SQL engine.
pub fn net_ctx(label: &str, engine: Engine, port: u16) -> Ctx {
    ctx(
        label,
        sql_input(label, engine, port, false),
        Some(rw_password()),
    )
}

/// A `sandb_rw` connection on a temp copy of the SQLite fixture.
pub fn sqlite_ctx(read_only: bool) -> Ctx {
    let (dir, st) = state();
    let file = dir.path().join("shop.db");
    std::fs::copy(sqlite_fixture(), &file).unwrap();
    ctx_in(
        dir,
        st,
        "sqlite",
        sqlite_input(&file, read_only),
        None,
    )
}

/// A Mongo connection as `user` on `shop_staging`.
pub fn mongo_ctx(label: &str, port: u16, user: &str, read_only: bool) -> Ctx {
    ctx(
        label,
        mongo_input(label, port, user, seed::STAGING, read_only),
        Some(password(user)),
    )
}

/// The four network SQL services plus the SQLite fixture.
pub fn all_sql() -> Vec<Ctx> {
    let mut v: Vec<Ctx> = sqlnet()
        .into_iter()
        .map(|n| net_ctx(n.label, n.engine, n.port))
        .collect();
    v.push(sqlite_ctx(false));
    v
}

/// One scratch name for tables and collections: `it_` plus twelve lowercase hex characters.
/// The prefix is one the mock LLM reads back out of a chat message, so a scenario can name it.
pub fn scratch() -> String {
    format!("it_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// A run request on `conn`.
pub fn req(conn: &Connection, sql: &str) -> RunRequest {
    RunRequest {
        conn_id: conn.id.clone(),
        database: None,
        sql: sql.into(),
        query_id: uuid::Uuid::new_v4().to_string(),
        limit: None,
        tab_id: None,
        sort: None,
    }
}

/// A cell of the last statement.
pub fn cell(r: &QueryRun, row: usize, col: usize) -> Value {
    r.statements.last().unwrap().rows[row][col].clone()
}

/// A cell of the last statement, looked up by column name.
pub fn cell_named(r: &QueryRun, name: &str, row: usize) -> Value {
    let s = r.statements.last().unwrap();
    let col = s
        .columns
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name} in {:?}", s.columns));
    s.rows[row][col].clone()
}

/// Drops a scratch name a test made when it ends, also while a failed test unwinds. `Drop`
/// cannot use the test's client or runtime (both are torn down after a panic), so it opens its
/// own root client on its own thread and reports failures on stderr.
pub struct Cleanup {
    uri: String,
    collections: Vec<(String, String)>,
    databases: Vec<String>,
}

impl Cleanup {
    /// A guard for scratch names on the server at `port`.
    pub fn new(port: u16) -> Self {
        Self {
            uri: format!("mongodb://root:{}@127.0.0.1:{port}/admin", root_password()),
            collections: Vec::new(),
            databases: Vec::new(),
        }
    }

    /// Tracks `coll` in `shop_staging` and returns it.
    pub fn collection(&mut self, coll: impl Into<String>) -> String {
        self.collection_in(seed::STAGING, coll)
    }

    /// Tracks `coll` in `database` and returns it.
    pub fn collection_in(&mut self, database: &str, coll: impl Into<String>) -> String {
        let coll = coll.into();
        self.collections
            .push((database.into(), coll.clone()));
        coll
    }

    /// Tracks a scratch database and returns it.
    pub fn database(&mut self, database: impl Into<String>) -> String {
        let database = database.into();
        self.databases.push(database.clone());
        database
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.collections.is_empty() && self.databases.is_empty() {
            return;
        }
        let uri = self.uri.clone();
        let collections = std::mem::take(&mut self.collections);
        let databases = std::mem::take(&mut self.databases);
        let joined = std::thread::Builder::new()
            .name("it-mongo-cleanup".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("cleanup runtime");
                rt.block_on(async move {
                    let client = mongodb::Client::with_uri_str(uri)
                        .await
                        .expect("cleanup client");
                    for (database, coll) in collections {
                        if let Err(e) = client
                            .database(&database)
                            .collection::<Document>(&coll)
                            .drop()
                            .await
                        {
                            let e = AppError::from(e);
                            // A name the test already dropped is the normal case.
                            let already_gone = e.server_code.as_ref().map(|c| c.code.as_str())
                                == Some("26");
                            if !already_gone {
                                eprintln!("cleanup: drop {database}.{coll}: {e:?}");
                            }
                        }
                    }
                    for database in databases {
                        if let Err(e) = client.database(&database).drop().await {
                            eprintln!("cleanup: drop database {database}: {e:?}");
                        }
                    }
                });
            })
            .map(|h| h.join());
        if let Err(e) = joined {
            eprintln!("cleanup: the mongo cleanup thread failed: {e:?}");
        }
    }
}

/// Drops a scratch collection; one that was never created is fine.
pub async fn drop_scratch(client: &mongodb::Client, database: &str, coll: &str) {
    if let Err(e) = client
        .database(database)
        .collection::<Document>(coll)
        .drop()
        .await
    {
        let e = AppError::from(e);
        assert_eq!(
            e.server_code.as_ref().map(|c| c.code.as_str()),
            Some("26"), // NamespaceNotFound
            "drop {database}.{coll}: {e:?}"
        );
    }
}

/// Exactly one terminal event, and it is the last one.
pub fn assert_terminal(events: &[ChatEvent]) {
    let terminal = events
        .iter()
        .filter(|e| matches!(e, ChatEvent::Done | ChatEvent::Error { .. }))
        .count();
    assert_eq!(terminal, 1, "{events:#?}");
    assert!(
        matches!(
            events.last(),
            Some(ChatEvent::Done | ChatEvent::Error { .. })
        ),
        "{events:#?}"
    );
}

/// Exactly one terminal event, it is the last one, and it is `Done`.
pub fn assert_done(events: &[ChatEvent]) {
    assert_terminal(events);
    assert_eq!(events.last(), Some(&ChatEvent::Done), "{events:#?}");
}

/// The streamed text of a chat.
pub fn text(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

/// Each tool call with its result, in call order.
pub fn tools(events: &[ChatEvent]) -> Vec<(&str, &Map<String, Value>, &ChatEvent)> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::ToolCall { id, name, args, .. } => Some((id, name, args)),
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
pub fn tool(events: &[ChatEvent]) -> (&str, &Map<String, Value>, &ChatEvent) {
    let all = tools(events);
    assert_eq!(all.len(), 1, "{events:#?}");
    all[0]
}

/// Request bodies the mock recorded whose user messages contain `marker`.
pub async fn recorded_with(marker: &str) -> Vec<Value> {
    static CRYPTO: LazyLock<()> = LazyLock::new(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    LazyLock::force(&CRYPTO);
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
pub fn tool_contents(bodies: &[Value]) -> Vec<Value> {
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
pub fn offered_tools(bodies: &[Value]) -> Vec<String> {
    bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

/// A direct root Mongo client, for operations `sandb_rw` may not run.
pub async fn root_client(port: u16) -> mongodb::Client {
    mongodb::Client::with_uri_str(format!(
        "mongodb://root:{}@127.0.0.1:{port}/admin",
        root_password()
    ))
    .await
    .unwrap()
}
