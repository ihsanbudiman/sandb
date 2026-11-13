//! MongoDB integration tests against the sandb-test docker stack (`pnpm db:up`): `mongo-rs`
//! (one-node replica set, 57017) and `mongo-standalone` (57018).
//! Run: `cargo test --features it`. An unreachable server FAILS the test; nothing is skipped.
#![cfg(feature = "it")]

mod common;

use mongodb::bson::doc;
use sandb_lib::core::connections::{ConnectionInput, Topology};
use sandb_lib::core::result::{MongoCollectionKind, TableKind};
use sandb_lib::core::result::RunRequest;
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::state::AppState;

/// Fields mode, `sandb_ro` (users live in `admin`).
fn fields(port: u16) -> ConnectionInput {
    common::mongo_input("mongo", port, &common::ro_user(), common::seed::STAGING, true)
}

#[tokio::test]
async fn fields_report_version_and_topology() {
    for (port, topology) in [
        (common::mongo_rs(), Topology::ReplicaSet),
        (common::mongo_standalone(), Topology::Standalone),
    ] {
        let (_d, st) = common::state();
        let r = st.test_connection(fields(port), Some(common::ro_password()), None)
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
    let (_d, st) = common::state();
    let mut i = fields(common::mongo_rs());
    i.uri = format!(
        "mongodb://sandb_ro:{}@127.0.0.1:{}/shop_staging?authSource=admin",
        common::ro_password(),
        common::mongo_rs()
    );
    let r = st.test_connection(i.clone(), None, None).await.unwrap();
    assert_eq!(r.topology, Some(Topology::ReplicaSet));

    let c = st.create_connection(i, None).unwrap();
    assert!(!c.uri.contains(&common::ro_password()), "{}", c.uri);
    // The saved string has no password: the test reads it from the secret store.
    let mut again = fields(common::mongo_rs());
    again.uri = c.uri.clone();
    let r = st.test_connection(again, None, Some(&c.id))
        .await
        .unwrap();
    assert!(r.server_version.starts_with("MongoDB 8."));
}

#[tokio::test]
async fn wrong_password_is_auth_failed() {
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let (_d, st) = common::state();
        let e = st.test_connection(fields(port), Some("definitely-wrong".into()), None)
            .await
            .expect_err("a wrong password must fail");
        assert_eq!(e.code, ErrorCode::AuthFailed, "{port}: {e:?}");
    }
}

/// MongoDB has no read-only session: on a read-only connection only the server's roles stop a
/// write. This proves the seed user `sandb_ro` (role `read`) is refused by the server itself.
#[tokio::test]
async fn read_role_user_gets_a_server_error_for_a_write() {
    let (_d, st) = common::state();
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let c = st.create_connection(fields(port), Some(common::ro_password())).unwrap();
        let o = mongo::conn::client_options(&c, Some(&common::ro_password()))
            .await
            .unwrap();
        let client = mongodb::Client::with_options(o).unwrap();
        let customers = client
            .database("shop_staging")
            .collection::<mongodb::bson::Document>("customers");
        assert_eq!(
            customers.count_documents(doc! {}).await.unwrap(),
            common::seed::CUSTOMERS
        );
        let e = customers
            .insert_one(doc! { "_id": "it-write" })
            .await
            .expect_err("sandb_ro must not write");
        assert_eq!(
            sandb_lib::core::error::AppError::from(e)
                .server_code
                .as_ref()
                .map(|c| c.code.as_str()),
            Some("13"),
            "{port}: Unauthorized"
        );
        client.shutdown().await;
    }
}

/// SQL text in the editor gets the mongosh parser's error, while the structure paths answer for
/// a Mongo connection.
#[tokio::test]
async fn sql_paths_refuse_mongo() {
    let (_d, st) = common::state();
    let mut i = fields(common::mongo_rs());
    i.read_only = false;
    let c = st.create_connection(i, Some(common::ro_password())).unwrap();
    let p = ops::table_properties(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert_eq!(
        p.mongo.expect("the mongo part").kind,
        MongoCollectionKind::Collection
    );
    let ddl = ops::table_ddl(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert!(ddl.starts_with("// sandb MongoDB export"), "{ddl}");
    let e = ops::run_query(&st, RunRequest {
            conn_id: c.id.clone(),
            database: None,
            sql: "SELECT 1".into(),
            query_id: "it-mongo-sql".into(),
            limit: None,
            tab_id: None,
            sort: None,
        }, )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
    assert!(e.message.starts_with("Line 1, column 1:"), "{e:?}");
}

fn saved(st: &AppState, port: u16) -> sandb_lib::core::connections::Connection {
    st.create_connection(fields(port), Some(common::ro_password())).unwrap()
}

/// `sandb_ro` (role `read`) lists only its authorized databases; `admin`/`local`/`config` are
/// hidden, so the list is exactly the two seed databases.
#[tokio::test]
async fn list_databases_returns_the_seed_databases() {
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let (_d, st) = common::state();
        let c = saved(&st, port);
        let dbs = ops::list_databases(&st, &c.id).await.unwrap();
        assert_eq!(dbs, ["analytics", "shop_staging"], "{port}");
        // MongoDB has no schema level and opens no pool for it.
        let schemas = ops::list_schemas(&st, &c.id, "shop_staging")
            .await
            .unwrap();
        assert!(schemas.is_empty(), "{port}");
    }
}

#[tokio::test]
async fn list_tables_shows_collections_and_views() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let tables = ops::list_tables(&st, &c.id, "shop_staging", None)
        .await
        .unwrap();
    for name in ["customers", "orders", "products"] {
        let t = tables.iter().find(|t| t.name == name).unwrap();
        assert_eq!(t.kind, TableKind::Table, "{name}");
        assert_eq!(
            t.row_estimate,
            Some(common::seed::CUSTOMERS as i64),
            "{name}"
        );
    }
    let view = tables.iter().find(|t| t.name == "order_summary").unwrap();
    assert_eq!(view.kind, TableKind::View);
    assert!(!tables.iter().any(|t| t.name.starts_with("system.")));

    let tables = ops::list_tables(&st, &c.id, "analytics", None)
        .await
        .unwrap();
    let metrics = tables.iter().find(|t| t.name == "metrics").unwrap();
    assert_eq!(metrics.kind, TableKind::Table, "time series is a table");
    let events = tables.iter().find(|t| t.name == "events").unwrap();
    assert_eq!(events.row_estimate, Some(common::seed::EVENTS as i64));
}

/// `price` mixes Decimal128 with the Int32 of every 25th and the String of every 40th product.
#[tokio::test]
async fn list_columns_reports_the_mixed_type_field() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let cols = ops::list_columns(&st, &c.id, "shop_staging", None, "products")
        .await
        .unwrap();
    assert_eq!(cols[0].name, "_id");
    assert!(cols[0].is_pk);
    let price = cols.iter().find(|c| c.name == "price").unwrap();
    for ty in ["decimal128", "int32", "string"] {
        assert!(price.data_type.split(" | ").any(|t| t == ty), "{ty}");
    }
    assert_eq!(price.frequency, Some(1.0));
    assert_eq!(price.sample_size, Some(common::seed::PRODUCTS as u32));
}

#[tokio::test]
async fn list_columns_flattens_nested_paths() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let cols = ops::list_columns(&st, &c.id, "shop_staging", None, "orders")
        .await
        .unwrap();
    assert!(cols.iter().any(|c| c.name == "items[].qty"));
    let cols = ops::list_columns(&st, &c.id, "shop_staging", None, "customers")
        .await
        .unwrap();
    assert!(cols.iter().any(|c| c.name == "address.city"));
}

/// `big_docs` is 100 documents of about 2 MB; the 8 MB cap merges the document that crosses
/// it, so about four documents land in the summary.
#[tokio::test]
async fn list_columns_stops_at_the_byte_cap() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let cols = ops::list_columns(&st, &c.id, "shop_staging", None, "big_docs")
        .await
        .unwrap();
    let n = cols[0].sample_size.unwrap();
    assert_eq!(
        n,
        common::seed::BIG_DOC_SAMPLE as u32,
        "the 8 MiB sample stops after this many big documents"
    );
}

#[tokio::test]
async fn list_columns_unknown_collection_is_not_found() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let e = ops::list_columns(&st, &c.id, "shop_staging", None, "nope")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}

/// A user with no roles at all cannot run `listDatabases`, even with `authorizedDatabases`:
/// the tree then shows the connection's own database.
#[tokio::test]
async fn list_databases_falls_back_to_the_own_database() {
    let (_d, st) = common::state();
    let admin = raw_client(&format!(
        "mongodb://root:{}@127.0.0.1:{}/admin",
        common::root_password(),
        common::mongo_rs()
    ))
    .await;
    let admindb = admin.database("admin");
    // A user left by an interrupted run is not an error; any other failure is.
    if let Err(e) = admindb.run_command(doc! { "dropUser": "it_nopriv" }).await {
        let e = AppError::from(e);
        assert_eq!(
            e.server_code.as_ref().map(|c| c.code.as_str()),
            Some("11"), // UserNotFound
            "dropUser it_nopriv: {e:?}"
        );
    }
    admindb
        .run_command(doc! { "createUser": "it_nopriv", "pwd": "it_nopriv_pw", "roles": [] })
        .await
        .unwrap();

    let mut i = fields(common::mongo_rs());
    i.user = "it_nopriv".into();
    i.name = "it-mongo-nopriv".into();
    let c = st.create_connection(i, Some("it_nopriv_pw".into())).unwrap();
    let dbs = ops::list_databases(&st, &c.id).await.unwrap();
    assert_eq!(dbs, ["shop_staging"]);

    admindb
        .run_command(doc! { "dropUser": "it_nopriv" })
        .await
        .expect("the test user must be removed again");
    admin.shutdown().await;
}

async fn raw_client(uri: &str) -> mongodb::Client {
    let options = mongodb::options::ClientOptions::parse(uri).await.unwrap();
    mongodb::Client::with_options(options).unwrap()
}

#[tokio::test]
async fn schema_snapshot_has_tables_and_columns() {
    let (_d, st) = common::state();
    let c = saved(&st, common::mongo_rs());
    let snap = ops::schema_snapshot(&st, &c.id, "shop_staging")
        .await
        .unwrap();
    assert!(!snap.truncated);
    let products = snap.tables.iter().find(|t| t.name == "products").unwrap();
    assert!(products.columns.iter().any(|c| c.name == "price"));
}
