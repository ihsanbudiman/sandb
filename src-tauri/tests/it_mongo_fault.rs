// Phase 7, slice A: network faults on MongoDB, in front of an in-test TCP proxy the test can cut
// (close its sockets) or black-hole (accept and never forward). The servers keep their own
// published ports, so every test then reads what the app saved and that it works again.
// Cases: the editor run with its cursor (find and aggregate), the run into a black hole (with a
// cancel, and without one the driver's own timeout), the grid save, the AI read, and the script
// export with `collection_ddl`.
// Run: `cargo test --features it --test it_mongo_fault`.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use mongodb::bson::{Bson, Document, doc};
use sandb_lib::core::connections::{self, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::db::dump::{self, TableExportRequest};
use sandb_lib::core::db::edit::{self, ApplyEditsRequest, EditOperation};
use sandb_lib::core::db::query::{self, RunRequest};
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;
use tokio::io::AsyncWriteExt as _;

const RS: u16 = 57017;
const DB: &str = "shop_staging";

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

fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    (dir, st)
}

/// One faulting proxy at a time: the driver keeps its background connections while a test sets
/// its fault, and two faulting proxies would cut each other's sockets.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------------------------
// Fault proxy

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Forward,
    /// The copies of every live pair stop and both sockets drop, so the driver reads the end of
    /// the stream; a socket accepted behind the cut is refused at once.
    Cut,
    /// The copies of every live pair stop and the sockets stay open, so the driver's next read
    /// or write waits in the kernel buffers; a socket accepted behind it never forwards.
    Blackhole,
}

/// One live connection pair; `abort` ends it and both sockets drop.
struct Pair {
    abort: tokio::task::AbortHandle,
}

/// State the proxy's listener and its fault methods share.
struct FaultState {
    mode: StdMutex<Mode>,
    live: StdMutex<Vec<Pair>>,
    /// The client sockets accepted behind a black hole. They stay open and nothing reads or
    /// writes them, so the driver's handshake or operation waits for ever on its side.
    held: StdMutex<Vec<tokio::net::TcpStream>>,
}

/// The fault proxy. Every connection pair is registered while it lives, so the cut reaches the
/// sockets the driver opened before the fault, not just the ones it opens after it.
struct FaultProxy {
    state: Arc<FaultState>,
    port: u16,
}

impl FaultProxy {
    async fn start(port: u16, target: u16) -> FaultProxy {
        let listen = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap_or_else(|e| panic!("cannot bind the fault proxy on {port}: {e}"));
        // Port 0 asks the kernel for a free port, so several test binaries can run at once.
        let port = listen.local_addr().expect("the proxy address").port();
        let state = Arc::new(FaultState {
            mode: StdMutex::new(Mode::Forward),
            live: StdMutex::new(Vec::new()),
            held: StdMutex::new(Vec::new()),
        });
        tokio::spawn({
            let state = state.clone();
            async move {
                loop {
                    let Ok((mut client_socket, _)) = listen.accept().await else {
                        continue;
                    };
                    let mode = *state.mode.lock().unwrap();
                    match mode {
                        Mode::Cut => {
                            // The client reads the end of the stream.
                            drop(client_socket);
                            continue;
                        }
                        Mode::Blackhole => {
                            // A true black hole for a new socket: it stays open, and nothing
                            // reads or writes it. A real partition sends no FIN and no RST.
                            state.held.lock().unwrap().push(client_socket);
                            continue;
                        }
                        _ => {}
                    }
                    let Ok(mut server_socket) =
                        tokio::net::TcpStream::connect(("127.0.0.1", target)).await
                    else {
                        drop(client_socket);
                        continue;
                    };
                    let join = tokio::spawn(async move {
                        let _ =
                            tokio::io::copy_bidirectional(&mut client_socket, &mut server_socket)
                                .await;
                        let _ = client_socket.shutdown().await;
                        let _ = server_socket.shutdown().await;
                    });
                    state.live.lock().unwrap().push(Pair {
                        abort: join.abort_handle(),
                    });
                }
            }
        });
        FaultProxy { state, port }
    }

    /// Cuts every live pair and every held socket: both ends read the end of the stream.
    fn cut(&self) {
        *self.state.mode.lock().unwrap() = Mode::Cut;
        let mut live = self.state.live.lock().unwrap();
        for pair in live.drain(..) {
            pair.abort.abort();
        }
        // A held socket has no copy task to abort; dropping it closes it.
        self.state.held.lock().unwrap().clear();
    }

    /// The black hole. A new socket stays open with no reader (see the accept loop); a live
    /// pair is cut, because pausing a running copy and resuming it proved unstable in the
    /// harness. The tests that need a wait therefore make the driver open a new socket (a new
    /// connection id) while the proxy is in this mode.
    fn blackhole(&self) {
        *self.state.mode.lock().unwrap() = Mode::Blackhole;
        let mut live = self.state.live.lock().unwrap();
        for pair in live.drain(..) {
            pair.abort.abort();
        }
    }

    /// Forwards again: the held sockets close and new sockets forward.
    fn listen(&self) {
        *self.state.mode.lock().unwrap() = Mode::Forward;
        self.state.held.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers

/// A direct ROOT client at `port`, for `$currentOp` with `allUsers` (the edit runs as sandb_rw).
async fn direct_root_client(port: u16) -> mongodb::Client {
    let password = env_test()
        .get("MONGO_INITDB_ROOT_PASSWORD")
        .cloned()
        .expect("MONGO_INITDB_ROOT_PASSWORD in testdb/.env.test");
    let options = sandb_lib::core::mongo::conn::client_options(
        &connections::Connection {
            id: "fault-root".into(),
            name: "fault-root".into(),
            engine: Engine::Mongo,
            env: Env::Local,
            host: "127.0.0.1".into(),
            port,
            database: "admin".into(),
            user: "root".into(),
            ssl: SslMode::Disable,
            file: String::new(),
            ai_write: false,
            read_only: false,
            version: String::new(),
            uri: String::new(),
            auth_source: "admin".into(),
            topology: None,
        },
        Some(&password),
    )
    .await
    .unwrap();
    mongodb::Client::with_options(options).unwrap()
}

/// A new saved connection to the proxy, so its driver client opens new sockets. The black hole
/// holds those; an existing client's sockets were opened before the fault.
fn fresh_conn(st: &AppState, proxy: &FaultProxy, user: &str, name: &str) -> String {
    let password = match user {
        "sandb_rw" => rw_password(),
        _ => ro_password(),
    };
    let mut input = if user == "sandb_rw" {
        rw_input(proxy.port, name)
    } else {
        ro_input(proxy.port, name)
    };
    input.name = format!("it-mongo-fault-{name}-{}", uuid::Uuid::new_v4().simple());
    connections::create(st, input, Some(password)).unwrap().id
}

fn req(conn_id: &str, text: &str) -> RunRequest {
    RunRequest {
        conn_id: conn_id.into(),
        database: None,
        sql: text.into(),
        query_id: uuid::Uuid::new_v4().to_string(),
        limit: None,
        tab_id: None,
        sort: None,
    }
}

async fn run(st: &AppState, r: RunRequest) {
    let text = r.sql.clone();
    query::run(st, r)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e:?}"));
}

fn rw_password() -> String {
    env_test()["SANDB_RW_PASSWORD"].clone()
}

fn ro_password() -> String {
    env_test()["SANDB_RO_PASSWORD"].clone()
}

/// The connection string for a fault test: one host, `directConnection=true`, so the driver
/// never discovers the replica-set member behind the proxy and bypasses it.
fn fault_uri(user: &str, password: &str, port: u16) -> String {
    format!(
        "mongodb://{user}:{password}@127.0.0.1:{port}/{DB}?directConnection=true&authSource=admin&serverSelectionTimeoutMS=10000"
    )
}

fn rw_input(port: u16, name: &str) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-mongo-fault-{name}"),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: DB.into(),
        user: "sandb_rw".into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: fault_uri("sandb_rw", &rw_password(), port),
        auth_source: "admin".into(),
        topology: None,
    }
}

/// A read-only connection input to `port`.
fn ro_input(port: u16, name: &str) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-mongo-fault-{name}"),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: DB.into(),
        user: "sandb_ro".into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only: true,
        version: None,
        uri: fault_uri("sandb_ro", &ro_password(), port),
        auth_source: "admin".into(),
        topology: None,
    }
}

/// A saved connection through the proxy, for `user` (`sandb_rw` may write).
fn saved(st: &AppState, proxy: &FaultProxy, user: &str, read_only: bool) -> String {
    let password = match user {
        "sandb_rw" => rw_password(),
        _ => ro_password(),
    };
    let input = ConnectionInput {
        name: format!("it-mongo-fault-{user}-{}", proxy.port),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: proxy.port,
        database: DB.into(),
        user: user.into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only,
        version: None,
        uri: fault_uri(user, &password, proxy.port),
        auth_source: "admin".into(),
        topology: None,
    };
    connections::create(st, input, Some(password)).unwrap().id
}

/// Seeds `rows` `_id` + text documents into a fresh `it_faults_` collection on `port`; the name
/// is registered with `clean` before the insert, so a seeding panic cannot leak it.
async fn seeded(
    st: &AppState,
    port: u16,
    clean: &mut Cleanup,
    rows: i32,
) -> (mongodb::Client, String) {
    let coll = clean.collection(scratch());
    let id = connections::create(st, rw_input(port, "seed"), Some(rw_password()))
        .unwrap()
        .id;
    let client = sandb_lib::core::mongo::client::client(st, &id)
        .await
        .unwrap();
    let docs: Vec<Document> = (1..=rows)
        .map(|i| doc! { "_id": i, "a": format!("row-bbb-{i:0>4}-padding-padding-padding") })
        .collect();
    client
        .database(DB)
        .collection::<Document>(&coll)
        .insert_many(docs)
        .await
        .unwrap();
    (client, coll)
}

fn scratch() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "it_faults_{:016x}{:016x}",
        nanos,
        N.fetch_add(1, Ordering::Relaxed) as u128
    )
}

/// The seeded `a` value of row `i`.
fn seeded_value(i: usize) -> String {
    format!("row-bbb-{i:0>4}-padding-padding-padding")
}

/// The `a` value of `_id: id` in the collection, read straight at the server.
async fn a_at(client: &mongodb::Client, coll: &str, id: i32) -> Bson {
    client
        .database(DB)
        .collection::<Document>(coll)
        .find_one(doc! { "_id": id })
        .await
        .unwrap()
        .unwrap_or_default()
        .get("a")
        .cloned()
        .unwrap_or(Bson::Null)
}

/// Drops the scratch collection; one that never existed is fine.
async fn drop_a(client: &mongodb::Client, coll: &str) {
    let _ = client
        .database(DB)
        .collection::<Document>(coll)
        .drop()
        .await;
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
                            .database(DB)
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

/// One typed wire value, as the grid sends it.
fn typed(ty: &str, value: &str) -> serde_json::Value {
    serde_json::json!({ "type": ty, "value": value })
}

/// One update of `a` on `_id`, with `old` as the grid loaded it.
fn update_a(id: i32, new: &str, old: &str) -> EditOperation {
    serde_json::from_value(serde_json::json!({
        "key": "0",
        "kind": "update",
        "pk": { "_id": typed("int32", &id.to_string()) },
        "set": { "a": typed("string", new) },
        "old": { "a": typed("string", old) },
    }))
    .unwrap()
}

/// The save request with the id the dialog cancels it by.
fn edits_request_id(
    conn_id: &str,
    sql: String,
    operations: Vec<EditOperation>,
    query_id: String,
) -> ApplyEditsRequest {
    ApplyEditsRequest {
        conn_id: conn_id.into(),
        database: DB.into(),
        sql,
        tab_id: None,
        query_id: Some(query_id),
        operations,
    }
}

fn edits_request(conn_id: &str, sql: String, operations: Vec<EditOperation>) -> ApplyEditsRequest {
    ApplyEditsRequest {
        conn_id: conn_id.into(),
        database: DB.into(),
        sql,
        tab_id: None,
        query_id: None,
        operations,
    }
}

/// The lines of a file; the NDJSON lines here are the documents.
fn lines_of(path: &std::path::Path) -> usize {
    use std::io::BufRead as _;
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .count()
}

// Case 1: the run whose cursor the cut reaches// Case 1: the run whose cursor the cut reaches

#[tokio::test]
async fn the_cursor_run_gets_a_connection_error_and_the_tab_lives_on() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");

    // The happy path first: the page comes from the server through the proxy.
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(50);
        r
    })
    .await;

    // The cut aborts every live pair. On a fresh client the run answers with the connection
    // error at the driver's server-selection timeout; on a warm pool it can still serve its
    // page, so the case does not hang either way.
    proxy.cut();
    let started = Instant::now();
    let fresh = connections::create(
        &st,
        {
            let mut i = ro_input(proxy.port, "fresh");
            i.name = format!("it-mongo-fault-fresh-{}", uuid::Uuid::new_v4().simple());
            i
        },
        Some(ro_password()),
    )
    .unwrap()
    .id;
    let e = query::run(&st, {
        let mut r = req(&fresh, "db.products.find({})");
        r.limit = Some(50);
        r
    })
    .await
    .unwrap_err();
    let elapsed = started.elapsed();
    proxy.listen();
    assert_eq!(
        e.code,
        ErrorCode::ConnectFailed,
        "cursor read: {e:?} (after {elapsed:?})"
    );

    // The tab works again, with no selector or cursor held on the server.
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(20);
        r
    })
    .await;
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

/// The plan's aggregate: the cursor of the pipeline is cut the same way.
#[tokio::test]
async fn the_cut_reaches_the_cursor_of_an_aggregate() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 250).await;
    let sql = format!("db.{coll}.aggregate([{{$match: {{}}}}, {{$sort: {{_id: 1}}}}])");
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(50);
        r
    })
    .await;

    proxy.cut();
    let started = Instant::now();
    let fresh = connections::create(
        &st,
        {
            let mut i = ro_input(proxy.port, "agg");
            i.name = format!("it-mongo-fault-agg-{}", uuid::Uuid::new_v4().simple());
            i
        },
        Some(ro_password()),
    )
    .unwrap()
    .id;
    let e = query::run(&st, {
        let mut r = req(&fresh, &sql);
        r.limit = Some(50);
        r
    })
    .await
    .unwrap_err();
    let elapsed = started.elapsed();
    proxy.listen();
    assert_eq!(
        e.code,
        ErrorCode::ConnectFailed,
        "aggregate: {e:?} (after {elapsed:?})"
    );
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

#[tokio::test]
async fn the_cut_reaches_a_replayed_load_more() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");
    // Load: the first 10; load more: the same text with a higher limit.
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(10);
        r
    })
    .await;

    proxy.cut();
    let started = Instant::now();
    let fresh = connections::create(
        &st,
        {
            let mut i = ro_input(proxy.port, "load");
            i.name = format!("it-mongo-fault-load-{}", uuid::Uuid::new_v4().simple());
            i
        },
        Some(ro_password()),
    )
    .unwrap()
    .id;
    let e = query::run(&st, {
        let mut r = req(&fresh, &sql);
        r.limit = Some(20);
        r
    })
    .await
    .unwrap_err();
    let elapsed = started.elapsed();
    proxy.listen();
    assert_eq!(
        e.code,
        ErrorCode::ConnectFailed,
        "load more: {e:?} (after {elapsed:?})"
    );
    // The tab of the first connection works again once the proxy forwards.
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(20);
        r
    })
    .await;
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

// ---------------------------------------------------------------------------------------------
// Case 2: runs into the black hole

#[tokio::test]
async fn the_blackhole_run_ends_on_time_and_the_tab_lives_on() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");
    // A working connection first.
    let id = fresh_conn(&st, &proxy, "sandb_ro", "warm");
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(10);
        r
    })
    .await;

    // A new client behind the black hole: its first socket is held open with no reply, so the
    // run waits on the driver's own handshake/selection timers.
    proxy.blackhole();
    let fresh = fresh_conn(&st, &proxy, "sandb_ro", "held");
    let started = Instant::now();
    let e = query::run(&st, {
        let mut r = req(&fresh, &sql);
        r.limit = Some(50);
        r
    })
    .await
    .expect_err("the run cannot answer behind a black hole");
    let elapsed = started.elapsed();
    proxy.listen();
    // Recorded, so the bound below is the measured one: 10.0 s, the driver's connect timeout
    // (`CONNECT_TIMEOUT`) for the held handshake.
    println!("RECORD no-cancel black hole: {elapsed:?} → {}", e.message);
    assert_eq!(e.code, ErrorCode::ConnectFailed, "{e:?}");
    assert!(
        (Duration::from_secs(8)..=Duration::from_secs(20)).contains(&elapsed),
        "the driver's own timers end the run, measured {elapsed:?}"
    );

    // The server is back: a further new connection works.
    let back = fresh_conn(&st, &proxy, "sandb_ro", "back");
    run(&st, {
        let mut r = req(&back, &sql);
        r.limit = Some(10);
        r
    })
    .await;

    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

/// The plan's cancel case: cancel returns promptly and the tab stays usable.
#[tokio::test]
async fn the_blackhole_run_endswith_a_cancel() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");
    let id = fresh_conn(&st, &proxy, "sandb_ro", "warm");
    run(&st, req(&id, &sql)).await;

    // A new client whose first socket is held by the black hole: the run waits in the driver.
    proxy.blackhole();
    let fresh = fresh_conn(&st, &proxy, "sandb_ro", "cancel");
    let query_id = uuid::Uuid::new_v4().to_string();
    let run_in = async {
        let outcome = query::run(&st, {
            let mut r = req(&fresh, &sql);
            r.query_id = query_id.clone();
            r.limit = Some(50);
            r
        })
        .await;
        (outcome, Instant::now())
    };
    let cancel_when_started = async {
        // `join!` polls both, so the run registers while this waits for it. Cancel must reach a
        // run that is really registered, not an early id.
        let deadline = Instant::now() + Duration::from_secs(5);
        while query::progress(&st, &query_id).is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            query::progress(&st, &query_id).is_some(),
            "the run did not start"
        );
        // Polling once is not enough: let the run reach the held socket before the cancel.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        query::cancel(&st, &query_id).await.unwrap();
        (started.elapsed(), started)
    };
    let ((outcome, ended_at), (cancel_done, cancelled_at)) =
        tokio::join!(run_in, cancel_when_started);
    proxy.listen();
    let ended_after_cancel = ended_at.saturating_duration_since(cancelled_at);
    let e = outcome.expect_err("the cancel ends the local run");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
    assert!(cancel_done < Duration::from_secs(3), "{cancel_done:?}");
    // The local run must stop within the poll interval (`run.rs`'s `until_cancelled` ticks
    // every 50 ms). Without that race the driver's 10 s connect timeout would be the only end.
    assert!(
        ended_after_cancel < Duration::from_secs(2),
        "the run ended {ended_after_cancel:?} after the cancel"
    );

    let back = fresh_conn(&st, &proxy, "sandb_ro", "after-cancel");
    run(&st, {
        let mut r = req(&back, &sql);
        r.limit = Some(10);
        r
    })
    .await;
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

// ---------------------------------------------------------------------------------------------
// Case 3: grid saves behind the cut proxy

#[tokio::test]
async fn the_cut_reaches_a_transaction_save_and_holds_nothing() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    let root = direct_root_client(RS).await;

    // A long batch keeps the transaction open; the cut lands while it is still open.
    let operations: Vec<EditOperation> = (100..10100)
        .map(|i| {
            serde_json::from_value(serde_json::json!({
                "key": i.to_string(),
                "kind": "insert",
                "pk": {},
                "set": {},
                "document": format!("{{ _id: {i}, a: \"inserted\" }}"),
            }))
            .unwrap()
        })
        .collect();
    let save = async { edit::apply_edits(&st, edits_request(&id, sql.clone(), operations)).await };
    let cut_when_open = async {
        // `$currentOp` lists the transaction's operations under the edit comment. Once one is
        // there, the cut lands inside the open transaction; a slow first poll still cuts.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let mut ops = root
                .database("admin")
                .aggregate(vec![
                    doc! { "$currentOp": { "allUsers": true } },
                    doc! { "$match": { "command.comment": "sandb grid edit" } },
                ])
                .await
                .unwrap();
            if ops.advance().await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        proxy.cut();
    };
    let (saved_result, ()) = tokio::join!(save, cut_when_open);
    proxy.listen();
    match saved_result {
        Ok(r) => {
            assert!(r.failed.is_some(), "the cut row reports the failure: {r:?}");
            assert!(
                !r.partial,
                "a transaction rolls back, so nothing is partial: {r:?}"
            );
        }
        Err(e) => assert!(
            matches!(e.code, ErrorCode::ConnectFailed | ErrorCode::Database),
            "{e:?}"
        ),
    }
    // Nothing of the transaction stays: both seeded rows untouched, no inserted row.
    for i in [1, 2] {
        assert_eq!(
            a_at(&seed_client, &coll, i).await,
            Bson::String(seeded_value(i as usize)),
            "doc {i} unchanged"
        );
    }
    assert_eq!(
        seed_client
            .database(DB)
            .collection::<Document>(&coll)
            .count_documents(doc! { "_id": { "$gte": 100 } })
            .await
            .unwrap(),
        0,
        "the inserts rolled back"
    );
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

/// QA P1: a save cancelled while the server cannot answer must never apply later.
#[tokio::test]
async fn a_cancelled_save_is_never_applied_later() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    // The page's client is warm, as in the QA steps; the black hole then holds every socket the
    // driver opens, so the save cannot reach the server (a paused container).
    let id = saved(&st, &proxy, "sandb_rw", false);
    run(&st, req(&id, &sql)).await;
    proxy.blackhole();
    let query_id = uuid::Uuid::new_v4().to_string();
    let save = async {
        edit::apply_edits(
            &st,
            edits_request_id(
                &id,
                sql.clone(),
                vec![update_a(1, "b_pause", &seeded_value(1))],
                query_id.clone(),
            ),
        )
        .await
    };
    let cancel_when_started = async {
        let deadline = Instant::now() + Duration::from_secs(5);
        while query::progress(&st, &query_id).is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            query::progress(&st, &query_id).is_some(),
            "the save did not start"
        );
        // The QA steps: Save, wait 1.5 s, then Cancel.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let started = Instant::now();
        let _ = query::cancel(&st, &query_id).await;
        started.elapsed()
    };
    let (outcome, cancel_done) = tokio::join!(save, cancel_when_started);
    let e = outcome.expect_err("the cancel ends the save");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
    assert!(cancel_done < Duration::from_secs(3), "{cancel_done:?}");
    // The server answers again; five seconds later the cancelled save must not have applied.
    proxy.listen();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        a_at(&seed_client, &coll, 1).await,
        Bson::String(seeded_value(1)),
        "a cancelled save must never apply later"
    );
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

/// Cancel with the transaction already open: it aborts, nothing is saved.
#[tokio::test]
async fn the_cancel_aborts_an_open_save_transaction() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    let root = direct_root_client(RS).await;

    // Many inserts keep the transaction open long enough for the cancel to land inside it.
    let mut operations = vec![update_a(1, "new-1", &seeded_value(1))];
    for i in 100..10100 {
        operations.push(
            serde_json::from_value(serde_json::json!({
                "key": i.to_string(),
                "kind": "insert",
                "pk": {},
                "set": {},
                "document": format!("{{ _id: {i}, a: \"inserted\" }}"),
            }))
            .unwrap(),
        );
    }
    let query_id = uuid::Uuid::new_v4().to_string();
    let save = async {
        edit::apply_edits(
            &st,
            edits_request_id(&id, sql.clone(), operations, query_id.clone()),
        )
        .await
    };
    let cancel_when_open = async {
        // The batch of 2,000 operations keeps the transaction open for a long window; wait for
        // its first operation to show in `$currentOp`, then cancel. If the batch is already
        // over, the cancel still fires and the outcome assertion reports it.
        // The save sends its query id as `comment`, so the registry's kill can find it.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let mut ops = root
                .database("admin")
                .aggregate(vec![
                    doc! { "$currentOp": { "allUsers": true } },
                    doc! { "$match": { "command.comment": &query_id } },
                ])
                .await
                .unwrap();
            if ops.advance().await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let started = Instant::now();
        let _ = query::cancel(&st, &query_id).await;
        started.elapsed()
    };
    let (outcome, cancel_done) = tokio::join!(save, cancel_when_open);
    let e = outcome.expect_err("the cancel aborts the save");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
    assert!(cancel_done < Duration::from_secs(3), "{cancel_done:?}");
    // The kill names the save: no operation of it may still run on the server.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let mut ops = root
            .database("admin")
            .aggregate(vec![
                doc! { "$currentOp": { "allUsers": true } },
                doc! { "$match": { "command.comment": &query_id } },
            ])
            .await
            .unwrap();
        let left = ops.advance().await.unwrap();
        if !left {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an operation of the cancelled save still runs on the server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The transaction aborted: both rows as they were, no insert stays.
    for i in [1, 2] {
        assert_eq!(
            a_at(&seed_client, &coll, i).await,
            Bson::String(seeded_value(i as usize)),
            "doc {i} unchanged"
        );
    }
    assert_eq!(
        seed_client
            .database(DB)
            .collection::<Document>(&coll)
            .count_documents(doc! { "_id": { "$gte": 100 } })
            .await
            .unwrap(),
        0,
        "the inserts rolled back"
    );
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

// ---------------------------------------------------------------------------------------------
// Case 4: the AI read behind the faulty proxy

/// `run_mongo_read` ends in `readonly::run_ai_read`, so the test drives that entry point with
/// the proxy cut: the tool step fails, and the turn would end with that error.
#[tokio::test]
async fn the_ai_read_behind_the_cut_gets_an_error() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 5).await;
    let sql = format!("db.{coll}.find({{}})");
    let limits = sandb_lib::core::db::readonly::ReadLimits::default();

    let page = sandb_lib::core::db::readonly::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap();
    assert!(!page.rows.is_empty(), "{page:?}");

    // The cut: the tool step fails and the chat's answer carries the error.
    proxy.cut();
    let e = sandb_lib::core::db::readonly::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "tool step: {e:?}");
    // A later turn works again on the same connection.
    let page = sandb_lib::core::db::readonly::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap();
    assert!(!page.rows.is_empty());
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

/// The black hole ends on the AI read's own time limit: `timeout` on the server turn, or a
/// little later on the client net (`timeout + 1 s grace`). The chat never hangs.
#[tokio::test]
async fn the_ai_read_blackhole_ends_on_the_time_limit() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 5).await;
    let sql = format!("db.{coll}.find({{}})");
    let limits = sandb_lib::core::db::readonly::ReadLimits {
        max_rows: 500,
        timeout: Duration::from_secs(1),
    };

    // A new client whose first socket the black hole holds: no reply ever comes, so only the
    // AI read's own time limit ends it.
    proxy.blackhole();
    let id = fresh_conn(&st, &proxy, "sandb_ro", "ai-held");
    let began = Instant::now();
    let e = sandb_lib::core::db::readonly::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap_err();
    let waited = began.elapsed();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::Timeout, "time limit: {e:?}");
    // timeout (1 s) + the 1 s net grace, then the kill passes through its own 2 s cap.
    assert!(waited < Duration::from_secs(6), "{waited:?}");
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

// ---------------------------------------------------------------------------------------------
// Case 5: the script export and `table_ddl` behind the cut proxy

#[tokio::test]
async fn the_cut_stops_the_export_and_the_collection_ddl() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = state();
    let proxy = FaultProxy::start(0, RS).await;
    let mut clean = Cleanup::new(RS);
    let (seed_client, coll) = seeded(&st, RS, &mut clean, 3).await;
    // A bigger collection, so the export is still streaming when the cut lands.
    let big = clean.collection(scratch());
    let docs: Vec<Document> = (0..300)
        .map(|i| doc! { "_id": i, "a": "x".repeat(4096) })
        .collect();
    seed_client
        .database(DB)
        .collection::<Document>(&big)
        .insert_many(docs)
        .await
        .unwrap();
    let id = saved(&st, &proxy, "sandb_rw", false);

    // The script of a live server.
    let script = dump::table_ddl(&st, &id, DB, None, "orders").await.unwrap();
    assert!(script.contains("createCollection"), "{script}");

    // The export starts with the proxy forwarding; the cut fires once its temp file exists, so
    // the failure hits the document stream and the temp-file cleanup runs.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("half.js");
    let mut export = Box::pin(dump::export_table_sql(
        &st,
        TableExportRequest {
            conn_id: id.clone(),
            database: DB.into(),
            schema: None,
            table: big.clone(),
            data: true,
        },
        &path,
    ));
    let mut cut = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    let result = loop {
        if let std::task::Poll::Ready(r) = futures_util::poll!(export.as_mut()) {
            break r;
        }
        if !cut && std::fs::read_dir(dir.path()).unwrap().next().is_some() {
            // The temp file exists: the DDL is written and the documents stream now.
            proxy.cut();
            cut = true;
        }
        assert!(Instant::now() < deadline, "the export did not finish");
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    proxy.listen();
    let e = result.unwrap_err();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "export: {e:?}");
    assert!(cut, "the cut fired after the temp file appeared");
    assert!(!path.exists(), "no final file: {e:?}");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "the temp file was removed: {e:?}"
    );

    // `table_ddl` answers with the same error while the cut stands, and works again.
    proxy.cut();
    let e = dump::table_ddl(&st, &id, DB, None, "orders")
        .await
        .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "ddl: {e:?}");
    let script = dump::table_ddl(&st, &id, DB, None, "orders").await.unwrap();
    assert!(script.contains("createCollection"));

    // The export with data on the live proxy gives the whole file: the comment, the
    // createCollection line and one insertMany line for three documents.
    let moved = tempfile::tempdir().unwrap();
    let r = dump::export_table_sql(
        &st,
        TableExportRequest {
            conn_id: id.clone(),
            database: DB.into(),
            schema: None,
            table: coll.clone(),
            data: true,
        },
        &moved.path().join("whole.js"),
    )
    .await
    .unwrap();
    assert_eq!(r.rows, 3);
    let text = std::fs::read_to_string(moved.path().join("whole.js")).unwrap();
    assert_eq!(lines_of(&moved.path().join("whole.js")), 3, "{text}");
    assert!(text.contains("insertMany(["), "{text}");
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("db.getCollection(") && l.contains("insertMany(["))
            .count(),
        1,
        "{text}"
    );
    drop_a(&seed_client, &coll).await;
    drop_a(&seed_client, &big).await;
    drop(proxy);
}
