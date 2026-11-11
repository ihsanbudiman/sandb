//! Integration tests for `core::mongo::write`: every write and schema statement runs against the
//! sandb-test docker stack (`pnpm db:up`) through the same `execute` the run path uses.
//! Run: `cargo test --features it --test it_mongo_write`.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mongodb::bson::{Bson, Document, doc};
use sandb_lib::core::connections::{self, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::mongo::command::Command;
use sandb_lib::core::mongo::write::WriteOutcome;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

const RS: u16 = 57017;
const STANDALONE: u16 = 57018;

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

fn rw_password() -> String {
    env_test()
        .get("SANDB_RW_PASSWORD")
        .cloned()
        .expect("SANDB_RW_PASSWORD in testdb/.env.test")
}

fn ro_password() -> String {
    env_test()
        .get("SANDB_RO_PASSWORD")
        .cloned()
        .expect("SANDB_RO_PASSWORD in testdb/.env.test")
}

fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    (dir, st)
}

/// Read-write user on `shop_staging`.
fn rw(port: u16) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-mongo-write-{port}"),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: "shop_staging".into(),
        user: "sandb_rw".into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: "admin".into(),
        topology: None,
    }
}

/// Read-only user; only the server's own roles stop a write.
fn ro(port: u16) -> ConnectionInput {
    let mut i = rw(port);
    i.name = format!("it-mongo-write-ro-{port}");
    i.user = "sandb_ro".into();
    i.read_only = true;
    i
}

/// A fresh scratch collection name, `it_scratch_` + 32 hex characters like a simple uuid.
fn scratch() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "it_scratch_{:016x}{:016x}",
        nanos,
        N.fetch_add(1, Ordering::Relaxed) as u128
    )
}

/// A `sandb_rw` client, opened through the connection's own `mongo::client::client`.
async fn rw_client(st: &AppState) -> mongodb::Client {
    let c = connections::create(st, rw(RS), Some(rw_password())).unwrap();
    mongo::client::client(st, &c.id).await.unwrap()
}

/// The parsed command of one statement.
fn command(text: &str) -> Command {
    let statements = mongo::parse::parse(text).expect("parse");
    assert_eq!(statements.len(), 1, "{text}");
    statements.into_iter().next().unwrap().command
}

/// Parses one statement and runs it as a write on `shop_staging`.
async fn run(client: &mongodb::Client, text: &str) -> WriteOutcome {
    mongo::write::execute(client, "shop_staging", &command(text), "it-mongo-write")
        .await
        .unwrap()
}

/// Runs one statement and returns its error.
async fn execute_err(client: &mongodb::Client, text: &str) -> AppError {
    mongo::write::execute(client, "shop_staging", &command(text), "it-mongo-write")
        .await
        .expect_err(text)
}

/// The document count of a collection.
async fn count(client: &mongodb::Client, coll: &str) -> u64 {
    client
        .database("shop_staging")
        .collection::<Document>(coll)
        .count_documents(doc! {})
        .await
        .unwrap()
}

/// The index names of a collection.
async fn index_names(client: &mongodb::Client, coll: &str) -> Vec<String> {
    client
        .database("shop_staging")
        .collection::<Document>(coll)
        .list_index_names()
        .await
        .unwrap()
}

/// Drops the scratch names a test made when the test ends, also while a failed test unwinds.
/// `Drop` cannot use the test's client or runtime (both are being torn down after a panic), so it
/// opens its own root client on its own thread: root can drop every name a test creates.
struct Cleanup {
    uri: String,
    collections: Vec<String>,
}

impl Cleanup {
    /// A guard for scratch names on the server at `port`.
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
                            .database("shop_staging")
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

/// Drops a scratch collection; one that was never created is fine. The tests drop before they
/// assert, so a failed assert cannot leak a collection.
async fn drop_scratch(client: &mongodb::Client, coll: &str) {
    if let Err(e) = client
        .database("shop_staging")
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

#[tokio::test]
async fn insert_one_inserts_one() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let out = run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(count, 1);
    assert_eq!(out.affected, 1);
    assert_eq!(out.document, None);
    assert_eq!(out.notice, format!("insertOne on {coll}: inserted 1"));
}

#[tokio::test]
async fn insert_many_inserts_three() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let out = run(
        &client,
        &format!("db.{coll}.insertMany([{{a: 1}}, {{a: 2}}, {{a: 3}}])"),
    )
    .await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(count, 3);
    assert_eq!(out.affected, 3);
    assert_eq!(out.notice, format!("insertMany on {coll}: inserted 3"));
}

#[tokio::test]
async fn update_many_reports_matched_and_modified() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(
        &client,
        &format!("db.{coll}.insertMany([{{n: 1}}, {{n: 2}}, {{n: 3}}])"),
    )
    .await;
    let text = format!("db.{coll}.updateMany({{n: {{$gte: 2}}}}, {{$set: {{done: true}}}})");
    let first = run(&client, &text).await;
    let second = run(&client, &text).await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(
        first.notice,
        format!("updateMany on {coll}: matched 2, modified 2")
    );
    assert_eq!(first.affected, 2);
    // The second run matches the same documents but changes nothing.
    assert_eq!(
        second.notice,
        format!("updateMany on {coll}: matched 2, modified 0")
    );
    assert_eq!(second.affected, 0);
    assert_eq!(count, 3);
}

#[tokio::test]
async fn update_one_upserts_when_nothing_matches() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(&client, &format!("db.{coll}.insertOne({{n: 1}})")).await;
    let out = run(
        &client,
        &format!("db.{coll}.updateOne({{n: 99}}, {{$set: {{n: 99}}}}, {{upsert: true}})"),
    )
    .await;
    let count = count(&client, &coll).await;
    let upserted = client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .find_one(doc! { "n": 99 })
        .await
        .unwrap();
    drop_scratch(&client, &coll).await;
    assert_eq!(
        out.notice,
        format!("updateOne on {coll}: matched 0, modified 0, upserted 1")
    );
    assert_eq!(out.affected, 1);
    assert_eq!(count, 2);
    assert!(upserted.is_some(), "the upserted document exists");
}

#[tokio::test]
async fn replace_one_replaces_the_document() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(&client, &format!("db.{coll}.insertOne({{_id: 1, a: 1}})")).await;
    let out = run(
        &client,
        &format!("db.{coll}.replaceOne({{_id: 1}}, {{a: 2}})"),
    )
    .await;
    let replaced = client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .find_one(doc! { "_id": 1 })
        .await
        .unwrap();
    drop_scratch(&client, &coll).await;
    assert_eq!(
        out.notice,
        format!("replaceOne on {coll}: matched 1, modified 1")
    );
    assert_eq!(out.affected, 1);
    assert_eq!(replaced, Some(doc! { "_id": 1, "a": 2 }));
}

#[tokio::test]
async fn find_one_and_update_returns_the_new_document() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(&client, &format!("db.{coll}.insertOne({{n: 1}})")).await;
    let after = run(
        &client,
        &format!(
            "db.{coll}.findOneAndUpdate({{n: 1}}, {{$inc: {{n: 1}}}}, {{returnDocument: 'after'}})"
        ),
    )
    .await;
    let none = run(
        &client,
        &format!("db.{coll}.findOneAndUpdate({{n: 99}}, {{$inc: {{n: 1}}}})"),
    )
    .await;
    drop_scratch(&client, &coll).await;
    assert_eq!(
        after.document.as_ref().unwrap().get("n"),
        Some(&Bson::Int32(2)),
        "returnDocument: 'after' returns the modified document"
    );
    assert_eq!(after.affected, 1);
    assert_eq!(
        after.notice,
        format!("findOneAndUpdate on {coll}: 1 document")
    );
    assert_eq!(none.document, None);
    assert_eq!(none.affected, 0);
    assert_eq!(
        none.notice,
        format!("findOneAndUpdate on {coll}: no document matched")
    );
}

#[tokio::test]
async fn delete_many_reports_the_count() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(
        &client,
        &format!(
            "db.{coll}.insertMany([{{gone: true}}, {{gone: true}}, {{gone: true}}, {{n: 1}}])"
        ),
    )
    .await;
    let out = run(&client, &format!("db.{coll}.deleteMany({{gone: true}})")).await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(out.notice, format!("deleteMany on {coll}: deleted 3"));
    assert_eq!(out.affected, 3);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn bulk_write_runs_mixed_models() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let out = run(
        &client,
        &format!(
            "db.{coll}.bulkWrite([\
              {{insertOne: {{document: {{kind: 'bw', n: 1}}}}}}, \
              {{insertMany: {{documents: [{{kind: 'bw', n: 2}}, {{kind: 'bw', n: 3}}]}}}}, \
              {{updateMany: {{filter: {{kind: 'bw'}}, update: {{$set: {{touched: true}}}}}}}}, \
              {{deleteOne: {{filter: {{kind: 'bw', n: 1}}}}}}])"
        ),
    )
    .await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(
        out.notice,
        format!("bulkWrite on {coll}: inserted 3, matched 3, modified 3, deleted 1, upserted 0")
    );
    assert_eq!(out.affected, 7); // inserted + modified + deleted + upserted
    assert_eq!(count, 2);
}

/// With `ordered: false` a failing operation does not stop the rest; the first error is reported
/// at the end.
#[tokio::test]
async fn bulk_write_unordered_continues_after_a_failure() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let e = execute_err(
        &client,
        &format!(
            "db.{coll}.bulkWrite([\
              {{insertOne: {{document: {{_id: 7}}}}}}, \
              {{insertOne: {{document: {{_id: 7}}}}}}, \
              {{insertOne: {{document: {{_id: 8}}}}}}], {{ordered: false}})"
        ),
    )
    .await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    // A failed bulk write is the server's write error: its own message, with the code as label.
    assert!(e.message.contains("E11000 duplicate key error"), "{e:?}");
    assert_eq!(
        e.server_code.as_deref(),
        Some("MongoDB error 11000"),
        "{e:?}"
    );
    assert_eq!(e.detail.as_deref(), Some("MongoDB error 11000"), "{e:?}");
    assert_eq!(count, 2, "the operations after the failure still ran");
}

/// With `ordered: true` (the default) the first failure stops the run.
#[tokio::test]
async fn bulk_write_ordered_stops_at_the_first_failure() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let e = execute_err(
        &client,
        &format!(
            "db.{coll}.bulkWrite([\
              {{insertOne: {{document: {{_id: 7}}}}}}, \
              {{insertOne: {{document: {{_id: 7}}}}}}, \
              {{insertOne: {{document: {{_id: 8}}}}}}])"
        ),
    )
    .await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert!(e.message.contains("E11000 duplicate key error"), "{e:?}");
    assert_eq!(
        e.server_code.as_deref(),
        Some("MongoDB error 11000"),
        "{e:?}"
    );
    assert_eq!(count, 1, "the operations after the failure did not run");
}

#[tokio::test]
async fn create_index_with_a_name_then_drop_it() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let created = run(
        &client,
        &format!("db.{coll}.createIndex({{a: 1}}, {{name: 'it_a'}})"),
    )
    .await;
    let indexes = index_names(&client, &coll).await;
    let dropped = run(&client, &format!("db.{coll}.dropIndex('it_a')")).await;
    let after = index_names(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(
        created.notice,
        format!("createIndex on {coll}: created it_a")
    );
    assert_eq!(created.affected, 0);
    assert!(indexes.contains(&"it_a".to_string()), "{indexes:?}");
    assert_eq!(dropped.notice, format!("dropIndex on {coll}: dropped it_a"));
    assert!(!after.contains(&"it_a".to_string()), "{after:?}");
}

#[tokio::test]
async fn create_indexes_then_drop_them() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let created = run(
        &client,
        &format!("db.{coll}.createIndexes([{{a: 1}}, {{b: -1}}])"),
    )
    .await;
    let dropped = run(&client, &format!("db.{coll}.dropIndexes()")).await;
    let after = index_names(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(
        created.notice,
        format!("createIndexes on {coll}: created a_1, b_-1")
    );
    assert_eq!(
        dropped.notice,
        format!("dropIndexes on {coll}: dropped all but _id")
    );
    assert_eq!(after, vec!["_id_".to_string()]);
}

#[tokio::test]
async fn rename_collection_then_drop_the_rename() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let renamed = format!("{coll}_renamed");
    clean.collection(&renamed);
    run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let out = run(
        &client,
        &format!("db.{coll}.renameCollection('{renamed}', true)"),
    )
    .await;
    let moved = count(&client, &renamed).await;
    let gone = count(&client, &coll).await;
    let drop_out = run(&client, &format!("db.{renamed}.drop()")).await;
    drop_scratch(&client, &coll).await;
    drop_scratch(&client, &renamed).await;
    assert_eq!(out.notice, format!("renameCollection: {coll} -> {renamed}"));
    assert_eq!(out.affected, 0);
    assert_eq!(moved, 1, "the documents moved to the new name");
    assert_eq!(gone, 0, "the old name is gone");
    assert_eq!(drop_out.notice, format!("drop: dropped {renamed}"));
}

/// An option the method does not know is refused before anything runs.
#[tokio::test]
async fn unknown_option_is_refused_before_anything_runs() {
    let (_d, st) = state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let insert = execute_err(
        &client,
        &format!("db.{coll}.insertMany([{{a: 1}}], {{bogus: 1}})"),
    )
    .await;
    let update = execute_err(
        &client,
        &format!("db.{coll}.updateOne({{a: 1}}, {{$set: {{b: 1}}}}, {{nope: true}})"),
    )
    .await;
    let count = count(&client, &coll).await;
    drop_scratch(&client, &coll).await;
    assert_eq!(insert.code, ErrorCode::InvalidInput, "{insert:?}");
    assert_eq!(insert.message, "Unknown insertMany option: bogus");
    assert_eq!(update.code, ErrorCode::InvalidInput, "{update:?}");
    assert_eq!(update.message, "Unknown updateOne option: nope");
    assert_eq!(count, 0, "nothing was inserted");
}

/// The editor's read-only check is the first barrier; the server's roles are the second. `execute`
/// runs after the first barrier, so a `sandb_ro` write must be refused by the server itself.
#[tokio::test]
async fn read_only_user_is_refused_by_the_server() {
    for port in [RS, STANDALONE] {
        let (_d, st) = state();
        let c = connections::create(&st, ro(port), Some(ro_password())).unwrap();
        let client = mongo::client::client(&st, &c.id).await.unwrap();
        let mut clean = Cleanup::new(port);
        let coll = clean.collection(scratch());
        let e = execute_err(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
        assert_eq!(
            e.server_code.as_deref(),
            Some("MongoDB error 13"), // Unauthorized
            "{port}: {e:?}"
        );
    }
}
