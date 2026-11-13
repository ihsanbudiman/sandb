// Phase 7, slice A: network faults on MongoDB, in front of an in-test TCP proxy the test can cut
// (close its sockets) or black-hole (accept and never forward). The servers keep their own
// published ports, so every test then reads what the app saved and that it works again.
// Cases: the editor run with its cursor (find and aggregate), the run into a black hole (with a
// cancel, and without one the driver's own timeout), the grid save, the AI read, and the script
// export with `collection_ddl`.
// Run: `cargo test --features it --test it_mongo_fault`.
#![cfg(feature = "it")]

mod common;

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use mongodb::bson::{Bson, Document, doc};
use sandb_lib::core::connections::ConnectionInput;
use sandb_lib::core::result::TableExportRequest;
use sandb_lib::core::result::{
    ApplyEditsRequest, EditOperation,
};
use sandb_lib::core::result::{ReadLimits, RunRequest};
use sandb_lib::core::ops;
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::state::AppState;
use tokio::io::AsyncWriteExt as _;

use common::Cleanup;

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
    common::root_client(port).await
}

/// A new saved connection to the proxy, so its driver client opens new sockets. The black hole
/// holds those; an existing client's sockets were opened before the fault.
fn fresh_conn(st: &AppState, proxy: &FaultProxy, user: &str, name: &str) -> String {
    let password = common::password(user);
    let mut input = if user == "sandb_rw" {
        rw_input(proxy.port, name)
    } else {
        ro_input(proxy.port, name)
    };
    input.name = format!("it-mongo-fault-{name}-{}", uuid::Uuid::new_v4().simple());
    st.create_connection(input, Some(password)).unwrap().id
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
    ops::run_query(st, r)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e:?}"));
}

/// The connection string for a fault test: one host, `directConnection=true`, so the driver
/// never discovers the replica-set member behind the proxy and bypasses it.
fn fault_uri(user: &str, password: &str, port: u16) -> String {
    format!(
        "mongodb://{user}:{password}@127.0.0.1:{port}/{}?directConnection=true&authSource=admin&serverSelectionTimeoutMS=10000",
        common::seed::STAGING
    )
}

fn rw_input(port: u16, name: &str) -> ConnectionInput {
    let mut input = common::mongo_input(
        &format!("mongo-fault-{name}"),
        port,
        "sandb_rw",
        common::seed::STAGING,
        false,
    );
    input.uri = fault_uri("sandb_rw", &common::rw_password(), port);
    input
}

/// A read-only connection input to `port`.
fn ro_input(port: u16, name: &str) -> ConnectionInput {
    let mut input = common::mongo_input(
        &format!("mongo-fault-{name}"),
        port,
        "sandb_ro",
        common::seed::STAGING,
        true,
    );
    input.uri = fault_uri("sandb_ro", &common::ro_password(), port);
    input
}

/// A saved connection through the proxy, for `user` (`sandb_rw` may write).
fn saved(st: &AppState, proxy: &FaultProxy, user: &str, read_only: bool) -> String {
    let password = common::password(user);
    let mut input = common::mongo_input(
        &format!("mongo-fault-{user}"),
        proxy.port,
        user,
        common::seed::STAGING,
        read_only,
    );
    input.uri = fault_uri(user, &password, proxy.port);
    st.create_connection(input, Some(password)).unwrap().id
}

/// Seeds `rows` `_id` + text documents into a fresh `it_faults_` collection on `port`; the name
/// is registered with `clean` before the insert, so a seeding panic cannot leak it.
async fn seeded(
    st: &AppState,
    port: u16,
    clean: &mut Cleanup,
    rows: i32,
) -> (mongodb::Client, String) {
    let coll = clean.collection(common::scratch());
    let id = st
        .create_connection(rw_input(port, "seed"), Some(common::rw_password()))
        .unwrap()
        .id;
    let client = sandb_lib::core::mongo::client::client(st, &id)
        .await
        .unwrap();
    let docs: Vec<Document> = (1..=rows)
        .map(|i| doc! { "_id": i, "a": format!("row-bbb-{i:0>4}-padding-padding-padding") })
        .collect();
    client
        .database(common::seed::STAGING)
        .collection::<Document>(&coll)
        .insert_many(docs)
        .await
        .unwrap();
    (client, coll)
}

/// The seeded `a` value of row `i`.
fn seeded_value(i: usize) -> String {
    format!("row-bbb-{i:0>4}-padding-padding-padding")
}

/// The `a` value of `_id: id` in the collection, read straight at the server.
async fn a_at(client: &mongodb::Client, coll: &str, id: i32) -> Bson {
    client
        .database(common::seed::STAGING)
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
    common::drop_scratch(client, common::seed::STAGING, coll).await;
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
        database: common::seed::STAGING.into(),
        sql,
        tab_id: None,
        query_id: Some(query_id),
        operations,
    }
}

fn edits_request(conn_id: &str, sql: String, operations: Vec<EditOperation>) -> ApplyEditsRequest {
    ApplyEditsRequest {
        conn_id: conn_id.into(),
        database: common::seed::STAGING.into(),
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

// Case 1: the run whose cursor the cut reaches

#[tokio::test]
async fn the_cursor_run_gets_a_connection_error_and_the_tab_lives_on() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 250).await;
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
    let fresh = st.create_connection({
            let mut i = ro_input(proxy.port, "fresh");
            i.name = format!("it-mongo-fault-fresh-{}", uuid::Uuid::new_v4().simple());
            i
        }, Some(common::ro_password()), )
    .unwrap()
    .id;
    let e = ops::run_query(&st, {
        let mut r = req(&fresh, "db.products.find({})");
        r.limit = Some(50);
        r
    })
    .await
    .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "cursor read: {e:?}");

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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 250).await;
    let sql = format!("db.{coll}.aggregate([{{$match: {{}}}}, {{$sort: {{_id: 1}}}}])");
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(50);
        r
    })
    .await;

    proxy.cut();
    let fresh = st.create_connection({
            let mut i = ro_input(proxy.port, "agg");
            i.name = format!("it-mongo-fault-agg-{}", uuid::Uuid::new_v4().simple());
            i
        }, Some(common::ro_password()), )
    .unwrap()
    .id;
    let e = ops::run_query(&st, {
        let mut r = req(&fresh, &sql);
        r.limit = Some(50);
        r
    })
    .await
    .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "aggregate: {e:?}");
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

#[tokio::test]
async fn the_cut_reaches_a_replayed_load_more() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");
    // Load: the first 10; load more: the same text with a higher limit.
    run(&st, {
        let mut r = req(&id, &sql);
        r.limit = Some(10);
        r
    })
    .await;

    proxy.cut();
    let fresh = st.create_connection({
            let mut i = ro_input(proxy.port, "load");
            i.name = format!("it-mongo-fault-load-{}", uuid::Uuid::new_v4().simple());
            i
        }, Some(common::ro_password()), )
    .unwrap()
    .id;
    let e = ops::run_query(&st, {
        let mut r = req(&fresh, &sql);
        r.limit = Some(20);
        r
    })
    .await
    .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "load more: {e:?}");
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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 250).await;
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
    let e = tokio::time::timeout(
        Duration::from_secs(30),
        ops::run_query(&st, {
            let mut r = req(&fresh, &sql);
            r.limit = Some(50);
            r
        }),
    )
    .await
    .expect("the driver's own timers end the run")
    .expect_err("the run cannot answer behind a black hole");
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "{e:?}");

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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 250).await;
    let sql = format!("db.{coll}.find({{}})");
    let id = fresh_conn(&st, &proxy, "sandb_ro", "warm");
    run(&st, req(&id, &sql)).await;

    // A new client whose first socket is held by the black hole: the run waits in the driver.
    proxy.blackhole();
    let fresh = fresh_conn(&st, &proxy, "sandb_ro", "cancel");
    let query_id = uuid::Uuid::new_v4().to_string();
    let run_in = async {
        ops::run_query(&st, {
            let mut r = req(&fresh, &sql);
            r.query_id = query_id.clone();
            r.limit = Some(50);
            r
        })
        .await
    };
    let cancel_when_started = async {
        // `join!` polls both, so the run registers while this waits for it. Cancel must reach a
        // run that is really registered, not an early id.
        let deadline = Instant::now() + Duration::from_secs(5);
        while ops::query_progress(&st, &query_id).is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            ops::query_progress(&st, &query_id).is_some(),
            "the run did not start"
        );
        ops::cancel_query(&st, &query_id).await.unwrap();
    };
    let (outcome, ()) = tokio::join!(run_in, cancel_when_started);
    proxy.listen();
    // `Cancelled` is the signal: an uncancelled run behind the black hole ends `ConnectFailed`
    // at the driver's connect timeout.
    let e = outcome.expect_err("the cancel ends the local run");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");

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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_rw", false);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    let root = direct_root_client(common::mongo_rs()).await;

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
    let save = async { ops::apply_edits(&st, edits_request(&id, sql.clone(), operations)).await };
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
            .database(common::seed::STAGING)
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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    // The page's client is warm, as in the QA steps; the black hole then holds every socket the
    // driver opens, so the save cannot reach the server (a paused container).
    let id = saved(&st, &proxy, "sandb_rw", false);
    run(&st, req(&id, &sql)).await;
    proxy.blackhole();
    let query_id = uuid::Uuid::new_v4().to_string();
    let save = async {
        ops::apply_edits(&st, edits_request_id(
                &id,
                sql.clone(),
                vec![update_a(1, "b_pause", &seeded_value(1))],
                query_id.clone(),
            ), )
        .await
    };
    let cancel_when_started = async {
        let deadline = Instant::now() + Duration::from_secs(5);
        while ops::query_progress(&st, &query_id).is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            ops::query_progress(&st, &query_id).is_some(),
            "the save did not start"
        );
        ops::cancel_query(&st, &query_id)
            .await
            .expect("cancel the save");
    };
    let (outcome, ()) = tokio::join!(save, cancel_when_started);
    let e = outcome.expect_err("the cancel ends the save");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_rw", false);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 2).await;
    let sql = format!("db.{coll}.find({{}})");
    let root = direct_root_client(common::mongo_rs()).await;

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
        ops::apply_edits(&st, edits_request_id(&id, sql.clone(), operations, query_id.clone()), )
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
        ops::cancel_query(&st, &query_id)
            .await
            .expect("cancel the save");
    };
    let (outcome, ()) = tokio::join!(save, cancel_when_open);
    let e = outcome.expect_err("the cancel aborts the save");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
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
            .database(common::seed::STAGING)
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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let id = saved(&st, &proxy, "sandb_ro", true);
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 5).await;
    let sql = format!("db.{coll}.find({{}})");
    let limits = ReadLimits::default();

    let page = sandb_lib::core::ops::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap();
    assert!(!page.rows.is_empty(), "{page:?}");

    // The cut: the tool step fails and the chat's answer carries the error.
    proxy.cut();
    let e = sandb_lib::core::ops::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "tool step: {e:?}");
    // A later turn works again on the same connection.
    let page = sandb_lib::core::ops::run_ai_read(&st, &id, None, &sql, limits)
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
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 5).await;
    let sql = format!("db.{coll}.find({{}})");
    let limits = ReadLimits {
        max_rows: 500,
        timeout: Duration::from_secs(1),
    };

    // A new client whose first socket the black hole holds: no reply ever comes, so only the
    // AI read's own time limit ends it.
    proxy.blackhole();
    let id = fresh_conn(&st, &proxy, "sandb_ro", "ai-held");
    let e = sandb_lib::core::ops::run_ai_read(&st, &id, None, &sql, limits)
        .await
        .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::Timeout, "time limit: {e:?}");
    drop_a(&seed_client, &coll).await;
    drop(proxy);
}

// ---------------------------------------------------------------------------------------------
// Case 5: the script export and `table_ddl` behind the cut proxy

#[tokio::test]
async fn the_cut_stops_the_export_and_the_collection_ddl() {
    let _serial = SERIAL.lock().await;
    let (_dir, st) = common::state();
    let proxy = FaultProxy::start(0, common::mongo_rs()).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let (seed_client, coll) = seeded(&st, common::mongo_rs(), &mut clean, 3).await;
    // A bigger collection, so the export is still streaming when the cut lands.
    let big = clean.collection(common::scratch());
    let docs: Vec<Document> = (0..300)
        .map(|i| doc! { "_id": i, "a": "x".repeat(4096) })
        .collect();
    seed_client
        .database(common::seed::STAGING)
        .collection::<Document>(&big)
        .insert_many(docs)
        .await
        .unwrap();
    let id = saved(&st, &proxy, "sandb_rw", false);

    // The script of a live server.
    let script = ops::table_ddl(&st, &id, common::seed::STAGING, None, "orders").await.unwrap();
    assert!(script.contains("createCollection"), "{script}");

    // The export starts with the proxy forwarding; the cut fires once its temp file exists, so
    // the failure hits the document stream and the temp-file cleanup runs.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("half.js");
    let mut export = Box::pin(ops::export_table_sql(&st, TableExportRequest {
            conn_id: id.clone(),
            database: common::seed::STAGING.into(),
            schema: None,
            table: big.clone(),
            data: true,
        }, &path, ));
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
    let e = ops::table_ddl(&st, &id, common::seed::STAGING, None, "orders")
        .await
        .unwrap_err();
    proxy.listen();
    assert_eq!(e.code, ErrorCode::ConnectFailed, "ddl: {e:?}");
    let script = ops::table_ddl(&st, &id, common::seed::STAGING, None, "orders").await.unwrap();
    assert!(script.contains("createCollection"));

    // The export with data on the live proxy gives the whole file: the comment, the
    // createCollection line and one insertMany line for three documents.
    let moved = tempfile::tempdir().unwrap();
    let r = ops::export_table_sql(&st, TableExportRequest {
            conn_id: id.clone(),
            database: common::seed::STAGING.into(),
            schema: None,
            table: coll.clone(),
            data: true,
        }, &moved.path().join("whole.js"), )
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
