//! MongoDB integration tests against the sandb-test docker stack (`pnpm db:up`): `mongo-rs`
//! (one-node replica set, 57017) and `mongo-standalone` (57018).
//! Run: `cargo test --features it`. An unreachable server FAILS the test; nothing is skipped.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;

use mongodb::bson::doc;
use sandb_lib::core::connections::{self, ConnectionInput, Engine, Env, SslMode, Topology};
use sandb_lib::core::db::dump;
use sandb_lib::core::db::introspect::{self, MongoCollectionKind, TableKind};
use sandb_lib::core::db::query::{self, RunRequest};
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::mongo;
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

/// Fields mode, `sandb_ro` (users live in `admin`).
fn fields(port: u16) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-mongo-{port}"),
        engine: Engine::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: "shop_staging".into(),
        user: "sandb_ro".into(),
        ssl: SslMode::Disable,
        file: String::new(),
        ai_write: false,
        read_only: true,
        version: None,
        uri: String::new(),
        auth_source: "admin".into(),
        topology: None,
    }
}

#[tokio::test]
async fn fields_report_version_and_topology() {
    for (port, topology) in [
        (RS, Topology::ReplicaSet),
        (STANDALONE, Topology::Standalone),
    ] {
        let (_d, st) = state();
        let r = connections::test(&st, fields(port), Some(ro_password()), None)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "mongo on 127.0.0.1:{port} is not reachable (is `pnpm db:up` running?): {e:?}"
                )
            });
        assert!(
            r.server_version.starts_with("MongoDB 8."),
            "{port}: {:?}",
            r.server_version
        );
        assert_eq!(r.topology, Some(topology), "{port}");
    }
}

#[tokio::test]
async fn connection_string_uses_the_stored_password() {
    let (_d, st) = state();
    let mut i = fields(RS);
    i.uri = format!(
        "mongodb://sandb_ro:{}@127.0.0.1:{RS}/shop_staging?authSource=admin",
        ro_password()
    );
    let r = connections::test(&st, i.clone(), None, None).await.unwrap();
    assert_eq!(r.topology, Some(Topology::ReplicaSet));

    let c = connections::create(&st, i, None).unwrap();
    assert!(!c.uri.contains(&ro_password()), "{}", c.uri);
    // The saved string has no password: the test reads it from the secret store.
    let mut again = fields(RS);
    again.uri = c.uri.clone();
    let r = connections::test(&st, again, None, Some(&c.id))
        .await
        .unwrap();
    assert!(r.server_version.starts_with("MongoDB 8."));
}

#[tokio::test]
async fn wrong_password_is_auth_failed() {
    for port in [RS, STANDALONE] {
        let (_d, st) = state();
        let e = connections::test(&st, fields(port), Some("definitely-wrong".into()), None)
            .await
            .expect_err("a wrong password must fail");
        assert_eq!(e.code, ErrorCode::AuthFailed, "{port}: {e:?}");
    }
}

/// MongoDB has no read-only session: on a read-only connection only the server's roles stop a
/// write. This proves the seed user `sandb_ro` (role `read`) is refused by the server itself.
#[tokio::test]
async fn read_role_user_gets_a_server_error_for_a_write() {
    let (_d, st) = state();
    for port in [RS, STANDALONE] {
        let c = connections::create(&st, fields(port), Some(ro_password())).unwrap();
        let o = mongo::conn::client_options(&c, Some(&ro_password()))
            .await
            .unwrap();
        let client = mongodb::Client::with_options(o).unwrap();
        let customers = client
            .database("shop_staging")
            .collection::<mongodb::bson::Document>("customers");
        assert_eq!(customers.count_documents(doc! {}).await.unwrap(), 500);
        let e = customers
            .insert_one(doc! { "_id": "it-write" })
            .await
            .expect_err("sandb_ro must not write");
        assert_eq!(
            sandb_lib::core::error::AppError::from(e)
                .server_code
                .as_deref(),
            Some("MongoDB error 13"),
            "{port}: Unauthorized"
        );
        client.shutdown().await;
    }
}

/// SQL text in the editor gets the mongosh parser's error, while the structure paths answer for
/// a Mongo connection.
#[tokio::test]
async fn sql_paths_refuse_mongo() {
    let (_d, st) = state();
    let mut i = fields(RS);
    i.read_only = false;
    let c = connections::create(&st, i, Some(ro_password())).unwrap();
    let p = introspect::table_properties(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert_eq!(
        p.mongo.expect("the mongo part").kind,
        MongoCollectionKind::Collection
    );
    let ddl = dump::table_ddl(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert!(ddl.starts_with("// sandb MongoDB export"), "{ddl}");
    let e = query::run(
        &st,
        RunRequest {
            conn_id: c.id.clone(),
            database: None,
            sql: "SELECT 1".into(),
            query_id: "it-mongo-sql".into(),
            limit: None,
            tab_id: None,
            sort: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
    assert!(e.message.starts_with("Line 1, column 1:"), "{e:?}");
}

fn saved(st: &AppState, port: u16) -> connections::Connection {
    connections::create(st, fields(port), Some(ro_password())).unwrap()
}

/// `sandb_ro` (role `read`) lists only its authorized databases; `admin`/`local`/`config` are
/// hidden, so the list is exactly the two seed databases.
#[tokio::test]
async fn list_databases_returns_the_seed_databases() {
    for port in [RS, STANDALONE] {
        let (_d, st) = state();
        let c = saved(&st, port);
        let dbs = introspect::list_databases(&st, &c.id).await.unwrap();
        assert_eq!(dbs, ["analytics", "shop_staging"], "{port}");
        // MongoDB has no schema level and opens no pool for it.
        let schemas = introspect::list_schemas(&st, &c.id, "shop_staging")
            .await
            .unwrap();
        assert!(schemas.is_empty(), "{port}");
    }
}

#[tokio::test]
async fn list_tables_shows_collections_and_views() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let tables = introspect::list_tables(&st, &c.id, "shop_staging", None)
        .await
        .unwrap();
    for name in ["customers", "orders", "products"] {
        let t = tables.iter().find(|t| t.name == name).unwrap();
        assert_eq!(t.kind, TableKind::Table, "{name}");
        assert_eq!(t.row_estimate, Some(500), "{name}");
    }
    let view = tables.iter().find(|t| t.name == "order_summary").unwrap();
    assert_eq!(view.kind, TableKind::View);
    assert!(!tables.iter().any(|t| t.name.starts_with("system.")));

    let tables = introspect::list_tables(&st, &c.id, "analytics", None)
        .await
        .unwrap();
    let metrics = tables.iter().find(|t| t.name == "metrics").unwrap();
    assert_eq!(metrics.kind, TableKind::Table, "time series is a table");
    let events = tables.iter().find(|t| t.name == "events").unwrap();
    assert_eq!(events.row_estimate, Some(10000));
}

/// `price` mixes Decimal128 with the Int32 of every 25th and the String of every 40th product.
#[tokio::test]
async fn list_columns_reports_the_mixed_type_field() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let cols = introspect::list_columns(&st, &c.id, "shop_staging", None, "products")
        .await
        .unwrap();
    assert_eq!(cols[0].name, "_id");
    assert!(cols[0].is_pk);
    let price = cols.iter().find(|c| c.name == "price").unwrap();
    for ty in ["decimal128", "int32", "string"] {
        assert!(price.data_type.split(" | ").any(|t| t == ty), "{ty}");
    }
    assert_eq!(price.frequency, Some(1.0));
    assert_eq!(price.sample_size, Some(500));
}

#[tokio::test]
async fn list_columns_flattens_nested_paths() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let cols = introspect::list_columns(&st, &c.id, "shop_staging", None, "orders")
        .await
        .unwrap();
    assert!(cols.iter().any(|c| c.name == "items[].qty"));
    let cols = introspect::list_columns(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert!(cols.iter().any(|c| c.name == "address.city"));
}

/// `big_docs` is 100 documents of about 2 MB; the 8 MB cap merges the document that crosses
/// it, so about four documents land in the summary.
#[tokio::test]
async fn list_columns_stops_at_the_byte_cap() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let cols = introspect::list_columns(&st, &c.id, "shop_staging", None, "big_docs")
        .await
        .unwrap();
    let n = cols[0].sample_size.unwrap();
    assert!((1..100).contains(&n), "{n}");
}

#[tokio::test]
async fn list_columns_unknown_collection_is_not_found() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let e = introspect::list_columns(&st, &c.id, "shop_staging", None, "nope")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}

/// A user with no roles at all cannot run `listDatabases`, even with `authorizedDatabases`:
/// the tree then shows the connection's own database.
#[tokio::test]
async fn list_databases_falls_back_to_the_own_database() {
    let (_d, st) = state();
    let admin = raw_client(&format!(
        "mongodb://root:{}@127.0.0.1:{RS}/admin",
        env_test()["MONGO_INITDB_ROOT_PASSWORD"]
    ))
    .await;
    let admindb = admin.database("admin");
    admindb
        .run_command(doc! { "dropUser": "it_nopriv" })
        .await
        .ok();
    admindb
        .run_command(doc! { "createUser": "it_nopriv", "pwd": "it_nopriv_pw", "roles": [] })
        .await
        .unwrap();
    admin.shutdown().await;

    let mut i = fields(RS);
    i.user = "it_nopriv".into();
    i.name = "it-mongo-nopriv".into();
    let c = connections::create(&st, i, Some("it_nopriv_pw".into())).unwrap();
    let dbs = introspect::list_databases(&st, &c.id).await.unwrap();
    assert_eq!(dbs, ["shop_staging"]);
}

async fn raw_client(uri: &str) -> mongodb::Client {
    let options = mongodb::options::ClientOptions::parse(uri).await.unwrap();
    mongodb::Client::with_options(options).unwrap()
}

#[tokio::test]
async fn schema_snapshot_has_tables_and_columns() {
    let (_d, st) = state();
    let c = saved(&st, RS);
    let snap = introspect::schema_snapshot(&st, &c.id, "shop_staging")
        .await
        .unwrap();
    assert!(!snap.truncated);
    let products = snap.tables.iter().find(|t| t.name == "products").unwrap();
    assert!(products.columns.iter().any(|c| c.name == "price"));
}
