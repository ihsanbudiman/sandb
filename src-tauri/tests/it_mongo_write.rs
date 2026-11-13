//! Integration tests for `core::mongo::write`: every write and schema statement runs against the
//! sandb-test docker stack (`pnpm db:up`) through the same `execute` the run path uses.
//! Run: `cargo test --features it --test it_mongo_write`.
#![cfg(feature = "it")]

mod common;

use mongodb::bson::{Bson, Document, doc};
use sandb_lib::core::connections::ConnectionInput;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::mongo;
use sandb_lib::core::mongo::command::Command;
use sandb_lib::core::mongo::comment::OpComment;
use sandb_lib::core::mongo::write::WriteOutcome;
use sandb_lib::core::state::AppState;

use common::Cleanup;

/// Read-write user on `shop_staging`.
fn rw(port: u16) -> ConnectionInput {
    common::mongo_input("mongo-write", port, &common::rw_user(), common::seed::STAGING, false)
}

/// Read-only user; only the server's own roles stop a write.
fn ro(port: u16) -> ConnectionInput {
    common::mongo_input(
        "mongo-write-ro",
        port,
        &common::ro_user(),
        common::seed::STAGING,
        true,
    )
}

/// A `sandb_rw` client, opened through the connection's own `mongo::client::client`.
async fn rw_client(st: &AppState) -> mongodb::Client {
    let c = st
        .create_connection(rw(common::mongo_rs()), Some(common::rw_password()))
        .unwrap();
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
    mongo::write::execute(
        client,
        "shop_staging",
        &command(text),
        OpComment::new("it-mongo-write"),
    )
    .await
    .unwrap()
}

/// Runs one statement and returns its error.
async fn execute_err(client: &mongodb::Client, text: &str) -> AppError {
    mongo::write::execute(
        client,
        "shop_staging",
        &command(text),
        OpComment::new("it-mongo-write"),
    )
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

#[tokio::test]
async fn insert_one_inserts_one() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    let out = run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let count = count(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(count, 1);
    assert_eq!(out.affected, 1);
    assert_eq!(out.document, None);
}

#[tokio::test]
async fn insert_many_inserts_three() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    let out = run(
        &client,
        &format!("db.{coll}.insertMany([{{a: 1}}, {{a: 2}}, {{a: 3}}])"),
    )
    .await;
    let count = count(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(count, 3);
    assert_eq!(out.affected, 3);
}

#[tokio::test]
async fn update_many_reports_matched_and_modified() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    run(
        &client,
        &format!("db.{coll}.insertMany([{{n: 1}}, {{n: 2}}, {{n: 3}}])"),
    )
    .await;
    let text = format!("db.{coll}.updateMany({{n: {{$gte: 2}}}}, {{$set: {{done: true}}}})");
    let first = run(&client, &text).await;
    let second = run(&client, &text).await;
    let count = count(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(first.affected, 2);
    // The second run matches the same documents but changes nothing.
    assert_eq!(second.affected, 0);
    assert_eq!(count, 3);
}

#[tokio::test]
async fn update_one_upserts_when_nothing_matches() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(out.affected, 1);
    assert_eq!(count, 2);
    assert!(upserted.is_some(), "the upserted document exists");
}

#[tokio::test]
async fn replace_one_replaces_the_document() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(out.affected, 1);
    assert_eq!(replaced, Some(doc! { "_id": 1, "a": 2 }));
}

#[tokio::test]
async fn find_one_and_update_returns_the_new_document() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(
        after.document.as_ref().unwrap().get("n"),
        Some(&Bson::Int32(2)),
        "returnDocument: 'after' returns the modified document"
    );
    assert_eq!(after.affected, 1);
    assert_eq!(none.document, None);
    assert_eq!(none.affected, 0);
}

#[tokio::test]
async fn delete_many_reports_the_count() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    run(
        &client,
        &format!(
            "db.{coll}.insertMany([{{gone: true}}, {{gone: true}}, {{gone: true}}, {{n: 1}}])"
        ),
    )
    .await;
    let out = run(&client, &format!("db.{coll}.deleteMany({{gone: true}})")).await;
    let count = count(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(out.affected, 3);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn bulk_write_runs_mixed_models() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(out.affected, 7); // inserted + modified + deleted + upserted
    assert_eq!(count, 2);
}

/// With `ordered: false` a failing operation does not stop the rest; the first error is reported
/// at the end.
#[tokio::test]
async fn bulk_write_unordered_continues_after_a_failure() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    // A failed bulk write is the server's write error: its own message, with the code as label.
    assert!(e.message.contains("E11000 duplicate key error"), "{e:?}");
    assert_eq!(
        e.server_code.as_ref().map(|c| c.code.as_str()),
        Some("11000"),
        "{e:?}"
    );
    assert_eq!(e.detail.as_deref(), Some("MongoDB error 11000"), "{e:?}");
    assert_eq!(count, 2, "the operations after the failure still ran");
}

/// With `ordered: true` (the default) the first failure stops the run.
#[tokio::test]
async fn bulk_write_ordered_stops_at_the_first_failure() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert!(e.message.contains("E11000 duplicate key error"), "{e:?}");
    assert_eq!(
        e.server_code.as_ref().map(|c| c.code.as_str()),
        Some("11000"),
        "{e:?}"
    );
    assert_eq!(count, 1, "the operations after the failure did not run");
}

#[tokio::test]
async fn create_index_with_a_name_then_drop_it() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let created = run(
        &client,
        &format!("db.{coll}.createIndex({{a: 1}}, {{name: 'it_a'}})"),
    )
    .await;
    let indexes = index_names(&client, &coll).await;
    let dropped = run(&client, &format!("db.{coll}.dropIndex('it_a')")).await;
    let after = index_names(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(created.affected, 0);
    assert_eq!(dropped.affected, 0);
    assert!(indexes.contains(&"it_a".to_string()), "{indexes:?}");
    assert!(!after.contains(&"it_a".to_string()), "{after:?}");
}

#[tokio::test]
async fn create_indexes_then_drop_them() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    run(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
    let created = run(
        &client,
        &format!("db.{coll}.createIndexes([{{a: 1}}, {{b: -1}}])"),
    )
    .await;
    let dropped = run(&client, &format!("db.{coll}.dropIndexes()")).await;
    let after = index_names(&client, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    assert_eq!(created.affected, 0);
    assert_eq!(dropped.affected, 0);
    assert_eq!(after, vec!["_id_".to_string()]);
}

#[tokio::test]
async fn rename_collection_then_drop_the_rename() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
    common::drop_scratch(&client, common::seed::STAGING, &renamed).await;
    assert_eq!(out.affected, 0);
    assert_eq!(moved, 1, "the documents moved to the new name");
    assert_eq!(gone, 0, "the old name is gone");
    assert_eq!(drop_out.affected, 0);
}

/// The notices are the user-facing text of a write: one test pins each message, the behaviour
/// tests above assert counts and documents only.
#[tokio::test]
async fn write_notices_name_the_method_and_the_collection() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
    let mut notices = Vec::new();

    notices.push(run(&client, &format!("db.{coll}.insertOne({{n: 1}})")).await.notice);
    notices.push(
        run(
            &client,
            &format!("db.{coll}.insertMany([{{n: 2}}, {{n: 3}}])"),
        )
        .await
        .notice,
    );
    notices.push(
        run(
            &client,
            &format!("db.{coll}.updateMany({{n: {{$gte: 2}}}}, {{$set: {{done: true}}}})"),
        )
        .await
        .notice,
    );
    notices.push(
        run(
            &client,
            &format!("db.{coll}.updateOne({{n: 99}}, {{$set: {{n: 99}}}}, {{upsert: true}})"),
        )
        .await
        .notice,
    );
    notices.push(
        run(
            &client,
            &format!("db.{coll}.replaceOne({{n: 1}}, {{n: 1, done: false}})"),
        )
        .await
        .notice,
    );
    notices.push(
        run(
            &client,
            &format!("db.{coll}.findOneAndUpdate({{n: 1}}, {{$inc: {{n: 1}}}})"),
        )
        .await
        .notice,
    );
    notices.push(
        run(&client, &format!("db.{coll}.deleteMany({{done: true}})")).await.notice,
    );
    notices.push(
        run(
            &client,
            &format!(
                "db.{coll}.bulkWrite([{{insertOne: {{document: {{n: 4}}}}}}, \
                 {{updateOne: {{filter: {{n: 1}}, update: {{$set: {{done: false}}}}}}}}, \
                 {{deleteOne: {{filter: {{n: 4}}}}}}])"
            ),
        )
        .await
        .notice,
    );
    notices.push(
        run(
            &client,
            &format!("db.{coll}.createIndex({{n: 1}}, {{name: 'it_notice'}})"),
        )
        .await
        .notice,
    );
    notices.push(
        run(&client, &format!("db.{coll}.dropIndex('it_notice')"))
            .await
            .notice,
    );
    notices.push(
        run(&client, &format!("db.{coll}.createIndexes([{{n: 1}}, {{m: -1}}])"))
            .await
            .notice,
    );
    notices.push(
        run(&client, &format!("db.{coll}.dropIndexes()"))
            .await
            .notice,
    );
    let renamed = format!("{coll}_renamed");
    clean.collection(&renamed);
    notices.push(
        run(
            &client,
            &format!("db.{coll}.renameCollection('{renamed}', true)"),
        )
        .await
        .notice,
    );
    notices.push(
        run(&client, &format!("db.{renamed}.drop()"))
            .await
            .notice,
    );

    assert_eq!(
        notices,
        vec![
            format!("insertOne on {coll}: inserted 1"),
            format!("insertMany on {coll}: inserted 2"),
            format!("updateMany on {coll}: matched 2, modified 2"),
            format!("updateOne on {coll}: matched 0, modified 0, upserted 1"),
            format!("replaceOne on {coll}: matched 1, modified 1"),
            format!("findOneAndUpdate on {coll}: 1 document"),
            format!("deleteMany on {coll}: deleted 2"),
            format!("bulkWrite on {coll}: inserted 1, matched 0, modified 0, deleted 1, upserted 0"),
            format!("createIndex on {coll}: created it_notice"),
            format!("dropIndex on {coll}: dropped it_notice"),
            format!("createIndexes on {coll}: created n_1, m_-1"),
            format!("dropIndexes on {coll}: dropped all but _id"),
            format!("renameCollection: {coll} -> {renamed}"),
            format!("drop: dropped {renamed}"),
        ]
    );
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
}

/// An option the method does not know is refused before anything runs.
#[tokio::test]
async fn unknown_option_is_refused_before_anything_runs() {
    let (_d, st) = common::state();
    let client = rw_client(&st).await;
    let mut clean = Cleanup::new(common::mongo_rs());
    let coll = clean.collection(common::scratch());
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
    common::drop_scratch(&client, common::seed::STAGING, &coll).await;
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
    for port in [common::mongo_rs(), common::mongo_standalone()] {
        let (_d, st) = common::state();
        let c = st
            .create_connection(ro(port), Some(common::ro_password()))
            .unwrap();
        let client = mongo::client::client(st.as_ref(), &c.id).await.unwrap();
        let mut clean = Cleanup::new(port);
        let coll = clean.collection(common::scratch());
        let e = execute_err(&client, &format!("db.{coll}.insertOne({{a: 1}})")).await;
        assert_eq!(
            e.server_code.as_ref().map(|c| c.code.as_str()),
            Some("13"), // Unauthorized
            "{port}: {e:?}"
        );
    }
}
