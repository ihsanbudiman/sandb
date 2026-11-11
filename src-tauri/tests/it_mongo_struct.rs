//! Integration tests for the MongoDB structure work: collection properties, the schema changes
//! the properties view runs, the mongosh script of a collection and the Extended JSON export.
//! Runs against the sandb-test docker stack (`pnpm db:up`), `mongo-rs` on 57017 with the seed of
//! `testdb/seed/mongo/01_data.js`. Run: `cargo test --features it --test it_mongo_struct`.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;

use futures_util::TryStreamExt;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, doc};
use sandb_lib::core::connections::{self, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::db::dump::{self, TableExportRequest};
use sandb_lib::core::db::export::{
    self, DocumentExportRequest, DocumentFormat, EjsonFlavor, ExportRequest,
};
use sandb_lib::core::db::introspect::{
    self, MongoCollectionInfo, MongoCollectionKind, TableProperties,
};
use sandb_lib::core::db::query::{self, RunRequest};
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

const RS: u16 = 57017;

/// Every test reads the seed of one small Mongo, and one of them streams `big_docs` (about
/// 200 MB): they take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A saved connection to `shop_staging` as `user` (`sandb_ro` or `sandb_rw`).
fn saved(st: &AppState, user: &str, read_only: bool) -> String {
    let key = if user == "sandb_rw" {
        "SANDB_RW_PASSWORD"
    } else {
        "SANDB_RO_PASSWORD"
    };
    let password = env_test().get(key).cloned().expect("password in .env.test");
    let input = ConnectionInput {
        name: format!("it-mongo-struct-{user}"),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: RS,
        database: "shop_staging".into(),
        user: user.into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only,
        version: None,
        uri: String::new(),
        auth_source: "admin".into(),
        topology: None,
    };
    connections::create(st, input, Some(password)).unwrap().id
}

/// The stack's root user, whose roles reach every database: `sandb_rw` is `readWrite` on
/// `shop_staging` and `analytics` only, so it cannot `collMod` or create a database.
fn root(st: &AppState) -> String {
    let password = env_test()
        .get("MONGO_INITDB_ROOT_PASSWORD")
        .cloned()
        .expect("MONGO_INITDB_ROOT_PASSWORD in testdb/.env.test");
    let mut input = ConnectionInput {
        name: "it-mongo-struct-root".into(),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: RS,
        database: "shop_staging".into(),
        user: "root".into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: "admin".into(),
        topology: None,
    };
    input.name = format!("it-mongo-struct-root-{}", uuid::Uuid::new_v4().simple());
    connections::create(st, input, Some(password)).unwrap().id
}

fn req(conn_id: &str, database: Option<&str>, text: &str) -> RunRequest {
    RunRequest {
        conn_id: conn_id.into(),
        database: database.map(str::to_string),
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

async fn run_err(st: &AppState, r: RunRequest) -> AppError {
    query::run(st, r).await.expect_err("the run must fail")
}

/// A fresh scratch collection name, `it_scratch_` + 32 hex characters.
fn scratch() -> String {
    format!("it_scratch_{}", uuid::Uuid::new_v4().simple())
}

/// A fresh scratch database name, dropped by each test that creates one.
fn scratch_db() -> String {
    format!("it_scratch_db_{}", uuid::Uuid::new_v4().simple())
}

/// Drops the scratch names a test made when the test ends, also while a failed test unwinds.
/// `Drop` cannot use the test's client or runtime (both are being torn down after a panic), so it
/// opens its own root client on its own thread: root can drop every name a test creates.
struct Cleanup {
    uri: String,
    collections: Vec<String>,
    databases: Vec<String>,
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
            databases: Vec::new(),
        }
    }

    /// Tracks `coll` and returns it.
    fn collection(&mut self, coll: impl Into<String>) -> String {
        let coll = coll.into();
        self.collections.push(coll.clone());
        coll
    }

    /// Tracks `db` and returns it.
    fn database(&mut self, db: impl Into<String>) -> String {
        let db = db.into();
        self.databases.push(db.clone());
        db
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
                    for db in databases {
                        let _ = client.database(&db).drop().await;
                    }
                });
            })
            .map(|h| h.join());
        let _ = joined;
    }
}

/// Quotes a collection name for the mongosh text of a test.
fn q(name: &str) -> String {
    format!("db.getCollection(\"{name}\")")
}

/// The BSON bytes of a document, with every NaN normalized, so a NaN equals a NaN.
fn canonical(v: &Bson) -> Vec<u8> {
    let mut out = Vec::new();
    doc! { "v": normalize(v) }.to_writer(&mut out).unwrap();
    out
}

fn normalize(v: &Bson) -> Bson {
    match v {
        Bson::Double(d) if d.is_nan() => Bson::String("<NaN>".into()),
        Bson::Document(d) => {
            Bson::Document(d.iter().map(|(k, v)| (k.clone(), normalize(v))).collect())
        }
        Bson::Array(a) => Bson::Array(a.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

/// The documents of a collection.
async fn documents(client: &mongodb::Client, db: &str, coll: &str) -> Vec<Document> {
    client
        .database(db)
        .collection::<Document>(coll)
        .find(doc! {})
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap()
}

/// The documents as BSON bytes, sorted: the same documents in any order.
fn canonicals(docs: &[Document]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = docs
        .iter()
        .map(|d| canonical(&Bson::Document(d.clone())))
        .collect();
    out.sort();
    out
}

/// One document with every BSON type the export must keep, plus odd keys and nesting.
fn every_type() -> Document {
    doc! {
        "_id": ObjectId::parse_str("65f100000000000000000001").unwrap(),
        "int32": Bson::Int32(i32::MIN),
        "int64": Bson::Int64(9_007_199_254_740_993),
        "long_min": Bson::Int64(i64::MIN),
        "double": Bson::Double(1.234_567_890_123),
        "double_whole": Bson::Double(42.0),
        "double_neg_zero": Bson::Double(-0.0),
        "double_nan": Bson::Double(f64::NAN),
        "double_inf": Bson::Double(f64::INFINITY),
        "double_neg_inf": Bson::Double(f64::NEG_INFINITY),
        "double_tiny": Bson::Double(5e-324),
        "decimal": Bson::Decimal128("12345.6789".parse::<Decimal128>().unwrap()),
        "decimal_nan": Bson::Decimal128("NaN".parse::<Decimal128>().unwrap()),
        "string": "héllo \"wörld\" \\ 😀",
        "empty_string": "",
        "bool": true,
        "null": Bson::Null,
        "date": Bson::DateTime(DateTime::from_millis(1_700_000_000_123)),
        "date_before_epoch": Bson::DateTime(DateTime::from_millis(-1)),
        "date_year_10000": Bson::DateTime(DateTime::from_millis(253_402_300_800_000)),
        "timestamp": Bson::Timestamp(Timestamp { time: 1_790_000_000, increment: 7 }),
        "regex": Bson::RegularExpression(Regex {
            pattern: "^sandb-[a-z]+$".into(),
            options: "i".into(),
        }),
        // A pattern a `/.../flags` literal cannot hold, so it comes back as `RegExp(...)`.
        "regex_slash": Bson::RegularExpression(Regex {
            pattern: "a/b[".into(),
            options: String::new(),
        }),
        "binary": Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![0, 1, 2, 253, 254, 255],
        }),
        "uuid": Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uuid::Uuid::parse_str("3b241101-e2bb-4255-8caf-4136c566a962")
                .unwrap()
                .as_bytes()
                .to_vec(),
        }),
        "uuid_old": Bson::Binary(Binary {
            subtype: BinarySubtype::UuidOld,
            bytes: vec![7; 16],
        }),
        "code": Bson::JavaScriptCode("function () { return 1; }".into()),
        "min": Bson::MinKey,
        "max": Bson::MaxKey,
        "document": { "a.b": 1, "$x": [Bson::MinKey], "": "empty key" },
        // JavaScript would read a quoted `"__proto__"` as the prototype, not as a field.
        "__proto__": { "__proto__": "own field" },
        "array": [1, 2.0, "three", {"four": 4}, [5]],
    }
}

#[tokio::test]
async fn properties_of_a_collection_and_of_a_view() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    run(
        &st,
        req(
            &id,
            None,
            &format!(
                "db.createCollection(\"{coll}\")\n\
                 {c}.insertMany([{{email: 'a@x', n: 1}}, {{email: 'b@x', n: 2}}])\n\
                 {c}.createIndex({{email: 1}}, {{unique: true, name: \"email_uniq\"}})",
                c = q(&coll)
            ),
        ),
    )
    .await;

    let p = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    let mongo = p.mongo.as_ref().expect("the mongo part");
    assert_eq!(mongo.kind, MongoCollectionKind::Collection);
    assert!(mongo.validator.is_none() && mongo.validation_level.is_none());
    // The seed has no options, and the SQL fields stay empty.
    assert_eq!(mongo.options, "{}");
    assert!(p.constraints.is_empty() && p.foreign_keys.is_empty() && p.references.is_empty());
    assert_eq!(mongo.stats.expect("stats").count, 2);

    let id_index = p.indexes.iter().find(|i| i.primary).expect("_id_");
    assert_eq!(id_index.name, "_id_");
    assert_eq!(id_index.columns, ["_id"]);
    assert_eq!(id_index.mongo.as_ref().unwrap().keys, "{\"_id\":1}");
    let index = p
        .indexes
        .iter()
        .find(|i| i.name == "email_uniq")
        .expect("index");
    assert_eq!(index.columns, ["email"]);
    assert!(index.unique && !index.primary && index.method.is_none());
    let info = index.mongo.as_ref().unwrap();
    assert_eq!(info.keys, "{\"email\":1}");
    assert!(
        !info.sparse && !info.hidden && info.ttl_seconds.is_none() && info.partial_filter.is_none()
    );
    assert!(info.options.is_none(), "{info:?}");
    assert_eq!(
        index.definition.as_deref(),
        Some(
            format!(
                "db.getCollection(\"{coll}\").createIndex({{\"email\": 1}}, {{\"name\": \"email_uniq\", \"unique\": true}})"
            )
            .as_str()
        )
    );

    // The seed view: no indexes and no stats, its options carry viewOn and pipeline.
    let view = introspect::table_properties(&st, &id, "shop_staging", None, "order_summary")
        .await
        .unwrap();
    assert!(view.indexes.is_empty());
    let mongo = view.mongo.expect("the mongo part");
    assert_eq!(mongo.kind, MongoCollectionKind::View);
    assert!(mongo.stats.is_none());
    assert!(
        mongo.options.contains("\"viewOn\": \"orders\"") && mongo.options.contains("\"pipeline\""),
        "{}",
        mongo.options
    );

    // The seed's time series collection.
    let series = introspect::table_properties(&st, &id, "analytics", None, "metrics")
        .await
        .unwrap();
    assert_eq!(
        series.mongo.expect("the mongo part").kind,
        MongoCollectionKind::Timeseries
    );

    // An unknown collection is `not_found`.
    let e = introspect::table_properties(&st, &id, "shop_staging", None, &coll[..8])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");

    run(&st, req(&id, None, &format!("{}.drop()", q(&coll)))).await;
}

#[tokio::test]
async fn index_changes_show_in_properties_and_drop() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let c = q(&coll);
    run(
        &st,
        req(
            &id,
            None,
            &format!(
                "db.createCollection(\"{coll}\")\n\
                 {c}.createIndex({{status: 1}}, {{name: \"status_partial\", unique: true, partialFilterExpression: {{status: {{$exists: true}}}}}})\n\
                 {c}.createIndex({{createdAt: 1}}, {{name: \"createdAt_ttl\", expireAfterSeconds: 3600}})
\
                 {c}.createIndex({{title: \"text\", body: \"text\"}}, {{name: \"text_idx\", weights: {{title: 5}}, default_language: \"spanish\"}})"
            ),
        ),
    )
    .await;

    let p = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    let partial = p
        .indexes
        .iter()
        .find(|i| i.name == "status_partial")
        .expect("the unique partial index");
    let info = partial.mongo.as_ref().unwrap();
    assert!(partial.unique);
    assert_eq!(
        info.partial_filter.as_deref(),
        Some("{\"status\":{\"$exists\":true}}")
    );
    assert_eq!(info.ttl_seconds, None);
    let ttl = p
        .indexes
        .iter()
        .find(|i| i.name == "createdAt_ttl")
        .expect("the TTL index");
    assert_eq!(ttl.mongo.as_ref().unwrap().ttl_seconds, Some(3600));
    assert!(!ttl.unique);
    // `weights` and `default_language` reach the server.
    let text = p
        .indexes
        .iter()
        .find(|i| i.name == "text_idx")
        .expect("the text index");
    assert_eq!(text.method.as_deref(), Some("text"));
    let options = text
        .mongo
        .as_ref()
        .unwrap()
        .options
        .as_deref()
        .unwrap_or("");
    assert!(options.contains("\"weights\""), "{options}");
    assert!(
        options.contains("\"default_language\":\"spanish\""),
        "{options}"
    );

    // Dropping by name: the other index stays.
    run(
        &st,
        req(&id, None, &format!("{c}.dropIndex(\"status_partial\")")),
    )
    .await;
    let p = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    assert!(
        p.indexes.iter().all(|i| i.name != "status_partial"),
        "{:?}",
        p.indexes
    );
    assert!(p.indexes.iter().any(|i| i.name == "createdAt_ttl"));

    run(&st, req(&id, None, &format!("{c}.drop()"))).await;
}

#[tokio::test]
async fn validator_is_set_and_removed() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = root(&st);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let c = q(&coll);
    run(
        &st,
        req(&id, None, &format!("db.createCollection(\"{coll}\")")),
    )
    .await;

    run(
        &st,
        req(
            &id,
            None,
            &format!(
                "db.runCommand({{collMod: \"{coll}\", validator: {{$jsonSchema: {{bsonType: \"object\", required: [\"a\"], properties: {{a: {{bsonType: \"int\"}}}}}}}}, validationLevel: \"moderate\", validationAction: \"warn\"}})"
            ),
        ),
    )
    .await;
    let mongo = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap()
        .mongo
        .unwrap();
    let validator = mongo.validator.expect("the validator text");
    assert!(validator.contains("\"$jsonSchema\""), "{validator}");
    assert!(
        validator.contains('\n'),
        "the validator is pretty: {validator}"
    );
    assert_eq!(mongo.validation_level.as_deref(), Some("moderate"));
    assert_eq!(mongo.validation_action.as_deref(), Some("warn"));

    // An empty validator removes it.
    run(
        &st,
        req(
            &id,
            None,
            &format!("db.runCommand({{collMod: \"{coll}\", validator: {{}}}})"),
        ),
    )
    .await;
    let mongo = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap()
        .mongo
        .unwrap();
    assert!(mongo.validator.is_none(), "{mongo:?}");

    run(&st, req(&id, None, &format!("{c}.drop()"))).await;
}

#[tokio::test]
async fn read_only_connection_refuses_schema_changes() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_ro", true);
    for text in [
        "db.orders.createIndex({status: 1})",
        "db.orders.dropIndex(\"_id_\")",
        "db.runCommand({collMod: \"orders\", validator: {}})",
        "db.dropDatabase()",
    ] {
        let e = run_err(&st, req(&id, None, text)).await;
        assert_eq!(e.code, ErrorCode::ConnectionReadOnly, "{text}: {e:?}");
    }
}

#[tokio::test]
async fn drop_database_removes_it_from_the_list() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = root(&st);
    let mut clean = Cleanup::new(RS);
    let db = clean.database(scratch_db());
    // A database appears in the list once it has a collection.
    run(
        &st,
        req(
            &id,
            Some(&db),
            &format!("{}.insertOne({{a: 1}})", q("keep")),
        ),
    )
    .await;
    let names = introspect::list_databases(&st, &id).await.unwrap();
    assert!(names.contains(&db), "{db} not in {names:?}");

    let r = query::run(&st, req(&id, Some(&db), "db.dropDatabase()"))
        .await
        .unwrap();
    assert_eq!(r.statements.len(), 1);
    let s = &r.statements[0];
    assert!(!s.is_read);
    assert_eq!(s.affected, Some(0));
    assert_eq!(s.row_count, 0);
    assert_eq!(s.sql, "db.dropDatabase()");
    assert!(
        r.notices
            .iter()
            .any(|n| n == &format!("dropDatabase: dropped {db}")),
        "{:?}",
        r.notices
    );

    let names = introspect::list_databases(&st, &id).await.unwrap();
    assert!(!names.contains(&db), "{db} still in {names:?}");
}

#[tokio::test]
async fn the_script_recreates_the_collection_in_another_database() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = root(&st);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let target = clean.database(scratch_db());
    let c = q(&coll);
    run(
        &st,
        req(
            &id,
            None,
            &format!(
                "db.createCollection(\"{coll}\", {{validator: {{$jsonSchema: {{bsonType: \"object\", required: [\"status\"], properties: {{status: {{bsonType: \"string\"}}}}}}}}, validationLevel: \"moderate\", validationAction: \"warn\"}})\n\
                 {c}.createIndex({{status: 1, createdAt: -1}}, {{name: \"status_created\", unique: true}})\n\
                 {c}.createIndex({{createdAt: 1}}, {{name: \"ttl\", expireAfterSeconds: 60}})\n\
                 {c}.insertOne({{status: \"paid\", createdAt: ISODate(\"2026-10-01T00:00:00Z\")}})"
            ),
        ),
    )
    .await;

    let ddl = dump::table_ddl(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    assert!(
        ddl.contains(&format!("db.createCollection(\"{coll}\", ")),
        "{ddl}"
    );
    assert!(ddl.contains("validationLevel"), "{ddl}");
    assert!(
        ddl.contains(&format!(
            "{c}.createIndex({{\"status\": 1, \"createdAt\": -1}}"
        )),
        "{ddl}"
    );
    // `_id_` comes with the collection.
    assert!(!ddl.contains("\"_id_\""), "{ddl}");

    // Without data the file is the script alone.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{coll}.js"));
    let r = dump::export_table_sql(
        &st,
        TableExportRequest {
            conn_id: id.clone(),
            database: "shop_staging".into(),
            schema: None,
            table: coll.clone(),
            data: false,
        },
        &path,
    )
    .await
    .unwrap();
    assert_eq!(r.rows, 0);
    let script_only = std::fs::read_to_string(&path).unwrap();
    assert!(!script_only.contains("insertMany("), "{script_only}");

    // The seed view: `db.createCollection` with its viewOn and pipeline.
    let view_ddl = dump::table_ddl(&st, &id, "shop_staging", None, "order_summary")
        .await
        .unwrap();
    assert!(view_ddl.contains("\"viewOn\": \"orders\""), "{view_ddl}");

    // The scripts run in the editor, on a database that does not exist yet.
    run(&st, req(&id, Some(&target), &ddl)).await;
    run(&st, req(&id, Some(&target), &view_ddl)).await;

    let source = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    let copy = introspect::table_properties(&st, &id, &target, None, &coll)
        .await
        .unwrap();
    assert_eq!(source.indexes, copy.indexes);
    let (source, copy) = (source.mongo.unwrap(), copy.mongo.unwrap());
    assert_eq!(source.kind, copy.kind);
    assert_eq!(source.validator, copy.validator);
    assert_eq!(source.validation_level, copy.validation_level);
    assert_eq!(source.validation_action, copy.validation_action);
    assert_eq!(source.options, copy.options);
    assert_eq!(copy.stats.expect("stats of an empty copy").count, 0);
    // The view came across too.
    let view = introspect::table_properties(&st, &id, &target, None, "order_summary")
        .await
        .unwrap();
    assert!(view.indexes.is_empty());
    assert_eq!(
        view.mongo.expect("the mongo part").kind,
        MongoCollectionKind::View
    );

    run(&st, req(&id, None, &format!("{c}.drop()"))).await;
    run(
        &st,
        req(&id, Some(&target), "db.runCommand({dropDatabase: 1})"),
    )
    .await;
}

/// The index options that change the index itself survive a replay: a projected wildcard index,
/// a text index with a language override and text version, and the geo options.
#[tokio::test]
async fn special_index_options_replay() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = root(&st);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let target = clean.database(scratch_db());
    let c = q(&coll);
    run(
        &st,
        req(
            &id,
            None,
            &format!(
                "db.createCollection(\"{coll}\")\n\
                 {c}.createIndex({{\"$**\": 1}}, {{name: \"wild\", wildcardProjection: {{\"a.b\": 1}}}})\n\
                 {c}.createIndex({{title: \"text\", body: \"text\"}}, {{name: \"text_lang\", weights: {{title: 5}}, default_language: \"spanish\", language_override: \"lang\", textIndexVersion: 3}})\n\
                 {c}.createIndex({{loc: \"2dsphere\"}}, {{name: \"sphere\", \"2dsphereIndexVersion\": 3}})\n\
                 {c}.createIndex({{loc2d: \"2d\"}}, {{name: \"flat\", bits: 30, min: -180, max: 180}})"
            ),
        ),
    )
    .await;

    let source = introspect::table_properties(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    let index = |name: &str| {
        source
            .indexes
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name} in {:?}", source.indexes))
    };
    let options = |name: &str| {
        index(name)
            .mongo
            .as_ref()
            .unwrap()
            .options
            .clone()
            .unwrap_or_default()
    };
    let wild = index("wild");
    assert_eq!(wild.method.as_deref(), Some("wildcard"));
    assert!(
        options("wild").contains("\"wildcardProjection\""),
        "{}",
        options("wild")
    );
    assert_eq!(index("text_lang").method.as_deref(), Some("text"));
    assert!(
        options("text_lang").contains("\"language_override\":\"lang\""),
        "{}",
        options("text_lang")
    );
    assert!(
        options("text_lang").contains("\"textIndexVersion\":3"),
        "{}",
        options("text_lang")
    );
    assert_eq!(index("sphere").method.as_deref(), Some("2dsphere"));
    assert_eq!(index("flat").method.as_deref(), Some("2d"));

    // The script carries every one of them, and a replay builds the same indexes.
    let ddl = dump::table_ddl(&st, &id, "shop_staging", None, &coll)
        .await
        .unwrap();
    for needle in [
        "wildcardProjection",
        "language_override",
        "textIndexVersion",
        "2dsphereIndexVersion",
        "bits",
        "min",
        "max",
    ] {
        assert!(ddl.contains(needle), "{needle} in {ddl}");
    }
    run(&st, req(&id, Some(&target), &ddl)).await;
    let copy = introspect::table_properties(&st, &id, &target, None, &coll)
        .await
        .unwrap();
    assert_eq!(source.indexes, copy.indexes);

    run(&st, req(&id, None, &format!("{c}.drop()"))).await;
    run(
        &st,
        req(&id, Some(&target), "db.runCommand({dropDatabase: 1})"),
    )
    .await;
}

#[tokio::test]
async fn export_with_data_recreates_every_bson_type() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = root(&st);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let target = clean.database(scratch_db());
    let client = mongo::client::client(&st, &id).await.unwrap();
    let source = vec![
        every_type(),
        doc! { "_id": 2, "nested": { "deeper": [{ "x": Bson::Double(f64::NAN) }] }, "date": Bson::DateTime(DateTime::from_millis(-62_167_219_200_001)) },
    ];
    client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .insert_many(source.clone())
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{coll}.js"));
    let r = dump::export_table_sql(
        &st,
        TableExportRequest {
            conn_id: id.clone(),
            database: "shop_staging".into(),
            schema: None,
            table: coll.clone(),
            data: true,
        },
        &path,
    )
    .await
    .unwrap();
    assert_eq!(r.rows, 2);
    assert_eq!(r.path, path.display().to_string());
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains("insertMany(["), "{script}");
    // A date outside the years 0 to 9999 keeps its milliseconds.
    assert!(script.contains("new Date(253402300800000)"), "{script}");

    run(&st, req(&id, Some(&target), &script)).await;
    let back = documents(&client, &target, &coll).await;
    assert_eq!(back.len(), source.len());
    assert_eq!(canonicals(&source), canonicals(&back));

    client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .drop()
        .await
        .unwrap();
    client.database(&target).drop().await.unwrap();
}

#[tokio::test]
async fn export_documents_as_json_and_ndjson() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let client = mongo::client::client(&st, &id).await.unwrap();
    let source = vec![
        every_type(),
        doc! { "_id": 2, "n": Bson::Int64(2), "d": Bson::Double(1.5) },
    ];
    client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .insert_many(source.clone())
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let request = |format, ejson| DocumentExportRequest {
        query: ExportRequest {
            conn_id: id.clone(),
            database: Some("shop_staging".into()),
            sql: format!("{}.find({{}})", q(&coll)),
            tab_id: None,
        },
        format,
        ejson,
    };

    let path = dir.path().join("docs.json");
    let r = export::export_documents(
        &st,
        request(DocumentFormat::Json, EjsonFlavor::Canonical),
        &path,
    )
    .await
    .unwrap();
    assert_eq!(r.rows, 2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[\n") && text.ends_with("\n]\n"), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let docs: Vec<Bson> = parsed
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| Bson::try_from(v.clone()).expect("canonical EJSON"))
        .collect();
    assert_eq!(docs.len(), 2);
    let parsed_docs: Vec<Document> = docs
        .iter()
        .map(|b| b.as_document().expect("a document").clone())
        .collect();
    assert_eq!(canonicals(&source), canonicals(&parsed_docs));

    let path = dir.path().join("docs.ndjson");
    export::export_documents(
        &st,
        request(DocumentFormat::Ndjson, EjsonFlavor::Canonical),
        &path,
    )
    .await
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let parsed_docs: Vec<Document> = lines
        .iter()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            Bson::try_from(value)
                .expect("canonical EJSON")
                .as_document()
                .expect("a document")
                .clone()
        })
        .collect();
    assert_eq!(canonicals(&source), canonicals(&parsed_docs));

    // Relaxed keeps a plain number where the type survives, and still parses.
    let path = dir.path().join("relaxed.json");
    export::export_documents(
        &st,
        request(DocumentFormat::Json, EjsonFlavor::Relaxed),
        &path,
    )
    .await
    .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.as_array().expect("an array").len(), 2);
    for value in parsed.as_array().unwrap() {
        assert!(Bson::try_from(value.clone()).is_ok(), "{value}");
    }
    let path = dir.path().join("relaxed.ndjson");
    export::export_documents(
        &st,
        request(DocumentFormat::Ndjson, EjsonFlavor::Relaxed),
        &path,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);

    // One read only, as for CSV.
    let e = export::check_documents(
        &st,
        &DocumentExportRequest {
            query: ExportRequest {
                conn_id: id.clone(),
                database: None,
                sql: format!("{}.deleteMany({{}})", q(&coll)),
                tab_id: None,
            },
            format: DocumentFormat::Json,
            ejson: EjsonFlavor::Canonical,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");

    // Extended JSON is not a SQL feature.
    let sqlite = connections::create(
        &st,
        ConnectionInput {
            name: "it-mongo-struct-sqlite".into(),
            engine: Engine::Sqlite,
            env: Env::Local,
            host: String::new(),
            port: 0,
            database: String::new(),
            user: String::new(),
            ssl: SslMode::Disable,
            file: dir.path().join("it.sqlite").display().to_string(),
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
    let e = export::check_documents(
        &st,
        &DocumentExportRequest {
            query: ExportRequest {
                conn_id: sqlite.id,
                database: None,
                sql: "SELECT 1".into(),
                tab_id: None,
            },
            format: DocumentFormat::Json,
            ejson: EjsonFlavor::Canonical,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");

    client
        .database("shop_staging")
        .collection::<Document>(&coll)
        .drop()
        .await
        .unwrap();
}

/// A server command error shows the server's own message and the code label, not the driver's
/// dump of the reply (`RawDocumentBuf { data: "5c01..." }`, `labels: {}`, `server response`).
#[tokio::test]
async fn a_server_command_error_stays_readable() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_rw", false);
    let mut clean = Cleanup::new(RS);
    let coll = clean.collection(scratch());
    let c = q(&coll);
    run(
        &st,
        req(&id, None, &format!("db.createCollection(\"{coll}\")")),
    )
    .await;

    // The server refuses a compound TTL index (code 67).
    let e = run_err(
        &st,
        req(
            &id,
            None,
            &format!(
                "{c}.createIndex({{a: 1, b: 1}}, {{name: \"ttl_two\", expireAfterSeconds: 60}})"
            ),
        ),
    )
    .await;
    assert_eq!(e.code, ErrorCode::Database, "{e:?}");
    assert_eq!(e.server_code.as_deref(), Some("MongoDB error 67"), "{e:?}");
    assert!(e.message.contains("TTL indexes are single-field"), "{e:?}");
    assert_eq!(e.detail.as_deref(), Some("MongoDB error 67"), "{e:?}");
    for bad in ["RawDocumentBuf", "server response", "labels:"] {
        assert!(!e.message.contains(bad), "{bad} in {e:?}");
    }

    // `sandb_rw` has no `dbAdmin` role, so `collMod` is not authorized (code 13). The message
    // stays the server's own sentence, which echoes the refused command (`comment`, `lsid`,
    // `$clusterTime`); only the driver's dump is gone, so the dialog is one line instead of two
    // kilobytes.
    let e = run_err(
        &st,
        req(
            &id,
            None,
            &format!("db.runCommand({{collMod: \"{coll}\", validator: {{}}}})"),
        ),
    )
    .await;
    assert_eq!(e.server_code.as_deref(), Some("MongoDB error 13"), "{e:?}");
    assert!(e.message.starts_with("not authorized"), "{e:?}");
    assert_eq!(e.detail.as_deref(), Some("MongoDB error 13"), "{e:?}");
    let shown = format!("{}: {}", e.message, e.detail.clone().unwrap_or_default());
    for bad in ["RawDocumentBuf", "server response", "labels:"] {
        assert!(!shown.contains(bad), "{bad} in {shown}");
    }

    run(&st, req(&id, None, &format!("{c}.drop()"))).await;
}

#[tokio::test]
async fn export_documents_streams_big_documents() {
    let _serial = SERIAL.lock().await;
    let (_d, st) = state();
    let id = saved(&st, "sandb_ro", true);
    // The count first: `big_docs` is read once, by the export.
    let count = query::run(&st, req(&id, None, "db.big_docs.countDocuments({})"))
        .await
        .unwrap();
    let count = count.statements[0].rows[0][0].as_i64().expect("a count");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.ndjson");
    let r = export::export_documents(
        &st,
        DocumentExportRequest {
            query: ExportRequest {
                conn_id: id.clone(),
                database: None,
                sql: "db.big_docs.find({})".into(),
                tab_id: None,
            },
            format: DocumentFormat::Ndjson,
            ejson: EjsonFlavor::Relaxed,
        },
        &path,
    )
    .await
    .unwrap();
    assert_eq!(r.rows, count as u64);
    let mut lines = 0i64;
    let mut longest = 0usize;
    for line in std::io::BufReader::new(std::fs::File::open(&path).unwrap()).lines() {
        let line = line.unwrap();
        lines += 1;
        longest = longest.max(line.len());
    }
    assert_eq!(lines, count);
    assert!(longest > 1_000_000, "a big document: {longest} bytes");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn properties_serialize_with_camel_case_and_omit_empty_parts() {
    let properties = TableProperties {
        indexes: vec![],
        constraints: vec![],
        foreign_keys: vec![],
        references: vec![],
        mongo: Some(MongoCollectionInfo {
            kind: MongoCollectionKind::Collection,
            validator: None,
            validation_level: None,
            validation_action: None,
            options: "{}".into(),
            stats: None,
        }),
    };
    let json = serde_json::to_value(&properties).unwrap();
    let mongo = &json["mongo"];
    assert_eq!(mongo["kind"], serde_json::json!("collection"));
    assert_eq!(mongo["options"], serde_json::json!("{}"));
    assert!(mongo.get("validator").is_none() && mongo.get("stats").is_none());
    let plain = TableProperties::default();
    let json = serde_json::to_value(&plain).unwrap();
    assert!(json.get("mongo").is_none(), "{json}");
}
