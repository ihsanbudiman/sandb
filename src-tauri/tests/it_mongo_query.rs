//! MongoDB editor runs (`query::run`) against the sandb-test docker stack (`pnpm db:up`),
//! `mongo-rs` on 57017 with the seed of `testdb/seed/mongo/01_data.js`.
//! Run: `cargo test --features it --test it_mongo_query`.
#![cfg(feature = "it")]

mod common;

use std::time::Duration;

use mongodb::bson::{Document, doc};
use sandb_lib::core::result::ExportRequest;
use sandb_lib::core::result::{DEFAULT_LIMIT, QueryRun, RunRequest, RunSort};
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::state::AppState;
use serde_json::{Value, json};

use common::Cleanup;

/// A saved connection to `shop_staging` as `user` (`sandb_ro` or `sandb_rw`).
fn saved(st: &AppState, user: &str, read_only: bool) -> String {
    let input = common::mongo_input(
        &format!("mongo-query-{user}"),
        common::mongo_rs(),
        user,
        common::seed::STAGING,
        read_only,
    );
    st.create_connection(input, Some(common::password(user)))
        .unwrap()
        .id
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

async fn run(st: &AppState, r: RunRequest) -> QueryRun {
    let text = r.sql.clone();
    ops::run_query(st, r)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e:?}"))
}

async fn run_err(st: &AppState, r: RunRequest) -> AppError {
    ops::run_query(st, r).await.expect_err("the run must fail")
}

fn column<'a>(r: &'a QueryRun, i: usize, name: &str) -> Vec<&'a Value> {
    let s = &r.statements[i];
    let c = s
        .columns
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name} in {:?}", s.columns));
    s.rows.iter().map(|row| &row[c]).collect()
}

#[tokio::test]
async fn find_returns_a_page_with_columns_types_and_documents() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let r = run(&st, req(&id, "db.products.find({})")).await;
    let s = &r.statements[0];
    assert_eq!(s.rows.len(), DEFAULT_LIMIT);
    assert!(s.truncated && s.is_read && s.sortable);
    let names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(&names[..4], ["_id", "sku", "name", "price"]);
    // The seed mixes decimal128, int32 and string prices.
    let price = s.columns.iter().find(|c| c.name == "price").unwrap();
    assert!(price.data_type.contains(" | "), "{price:?}");
    let docs = s.documents.as_ref().unwrap();
    let types = s.cell_types.as_ref().unwrap();
    assert_eq!(docs.len(), s.rows.len());
    assert_eq!(types.len(), s.rows.len());
    // Product 40 has a string price (row 39); product 7 has no stock (row 6).
    assert_eq!(types[39][3], "string");
    let stock = names.iter().position(|n| *n == "stock").unwrap();
    assert_eq!(types[6][stock], "missing");
    assert!(
        docs[0]["price"].get("$numberDecimal").is_some(),
        "{}",
        docs[0]
    );
}

#[tokio::test]
async fn find_applies_filter_projection_sort_and_limit() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let r = run(
        &st,
        req(
            &id,
            "db.products.find({stock: {$gte: 100}}, {sku: 1, stock: 1, _id: 0})\n  .sort({stock: -1, _id: 1}).limit(5)",
        ),
    )
    .await;
    let s = &r.statements[0];
    assert_eq!(s.rows.len(), 5);
    assert!(!s.truncated);
    let names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["sku", "stock"]);
    let stock: Vec<i64> = column(&r, 0, "stock")
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert!(stock.windows(2).all(|w| w[0] >= w[1]), "{stock:?}");
}

#[tokio::test]
async fn aggregate_and_nested_paths() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let r = run(
        &st,
        req(
            &id,
            "db.orders.aggregate([{$group: {_id: '$status', n: {$sum: 1}}}, {$sort: {_id: 1}}])",
        ),
    )
    .await;
    let s = &r.statements[0];
    assert_eq!(s.rows.len(), 5);
    let total: i64 = column(&r, 0, "n").iter().map(|v| v.as_i64().unwrap()).sum();
    assert_eq!(total, common::seed::ORDERS as i64);
    let r = run(&st, req(&id, "db.customers.find({}).limit(3)")).await;
    let names: Vec<&str> = r.statements[0]
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(names.contains(&"address.city"), "{names:?}");
    assert!(names.contains(&"address.geo.lat"), "{names:?}");
    assert_eq!(column(&r, 0, "tags")[0], &json!("[1 item]"));
}

#[tokio::test]
async fn header_sort_orders_find_and_aggregate() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    for text in [
        "db.customers.find({}, {name: 1})",
        "db.customers.aggregate([{$project: {name: 1}}])",
    ] {
        let mut r = req(&id, text);
        r.sort = Some(RunSort {
            column: "_id".into(),
            column_index: 0,
            ascending: false,
        });
        let out = run(&st, r).await;
        assert_eq!(out.statements[0].sql, text);
        assert_eq!(
            column(&out, 0, "_id")[0],
            &json!(common::seed::CUSTOMERS),
            "{text}"
        );
    }
    let mut r = req(&id, "db.customers.countDocuments({})");
    r.sort = Some(RunSort {
        column: "count".into(),
        column_index: 0,
        ascending: true,
    });
    assert_eq!(run_err(&st, r).await.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn load_more_raises_the_limit() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let mut r = req(&id, "db.orders.find({})");
    r.limit = Some(400);
    let out = run(&st, r).await;
    assert_eq!(out.statements[0].rows.len(), 400);
    assert!(out.statements[0].truncated);
}

#[tokio::test]
async fn large_documents_stop_at_the_page_byte_limit() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let out = run(&st, req(&id, "db.big_docs.find({})")).await;
    let s = &out.statements[0];
    assert!(
        (1..10).contains(&s.rows.len()),
        "about 2 MB each: {}",
        s.rows.len()
    );
    assert!(s.truncated);
    assert!(
        out.notices.iter().any(|n| n.contains("16 MB page limit")),
        "{:?}",
        out.notices
    );
}

#[tokio::test]
async fn counts_distinct_show_and_use_in_one_run() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let out = run(
        &st,
        req(
            &id,
            "db.customers.countDocuments({plan: 'pro'})\ndb.customers.distinct('plan')\nshow collections\nuse analytics\ndb.events.countDocuments({})\ndb.products.find().count()",
        ),
    )
    .await;
    assert_eq!(out.statements.len(), 6);
    let count = |i: usize| column(&out, i, "count")[0].as_i64().unwrap();
    assert_eq!(count(0), common::seed::CUSTOMERS_ON_PRO);
    assert_eq!(out.statements[1].rows.len(), 3);
    let names: Vec<&Value> = column(&out, 2, "name");
    assert!(names.contains(&&json!("order_summary")), "{names:?}");
    assert!(out.notices.iter().any(|n| n.contains("analytics")));
    assert_eq!(count(4), common::seed::EVENTS as i64);
    // `use` lasts for the rest of the run: analytics has no products.
    assert_eq!(count(5), 0);
}

#[tokio::test]
async fn read_only_connection_refuses_writes_before_they_run() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_rw", true);
    for text in [
        "db.products.insertOne({a: 1})",
        "db.products.find({$where: 'true'})",
        "db.orders.aggregate([{$out: 'copy'}])",
        "db.runCommand({dropDatabase: 1})",
    ] {
        let e = run_err(&st, req(&id, text)).await;
        assert_eq!(e.code, ErrorCode::ConnectionReadOnly, "{text}: {e:?}");
    }
    // The server is the second barrier: the `read` role cannot write even past the app check.
    let ro = saved(&st, "sandb_ro", false);
    let e = ops::run_editor_unchecked(&st, req(&ro, "db.products.insertOne({a: 1})"))
        .await
        .expect_err("server refuses");
    assert_eq!(e.code, ErrorCode::Database, "{e:?}");
    assert!(e.message.contains("not authorized"), "{e:?}");
}

#[tokio::test]
async fn writes_report_counts_on_a_writable_connection() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_rw", false);
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    let out = ops::run_query(&st, req(
            &id,
            &format!(
                "db.{coll}.insertMany([{{a: 1}}, {{a: 2}}, {{a: 3}}])\ndb.{coll}.updateMany({{a: {{$gte: 2}}}}, {{$set: {{b: true}}}})\ndb.{coll}.find({{b: true}})\ndb.{coll}.drop()"
            ),
        ), )
    .await;
    let out = out.unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(out.statements[0].affected, Some(3));
    assert_eq!(out.statements[1].affected, Some(2));
    assert!(!out.statements[1].is_read);
    assert_eq!(out.statements[2].rows.len(), 2);
    assert!(
        out.notices
            .iter()
            .any(|n| n.contains("matched 2, modified 2")),
        "{:?}",
        out.notices
    );
}

#[tokio::test]
async fn parse_errors_carry_the_position() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let e = run_err(&st, req(&id, "db.products.find({a: })")).await;
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(e.message.starts_with("Line 1, column 22:"), "{e:?}");
}

#[tokio::test]
async fn csv_export_streams_every_document_with_flattened_columns() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("customers.csv");
    let request = |sql: &str| ExportRequest {
        conn_id: id.clone(),
        database: None,
        sql: sql.into(),
        tab_id: None,
    };
    let r = ops::export_csv(&st, request("db.customers.find({})"), &path)
        .await
        .unwrap();
    assert_eq!(r.rows, common::seed::CUSTOMERS);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split(',').collect();
    assert!(header.contains(&"address.city"), "{header:?}");
    // A field that only some documents have is a column too; arrays keep their full value.
    assert!(header.contains(&"phone"), "{header:?}");
    let tags = header.iter().position(|h| *h == "tags").unwrap();
    let first: Vec<&str> = lines.next().unwrap().split(',').collect();
    assert_eq!(first[tags], "\"[\"\"newsletter\"\"]\"", "{first:?}");
    assert_eq!(text.lines().count(), 501);
    let e = ops::check_export(&st, &request("db.customers.deleteMany({})")).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn cancel_stops_a_slow_aggregation_on_the_server() {
    let (_d, st) = common::state();
    let id = saved(&st, "sandb_ro", true);
    // A correlated `$lookup` of the events on themselves scans 10,000 × 10,000 documents.
    let mut r = req(
        &id,
        "db.events.aggregate([{$lookup: {from: 'events', let: {u: '$user_id'}, pipeline: [{$match: {$expr: {$eq: ['$user_id', '$$u']}}}, {$count: 'n'}], as: 'same'}}, {$count: 'n'}])",
    );
    r.database = Some("analytics".into());
    let query_id = r.query_id.clone();
    let task = {
        let st = st.clone();
        tokio::spawn(async move { ops::run_query(&st, r).await })
    };
    // Wait for the run to register, then cancel it.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ops::query_progress(&st, &query_id).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the aggregation never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ops::cancel_query(&st, &query_id).await.unwrap();
    let e = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the run ends after the cancel")
        .unwrap()
        .expect_err("cancelled");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
    // Nothing with the run's comment is left on the server.
    let admin = common::root_client(common::mongo_rs()).await.database("admin");
    let mut ops = admin
        .aggregate(vec![
            doc! { "$currentOp": { "allUsers": true } },
            doc! { "$match": { "command.comment": &query_id } },
        ])
        .await
        .unwrap();
    let mut left: Vec<Document> = Vec::new();
    while ops.advance().await.unwrap() {
        left.push(ops.deserialize_current().unwrap());
    }
    assert!(left.is_empty(), "{left:?}");
}
