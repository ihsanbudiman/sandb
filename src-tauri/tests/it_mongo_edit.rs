//! Integration tests for grid edits on MongoDB (`core::mongo::edit` through the shared
//! `core::db::edit` commands) against the sandb-test docker stack (`pnpm db:up`). Each test seeds
//! its own scratch collection with explicit types and drops it at the end.
//! Run: `cargo test --features it --test it_mongo_edit`.
#![cfg(feature = "it")]

mod common;

use mongodb::bson::oid::ObjectId;
use mongodb::bson::{Bson, DateTime, Decimal128, Document, doc};
use mongodb::options::{Collation, CollationStrength};
use sandb_lib::core::connections::ConnectionInput;
use sandb_lib::core::result::{
    ApplyEditsRequest, ApplyEditsResult, CellValueRequest, EditOperation,
    EditableRequest, EditableResult,
};
use sandb_lib::core::result::RunRequest;
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::mongo::value::literal;
use sandb_lib::core::state::AppState;
use serde_json::{Value, json};

use common::{Cleanup, Ctx};

const CONFLICT: &str =
    "The document changed or no longer exists since it was loaded. Nothing was overwritten.";

/// Read-write user on `shop_staging`.
fn rw(port: u16) -> ConnectionInput {
    common::mongo_input("mongo-edit", port, &common::rw_user(), common::seed::STAGING, false)
}

/// A read-write connection on `port`, its client and a scratch collection seeded with `docs`.
struct Fixture {
    _dir: tempfile::TempDir,
    st: std::sync::Arc<AppState>,
    conn_id: String,
    client: mongodb::Client,
    coll: String,
    _clean: Cleanup,
}

impl Fixture {
    async fn new(port: u16, docs: Vec<Document>) -> Self {
        let f = Self::open(port).await;
        f.seed(common::seed::STAGING, docs).await;
        f
    }

    /// The connection and a scratch name; the collection does not exist yet.
    async fn open(port: u16) -> Self {
        let ctx: Ctx = common::ctx(
            "mongo-edit",
            rw(port),
            Some(common::rw_password()),
        );
        let mut clean = Cleanup::new(port);
        let coll = clean.collection(common::scratch());
        let client = mongo::client::client(ctx.st.as_ref(), &ctx.conn.id)
            .await
            .unwrap();
        Fixture {
            _dir: ctx._dir,
            st: ctx.st,
            conn_id: ctx.conn.id,
            client,
            coll,
            _clean: clean,
        }
    }

    async fn seed(&self, db: &str, docs: Vec<Document>) {
        self.client
            .database(db)
            .collection::<Document>(&self.coll)
            .insert_many(docs)
            .await
            .unwrap();
    }

    /// Tracks the fixture's scratch name in `database` too, for a test that seeds it there.
    fn track_in(&mut self, database: &str) {
        self._clean.collection_in(database, &self.coll);
    }

    fn sql(&self) -> String {
        format!("db.{}.find()", self.coll)
    }

    async fn editable(&self, sql: &str) -> EditableResult {
        ops::editable_result(&self.st, EditableRequest {
                conn_id: self.conn_id.clone(),
                database: common::seed::STAGING.into(),
                sql: sql.into(),
            }, )
        .await
        .unwrap()
    }

    async fn save(&self, operations: Vec<EditOperation>) -> Result<ApplyEditsResult, AppError> {
        ops::apply_edits(&self.st, ApplyEditsRequest {
                conn_id: self.conn_id.clone(),
                database: common::seed::STAGING.into(),
                sql: self.sql(),
                tab_id: None,
                query_id: None,
                operations,
            }, )
        .await
    }

    async fn cell(&self, id: Value, column: &str) -> Result<Option<String>, AppError> {
        ops::cell_value(&self.st, CellValueRequest {
                conn_id: self.conn_id.clone(),
                database: common::seed::STAGING.into(),
                sql: self.sql(),
                tab_id: None,
                pk: [("_id".to_string(), id)].into(),
                column: column.into(),
            }, )
        .await
    }

    async fn find(&self, id: i32) -> Option<Document> {
        self.client
            .database(common::seed::STAGING)
            .collection::<Document>(&self.coll)
            .find_one(doc! { "_id": id })
            .await
            .unwrap()
    }

    async fn count(&self) -> u64 {
        self.client
            .database(common::seed::STAGING)
            .collection::<Document>(&self.coll)
            .count_documents(doc! {})
            .await
            .unwrap()
    }

    /// Drops the scratch collection. The tests drop before they assert, so a failed assert
    /// cannot leak a collection.
    async fn drop(&self) {
        self.client
            .database(common::seed::STAGING)
            .collection::<Document>(&self.coll)
            .drop()
            .await
            .unwrap();
    }
}

/// A typed wire value.
fn typed(ty: &str, value: &str) -> Value {
    json!({ "type": ty, "value": value })
}

fn int_id(id: i32) -> Value {
    typed("int32", &id.to_string())
}

fn update(id: i32, set: &[(&str, Value)], old: &[(&str, Value)]) -> EditOperation {
    update_id(int_id(id), set, old)
}

fn update_id(id: Value, set: &[(&str, Value)], old: &[(&str, Value)]) -> EditOperation {
    serde_json::from_value(json!({
        "key": "0",
        "kind": "update",
        "pk": { "_id": id },
        "set": set.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<serde_json::Map<_, _>>(),
        "old": old.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<serde_json::Map<_, _>>(),
    }))
    .unwrap()
}

/// The BSON bytes of a value: equal bytes are the same type and the same bits.
fn bits(v: &Bson) -> Vec<u8> {
    let mut out = Vec::new();
    doc! { "v": v.clone() }.to_writer(&mut out).unwrap();
    out
}

fn two_docs() -> Vec<Document> {
    vec![doc! { "_id": 1, "a": 1 }, doc! { "_id": 2, "a": 2 }]
}

/// Changes a of doc 1 (old value right), then a of doc 2 with an old value that is wrong.
fn batch_with_conflict_second() -> Vec<EditOperation> {
    vec![
        update(
            1,
            &[("a", typed("int32", "10"))],
            &[("a", typed("int32", "1"))],
        ),
        update(
            2,
            &[("a", typed("int32", "20"))],
            &[("a", typed("int32", "99"))],
        ),
    ]
}

#[tokio::test]
async fn replica_set_saves_the_batch_in_one_transaction() {
    let f = Fixture::new(common::mongo_rs(), two_docs()).await;
    let editable = f.editable(&f.sql()).await;
    let r = f
        .save(vec![
            update(
                1,
                &[("a", typed("int32", "10"))],
                &[("a", typed("int32", "1"))],
            ),
            update(
                2,
                &[("a", typed("int32", "20"))],
                &[("a", typed("int32", "2"))],
            ),
        ])
        .await;
    let (d1, d2) = (f.find(1).await, f.find(2).await);
    f.drop().await;
    assert_eq!(editable.transactions, Some(true));
    assert_eq!(editable.table.as_deref(), Some(f.coll.as_str()));
    assert_eq!(editable.pk, vec!["_id".to_string()]);
    assert_eq!(editable.reason, None);
    let r = r.unwrap();
    assert_eq!(r.failed, None);
    assert!(!r.partial);
    assert_eq!(d1.unwrap().get("a"), Some(&Bson::Int32(10)));
    assert_eq!(d2.unwrap().get("a"), Some(&Bson::Int32(20)));
}

#[tokio::test]
async fn replica_set_rolls_back_the_batch_when_one_change_fails() {
    let f = Fixture::new(common::mongo_rs(), two_docs()).await;
    let r = f.save(batch_with_conflict_second()).await;
    let (d1, d2) = (f.find(1).await, f.find(2).await);
    f.drop().await;
    let r = r.unwrap();
    let failed = r.failed.unwrap();
    assert_eq!(failed.index, 1);
    assert_eq!(failed.message, CONFLICT);
    assert!(!r.partial);
    assert_eq!(d1.unwrap().get("a"), Some(&Bson::Int32(1)));
    assert_eq!(d2.unwrap().get("a"), Some(&Bson::Int32(2)));
}

#[tokio::test]
async fn standalone_keeps_the_changes_before_the_failure() {
    let f = Fixture::new(common::mongo_standalone(), two_docs()).await;
    let editable = f.editable(&f.sql()).await;
    let r = f.save(batch_with_conflict_second()).await;
    let (d1, d2) = (f.find(1).await, f.find(2).await);
    f.drop().await;
    assert_eq!(editable.transactions, Some(false));
    let r = r.unwrap();
    let failed = r.failed.unwrap();
    assert_eq!(failed.index, 1);
    assert_eq!(failed.message, CONFLICT);
    assert!(r.partial);
    assert_eq!(d1.unwrap().get("a"), Some(&Bson::Int32(10)));
    assert_eq!(d2.unwrap().get("a"), Some(&Bson::Int32(2)));
}

/// A failure at the first operation saves nothing, so it is not partial.
#[tokio::test]
async fn standalone_failure_at_the_first_change_is_not_partial() {
    let f = Fixture::new(common::mongo_standalone(), two_docs()).await;
    let r = f
        .save(vec![update(
            1,
            &[("a", typed("int32", "10"))],
            &[("a", typed("int32", "5"))],
        )])
        .await;
    f.drop().await;
    let r = r.unwrap();
    assert_eq!(r.failed.unwrap().index, 0);
    assert!(!r.partial);
}

/// A document changed by someone else after it was loaded is never overwritten.
#[tokio::test]
async fn changed_document_is_a_conflict() {
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let f = Fixture::new(port, vec![doc! { "_id": 1, "a": 1, "b": "x" }]).await;
        // Another user changes a after the grid loaded a: 1.
        f.client
            .database(common::seed::STAGING)
            .collection::<Document>(&f.coll)
            .update_one(doc! { "_id": 1 }, doc! { "$set": { "a": 5 } })
            .await
            .unwrap();
        let r = f
            .save(vec![update(
                1,
                &[("a", typed("int32", "10")), ("b", typed("string", "y"))],
                &[("a", typed("int32", "1")), ("b", typed("string", "x"))],
            )])
            .await;
        // A deleted document is a conflict too.
        let gone = f
            .save(vec![update(
                9,
                &[("a", typed("int32", "1"))],
                &[("a", typed("int32", "1"))],
            )])
            .await;
        let d = f.find(1).await.unwrap();
        f.drop().await;
        let failed = r.unwrap().failed.unwrap();
        assert_eq!(
            (failed.index, failed.message.as_str()),
            (0, CONFLICT),
            "{port}"
        );
        assert_eq!(gone.unwrap().failed.unwrap().message, CONFLICT, "{port}");
        assert_eq!(d, doc! { "_id": 1, "a": 5, "b": "x" }, "{port}");
    }
}

/// Null and missing old values: `$type: "null"` and `$exists: false` match them.
#[tokio::test]
async fn null_and_missing_old_values_match() {
    let f = Fixture::new(common::mongo_rs(), vec![doc! { "_id": 1, "n": Bson::Null }]).await;
    let r = f
        .save(vec![update(
            1,
            &[("n", typed("int32", "1")), ("m", typed("string", "new"))],
            &[
                ("n", json!({ "type": "null", "value": null })),
                ("m", json!({ "type": "missing", "value": null })),
            ],
        )])
        .await;
    let d = f.find(1).await.unwrap();
    f.drop().await;
    assert_eq!(r.unwrap().failed, None);
    assert_eq!(d, doc! { "_id": 1, "n": 1, "m": "new" });
}

#[tokio::test]
async fn types_are_kept_after_the_save() {
    let oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
    let new_oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4c").unwrap();
    let nested =
        doc! { "n": 3_i64, "d": 2.0, "a": [1.0, Bson::Decimal128("0.10".parse().unwrap())] };
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let start = doc! {
            "_id": 1,
            "big": 9_007_199_254_740_993_i64,
            "dec": Bson::Decimal128("1.50".parse::<Decimal128>().unwrap()),
            "when": DateTime::from_millis(1_700_000_000_123),
            "ref": oid,
            "whole": 42.0,
            "nested": nested.clone(),
            "untouched": 7_i64,
        };
        let f = Fixture::new(port, vec![start.clone()]).await;
        let r = f
            .save(vec![update(
                1,
                &[
                    ("big", typed("int64", "9007199254740995")),
                    ("dec", typed("decimal128", "2.50")),
                    ("when", typed("date", "2024-02-29T12:34:56.789Z")),
                    ("ref", typed("objectId", &new_oid.to_hex())),
                    ("whole", typed("double", "43")),
                    (
                        "nested",
                        typed(
                            "shell",
                            "{ n: NumberLong(\"5\"), d: 1.0, a: [1.0, NumberDecimal(\"0.20\")] }",
                        ),
                    ),
                ],
                &[
                    ("big", typed("int64", "9007199254740993")),
                    ("dec", typed("decimal128", "1.50")),
                    ("when", typed("date", "2023-11-14T22:13:20.123Z")),
                    ("ref", typed("objectId", &oid.to_hex())),
                    ("whole", typed("double", "42")),
                    (
                        "nested",
                        typed("shell", &literal(&Bson::Document(nested.clone())).unwrap()),
                    ),
                ],
            )])
            .await;
        let d = f.find(1).await.unwrap();
        f.drop().await;
        assert_eq!(r.unwrap().failed, None, "{port}");
        let expected = doc! {
            "_id": 1,
            "big": 9_007_199_254_740_995_i64,
            "dec": Bson::Decimal128("2.50".parse::<Decimal128>().unwrap()),
            "when": DateTime::parse_rfc3339_str("2024-02-29T12:34:56.789Z").unwrap(),
            "ref": new_oid,
            "whole": 43.0,
            "nested": { "n": 5_i64, "d": 1.0, "a": [1.0, Bson::Decimal128("0.20".parse().unwrap())] },
            "untouched": 7_i64,
        };
        assert_eq!(
            bits(&Bson::Document(d.clone())),
            bits(&Bson::Document(expected)),
            "{port}: {d:?}"
        );
    }
}

#[tokio::test]
async fn insert_and_delete() {
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let f = Fixture::new(port, two_docs()).await;
        let ops: Vec<EditOperation> = serde_json::from_value(json!([
            { "key": "d", "kind": "delete", "pk": { "_id": int_id(1) }, "set": {} },
            { "key": "i", "kind": "insert", "pk": {}, "set": {},
              "document": "{ _id: 3, n: NumberLong(\"7\"), at: ISODate(\"2024-01-01T00:00:00Z\") }" },
        ]))
        .unwrap();
        let r = f.save(ops).await;
        let gone = f
            .save(
                serde_json::from_value(json!([
                    { "key": "d", "kind": "delete", "pk": { "_id": int_id(1) }, "set": {} },
                ]))
                .unwrap(),
            )
            .await;
        let (d1, d3, count) = (f.find(1).await, f.find(3).await, f.count().await);
        f.drop().await;
        assert_eq!(r.unwrap().failed, None, "{port}");
        let gone = gone.unwrap().failed.unwrap();
        assert_eq!(gone.message, "The document no longer exists", "{port}");
        assert_eq!(d1, None, "{port}");
        assert_eq!(
            d3,
            Some(doc! {
                "_id": 3,
                "n": 7_i64,
                "at": DateTime::parse_rfc3339_str("2024-01-01T00:00:00Z").unwrap(),
            }),
            "{port}"
        );
        assert_eq!(count, 2, "{port}");
    }
}

/// A value that does not convert fails its operation before anything runs.
#[tokio::test]
async fn conversion_error_saves_nothing() {
    let f = Fixture::new(common::mongo_standalone(), two_docs()).await;
    let r = f
        .save(vec![
            update(
                1,
                &[("a", typed("int32", "10"))],
                &[("a", typed("int32", "1"))],
            ),
            update(
                2,
                &[("a", typed("int32", "x"))],
                &[("a", typed("int32", "2"))],
            ),
        ])
        .await;
    let d1 = f.find(1).await;
    f.drop().await;
    let r = r.unwrap();
    let failed = r.failed.unwrap();
    assert_eq!(failed.index, 1);
    assert_eq!(failed.message, "a: Invalid int32 value: x");
    assert!(!r.partial);
    assert_eq!(d1.unwrap().get("a"), Some(&Bson::Int32(1)));
}

#[tokio::test]
async fn read_only_connection_is_refused() {
    let (_d, st) = common::state();
    let mut input = rw(common::mongo_rs());
    input.user = "sandb_ro".into();
    input.read_only = true;
    let c = st
        .create_connection(input, Some(common::ro_password()))
        .unwrap();
    let sql = "db.orders.find()".to_string();
    let editable = ops::editable_result(&st, EditableRequest {
            conn_id: c.id.clone(),
            database: common::seed::STAGING.into(),
            sql: sql.clone(),
        }, )
    .await
    .unwrap();
    let e = ops::apply_edits(&st, ApplyEditsRequest {
            conn_id: c.id,
            database: common::seed::STAGING.into(),
            sql,
            tab_id: None,
            query_id: None,
            operations: vec![update(
                1,
                &[("a", typed("int32", "1"))],
                &[("a", typed("int32", "2"))],
            )],
        }, )
    .await
    .unwrap_err();
    assert_eq!(
        editable.reason.as_deref(),
        Some("The connection is read-only")
    );
    assert_eq!(e.code, ErrorCode::ConnectionReadOnly);
}

#[tokio::test]
async fn editable_result_reasons() {
    let f = Fixture::new(common::mongo_rs(), two_docs()).await;
    let coll = f.coll.clone();
    let mut reasons = Vec::new();
    for sql in [
        format!("db.{coll}.aggregate([])"),
        format!("db.{coll}.find().count()"),
        format!("db.{coll}.find({{}}, {{_id: 0}})"),
        format!("db.{coll}.find({{}}, {{t: {{$toUpper: '$a'}}}})"),
        "db.order_summary.find()".into(),
        "db.no_such_collection_here.find()".into(),
        "db.system.views.find()".into(),
        "db.orders.find(".into(),
    ] {
        reasons.push(f.editable(&sql).await.reason);
    }
    let projected = f.editable(&format!("db.{coll}.find({{}}, {{a: 1}})")).await;
    let view_save = ops::apply_edits(&f.st, ApplyEditsRequest {
            conn_id: f.conn_id.clone(),
            database: common::seed::STAGING.into(),
            sql: "db.order_summary.find()".into(),
            tab_id: None,
            query_id: None,
            operations: vec![],
        }, )
    .await;
    f.drop().await;
    let not_find = Some("Only the result of one find() on a collection can be edited".to_string());
    assert_eq!(
        reasons,
        vec![
            not_find.clone(),
            not_find.clone(),
            Some("The projection hides _id".into()),
            Some("The projection computes or hides fields".into()),
            Some("Views are read-only".into()),
            Some("The collection does not exist".into()),
            Some("System collections are read-only".into()),
            not_find,
        ]
    );
    assert_eq!(projected.reason, None);
    assert_eq!(projected.transactions, Some(true));
    let e = view_save.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert_eq!(e.message, "Views are read-only");
}

#[tokio::test]
async fn cell_value_reads_the_full_value() {
    let long = "é".repeat(5000);
    let f = Fixture::new(
        common::mongo_rs(),
        vec![doc! {
            "_id": 1,
            "long": long.clone(),
            "doc": { "n": 5_i64, "d": 1.0 },
            "deep": { "a": { "b": 2 } },
        }],
    )
    .await;
    let long_cell = f.cell(int_id(1), "long").await;
    let doc_cell = f.cell(int_id(1), "doc").await;
    let deep_cell = f.cell(int_id(1), "deep.a.b").await;
    let missing = f.cell(int_id(1), "nope").await;
    let gone = f.cell(int_id(2), "long").await;
    f.drop().await;
    assert_eq!(long_cell.unwrap(), Some(long));
    assert_eq!(
        doc_cell.unwrap().as_deref(),
        Some("{\n  \"n\": NumberLong(\"5\"),\n  \"d\": Double(1.0)\n}")
    );
    assert_eq!(deep_cell.unwrap().as_deref(), Some("2"));
    assert_eq!(missing.unwrap(), None);
    let e = gone.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(e.message, "The document no longer exists");
}

/// After `use analytics` in the run, the result carries its own database and an edit sent to it
/// changes that database only.
#[tokio::test]
async fn use_in_the_run_sets_the_database_of_the_result() {
    let mut f = Fixture::new(common::mongo_rs(), two_docs()).await;
    f.track_in("analytics");
    f.seed("analytics", two_docs()).await;
    let run = ops::run_query(&f.st, RunRequest {
            conn_id: f.conn_id.clone(),
            database: Some(common::seed::STAGING.into()),
            sql: format!("use analytics\ndb.{}.find()", f.coll),
            query_id: uuid::Uuid::new_v4().to_string(),
            limit: None,
            tab_id: None,
            sort: None,
        }, )
    .await;
    let edited = match &run {
        Ok(run) => Some(
            ops::apply_edits(&f.st, ApplyEditsRequest {
                    conn_id: f.conn_id.clone(),
                    database: run.statements[1].database.clone().unwrap_or_default(),
                    sql: run.statements[1].sql.clone(),
                    tab_id: None,
                    query_id: None,
                    operations: vec![update(
                        1,
                        &[("a", typed("int32", "10"))],
                        &[("a", typed("int32", "1"))],
                    )],
                }, )
            .await,
        ),
        Err(_) => None,
    };
    let analytics = f
        .client
        .database("analytics")
        .collection::<Document>(&f.coll)
        .find_one(doc! { "_id": 1 })
        .await
        .unwrap();
    let staging = f.find(1).await;
    f.client
        .database("analytics")
        .collection::<Document>(&f.coll)
        .drop()
        .await
        .unwrap();
    f.drop().await;
    let run = run.unwrap();
    assert_eq!(run.statements[0].database.as_deref(), Some("analytics"));
    assert_eq!(run.statements[1].database.as_deref(), Some("analytics"));
    assert_eq!(run.statements[1].sql, format!("db.{}.find()", f.coll));
    assert_eq!(edited.unwrap().unwrap().failed, None);
    assert_eq!(analytics.unwrap().get("a"), Some(&Bson::Int32(10)));
    assert_eq!(staging.unwrap().get("a"), Some(&Bson::Int32(1)));
}

/// The old value matches only itself: not a missing field for null, not null for missing, not an
/// array that holds it, not the same number of another type.
#[tokio::test]
async fn old_value_must_match_exactly() {
    let f = Fixture::new(
        common::mongo_rs(),
        vec![
            doc! { "_id": 1 },
            doc! { "_id": 2, "a": Bson::Null },
            doc! { "_id": 3, "a": [5, 6] },
            doc! { "_id": 4, "a": 5_i64 },
            doc! { "_id": 5, "a": [Bson::Null, Bson::Int32(5)] },
            doc! { "_id": 6, "a": [[1, 2], 3] },
            doc! { "_id": 7, "a": [{ "x": 1 }] },
            doc! { "_id": 8, "a": [[1, 2], [3]] },
            doc! { "_id": 9, "a": [1, 2] },
            doc! { "_id": 10, "a": 5.0 },
        ],
    )
    .await;
    let null = json!({ "type": "null", "value": null });
    let missing = json!({ "type": "missing", "value": null });
    let cases = [
        (1, null.clone()),
        (2, missing),
        (3, typed("int32", "5")),
        (4, typed("int32", "5")),
        (5, null),
        (6, typed("shell", "[1, 2]")),
        (7, typed("shell", "{ x: 1 }")),
        (8, typed("shell", "[[1, 2], [3]]")),
        (9, typed("shell", "[1, 2]")),
        (10, typed("double", "5")),
    ];
    let mut messages = Vec::new();
    for (id, old) in cases {
        let r = f
            .save(vec![update(
                id,
                &[("a", typed("string", "new"))],
                &[("a", old)],
            )])
            .await;
        messages.push(r.map(|r| r.failed.map(|failed| failed.message)));
    }
    f.drop().await;
    let conflict = Ok(Some(CONFLICT.to_string()));
    assert_eq!(
        messages,
        vec![
            conflict.clone(),
            conflict.clone(),
            conflict.clone(),
            conflict.clone(),
            conflict.clone(),
            conflict.clone(),
            conflict,
            // Exactly equal arrays, nested ones too, and the same double still match.
            Ok(None),
            Ok(None),
            Ok(None),
        ]
    );
}

/// A case-insensitive default collation of the collection does not loosen the match.
#[tokio::test]
async fn collection_collation_does_not_loosen_the_match() {
    let f = Fixture::open(common::mongo_rs()).await;
    f.client
        .database(common::seed::STAGING)
        .create_collection(&f.coll)
        .collation(
            Collation::builder()
                .locale("en")
                .strength(CollationStrength::Secondary)
                .build(),
        )
        .await
        .unwrap();
    f.seed(common::seed::STAGING, vec![doc! { "_id": "A", "s": "X" }]).await;
    let by_id = f
        .save(vec![update_id(
            typed("string", "a"),
            &[("s", typed("string", "Y"))],
            &[("s", typed("string", "X"))],
        )])
        .await;
    let by_old = f
        .save(vec![update_id(
            typed("string", "A"),
            &[("s", typed("string", "Y"))],
            &[("s", typed("string", "x"))],
        )])
        .await;
    let cell = f.cell(typed("string", "a"), "s").await;
    let d = f
        .client
        .database(common::seed::STAGING)
        .collection::<Document>(&f.coll)
        .find_one(doc! { "_id": "A" })
        .await
        .unwrap();
    f.drop().await;
    assert_eq!(by_id.unwrap().failed.unwrap().message, CONFLICT);
    assert_eq!(by_old.unwrap().failed.unwrap().message, CONFLICT);
    assert_eq!(cell.unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(d, Some(doc! { "_id": "A", "s": "X" }));
}
