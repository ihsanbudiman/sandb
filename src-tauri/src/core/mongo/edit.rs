//! Grid edits on MongoDB: which `find` results can be edited, the typed values of the cell editor
//! and the save of a batch of changes.
//!
//! Every value keeps its BSON type: the grid sends the type with the text
//! (`{ "type": "int64", "value": "42" }`) and `value::typed` builds exactly that type, so an
//! Int64 or a Decimal128 never comes back as a Double. An update filters on `_id` and on the old
//! value of each changed field, so a document that changed since it was loaded is never
//! overwritten.

use std::collections::BTreeMap;

use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{Collation, DeleteOptions, FindOneOptions, InsertOneOptions, UpdateOptions};
use mongodb::{Client, ClientSession, Collection};
use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, Topology, target_database};
use crate::core::engine::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{
    ApplyEditsRequest, ApplyEditsResult, CellValue, CellValueRequest, EditFailure, EditKind,
    EditOperation, EditableRequest, EditableResult, MongoCollectionKind, TypedValue,
};
use crate::core::mongo::client::Access;
use crate::core::mongo::command::{Command, Method};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::conn::topology_of;
use crate::core::mongo::reply;
use crate::core::mongo::value::{self, from_wire, type_alias};
use crate::core::mongo::{client, introspect, parse, WRITE_REPLY_WAIT};

/// The `comment` of a grid edit that has no run of its own.
const GRID_EDIT: &str = "sandb grid edit";
const NOT_FIND: &str = "Only the result of one find() on a collection can be edited";
const COMPUTED: &str = "The projection computes or hides fields";
const CONFLICT: &str =
    "The document changed or no longer exists since it was loaded. Nothing was overwritten.";
const GONE: &str = "The document no longer exists";

fn value_at(path: &str, v: &TypedValue) -> Result<Option<Bson>, String> {
    from_wire(v).map_err(|e| format!("{path}: {}", e.message))
}

fn typed_at(path: &str, v: &CellValue) -> Result<TypedValue, Refusal> {
    serde_json::from_value(v.clone())
        .map_err(|_| Refusal::message(format!("{path}: A value needs a type and a text")))
}

struct Target {
    collection: String,
    transactions: bool,
}

fn find_collection(sql: &str) -> Result<String, &'static str> {
    let Ok(mut statements) = parse::parse(sql) else {
        return Err(NOT_FIND);
    };
    if statements.len() != 1 {
        return Err(NOT_FIND);
    }
    let Command::Collection {
        collection,
        method: Method::Find(find),
    } = statements.remove(0).command
    else {
        return Err(NOT_FIND);
    };
    if find.count {
        return Err(NOT_FIND);
    }
    if let Some(projection) = &find.projection {
        check_projection(projection)?;
    }
    if collection.starts_with("system.") {
        return Err("System collections are read-only");
    }
    Ok(collection)
}

/// Only plain include/exclude projections keep each shown value equal to the stored one.
fn check_projection(projection: &Document) -> Result<(), &'static str> {
    for (key, value) in projection {
        // `a.$` shows only the matched array element: a save would write it back as the array.
        if key.contains('$') {
            return Err(COMPUTED);
        }
        let n = match value {
            Bson::Boolean(b) => f64::from(u8::from(*b)),
            Bson::Int32(n) => f64::from(*n),
            Bson::Int64(n) => *n as f64,
            Bson::Double(n) => *n,
            Bson::Decimal128(d) => d.to_string().parse().map_err(|_| COMPUTED)?,
            _ => return Err(COMPUTED),
        };
        if n != 0.0 && n != 1.0 {
            return Err(COMPUTED);
        }
        if key == "_id" && n == 0.0 {
            return Err("The projection hides _id");
        }
    }
    Ok(())
}

/// A step that did not finish: the result cannot be edited (a message for the user, not an
/// error), or the request itself is malformed.
#[derive(Debug)]
enum Refusal {
    Message(String),
    Invalid(AppError),
}

impl From<AppError> for Refusal {
    fn from(e: AppError) -> Self {
        Refusal::Invalid(e)
    }
}

impl From<mongodb::error::Error> for Refusal {
    fn from(e: mongodb::error::Error) -> Self {
        Refusal::Invalid(e.into())
    }
}

impl From<Refusal> for AppError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Message(message) => AppError::invalid(message),
            Refusal::Invalid(error) => error,
        }
    }
}

/// The Mongo collection of a find result, or why it cannot be edited. The `listCollections`
/// lookup is the schema browser's, so "does it exist / is it a view" has one implementation.
async fn editable_collection(
    client: &Client,
    database: &str,
    sql: &str,
) -> Result<String, Refusal> {
    if database.is_empty() {
        return Err(Refusal::Message(
            "Choose a database for this tab first".into(),
        ));
    }
    let collection = find_collection(sql).map_err(|reason| Refusal::Message(reason.into()))?;
    let entry = introspect::collection_entry(&client.database(database), &collection, true).await?;
    match entry {
        None => return Err(Refusal::Message("The collection does not exist".into())),
        Some(entry)
            if reply::named_kind(entry.get_str("type").ok()) == MongoCollectionKind::View =>
        {
            return Err(Refusal::Message("Views are read-only".into()));
        }
        Some(_) => {}
    }
    Ok(collection)
}

/// Whether a save can use one transaction: a replica set or a sharded cluster, not a standalone.
async fn transactions(client: &Client, comment: OpComment<'_>) -> Result<bool, Refusal> {
    let hello = client
        .database("admin")
        .run_command(doc! { "hello": 1, "comment": comment.as_str() })
        .await?;
    Ok(topology_of(&hello) != Topology::Standalone)
}

async fn target(
    client: &Client,
    database: &str,
    sql: &str,
    comment: OpComment<'_>,
) -> Result<Target, Refusal> {
    let collection = editable_collection(client, database, sql).await?;
    let transactions = transactions(client, comment).await?;
    Ok(Target {
        collection,
        transactions,
    })
}

/// `edit::editable_result` for a MongoDB connection.
pub async fn editable_result(
    access: &impl Access,
    conn: &Connection,
    request: EditableRequest,
) -> Result<EditableResult, AppError> {
    if !conn.may_write() {
        return Ok(EditableResult::not_editable("The connection is read-only"));
    }
    let database = target_database(conn, Some(&request.database));
    let client = client::client(access, &conn.id).await?;
    Ok(
        match target(&client, &database, &request.sql, OpComment::new(GRID_EDIT)).await {
            Ok(t) => EditableResult {
                table: Some(t.collection),
                schema: None,
                pk: vec!["_id".into()],
                reason: None,
                transactions: Some(t.transactions),
            },
            Err(Refusal::Message(reason)) => EditableResult::not_editable(reason),
            Err(e) => return Err(e.into()),
        },
    )
}

/// One operation of a save, prepared before any runs, so a value that does not convert saves
/// nothing.
#[derive(Debug, PartialEq)]
enum Write {
    Update { filter: Document, update: Document },
    Delete { filter: Document },
    Insert { document: Document },
}

impl Write {
    /// Builds one operation. A malformed request is [`Refusal::Invalid`]; a value that does not
    /// convert is the row's failure message.
    fn prepare(op: &EditOperation) -> Result<Write, Refusal> {
        match op.kind {
            EditKind::Insert => {
                let Some(text) = &op.document else {
                    return Err(Refusal::Invalid(AppError::invalid(
                        "An insert needs a document",
                    )));
                };
                match parse::parse_value(text) {
                    Ok(Bson::Document(document)) => Ok(Write::Insert { document }),
                    Ok(_) => Err(Refusal::message(
                        "The new document must be a document: { … }",
                    )),
                    Err(e) => Err(Refusal::message(format!("New document: {e}"))),
                }
            }
            EditKind::Delete => Ok(Write::Delete {
                filter: id_filter(&op.pk)?,
            }),
            EditKind::Update => {
                if op.set.is_empty() {
                    return Err(Refusal::Invalid(AppError::invalid(
                        "An update needs at least one field",
                    )));
                }
                if let Some(path) = op.set.keys().find(|p| !op.old.contains_key(*p)) {
                    return Err(Refusal::Invalid(AppError::invalid(format!(
                        "The change of {path} has no old value"
                    ))));
                }
                let mut filter = id_filter(&op.pk)?;
                let mut set = Document::new();
                for (path, new) in &op.set {
                    let new = typed_at(path, new)?;
                    let new = match value_at(path, &new) {
                        Ok(Some(v)) => v,
                        Ok(None) => {
                            return Err(Refusal::message(format!(
                                "{path}: a new value cannot be missing"
                            )));
                        }
                        Err(message) => return Err(Refusal::message(message)),
                    };
                    let old = match value_at(path, &op.old[path]) {
                        Ok(v) => v,
                        Err(message) => return Err(Refusal::message(message)),
                    };
                    filter.insert(path.clone(), old_condition(old));
                    set.insert(path.clone(), new);
                }
                Ok(Write::Update {
                    filter,
                    update: doc! { "$set": set },
                })
            }
        }
    }

    /// Runs the operation, in `session` when the save is in a transaction. `Some(message)` when
    /// it failed or changed nothing.
    async fn run(
        self,
        coll: &Collection<Document>,
        session: Option<&mut ClientSession>,
        comment: OpComment<'_>,
    ) -> Option<String> {
        let r = match self {
            Write::Update { filter, update } => {
                let mut o = UpdateOptions::default();
                o.comment = Some(comment.tag());
                o.collation = Some(simple_collation());
                let a = coll.update_one(filter, update).with_options(o);
                match session {
                    Some(s) => a.session(s).await,
                    None => a.await,
                }
                .map(|r| (r.matched_count == 0).then_some(CONFLICT))
            }
            Write::Delete { filter } => {
                let mut o = DeleteOptions::default();
                o.comment = Some(comment.tag());
                o.collation = Some(simple_collation());
                let a = coll.delete_one(filter).with_options(o);
                match session {
                    Some(s) => a.session(s).await,
                    None => a.await,
                }
                .map(|r| (r.deleted_count == 0).then_some(GONE))
            }
            Write::Insert { document } => {
                let mut o = InsertOneOptions::default();
                o.comment = Some(comment.tag());
                let a = coll.insert_one(document).with_options(o);
                match session {
                    Some(s) => a.session(s).await,
                    None => a.await,
                }
                .map(|_| None)
            }
        };
        match r {
            Ok(None) => None,
            Ok(Some(message)) => Some(message.into()),
            Err(e) => Some(AppError::from(e).message),
        }
    }
}

impl Refusal {
    fn message(text: impl Into<String>) -> Self {
        Refusal::Message(text.into())
    }
}

/// `{ _id: <value> }` from the `pk` of an operation.
fn id_filter(pk: &BTreeMap<String, CellValue>) -> Result<Document, Refusal> {
    let (Some(id), 1) = (pk.get("_id"), pk.len()) else {
        return Err(Refusal::Invalid(AppError::invalid(
            "The request must contain exactly the _id value",
        )));
    };
    let id = typed_at("_id", id)?;
    match value_at("_id", &id) {
        Ok(Some(id)) => Ok(doc! { "_id": id }),
        Ok(None) => Err(Refusal::message("_id: a document always has an _id")),
        Err(message) => Err(Refusal::message(message)),
    }
}

/// The condition on the old value of a changed field. It matches only that exact value: `$eq`
/// alone also matches an array that holds the value and a number of another type, so a scalar
/// adds its `$type` and refuses arrays, and an array refuses arrays that hold it as an element.
fn old_condition(old: Option<Bson>) -> Document {
    match old {
        None => doc! { "$exists": false },
        Some(Bson::Null) => doc! { "$type": "null", "$not": { "$type": "array" } },
        Some(v @ Bson::Array(_)) => {
            doc! { "$eq": v.clone(), "$not": { "$elemMatch": { "$eq": v } } }
        }
        Some(v) => {
            let ty = type_alias(&v);
            doc! { "$eq": v, "$type": ty, "$not": { "$type": "array" } }
        }
    }
}

/// Binary comparison, so a case-insensitive default collation of the collection does not loosen
/// the match on `_id` or on the old values.
fn simple_collation() -> Collation {
    Collation::builder().locale("simple").build()
}

fn failed(index: usize, message: String, partial: bool) -> ApplyEditsResult {
    ApplyEditsResult {
        failed: Some(EditFailure { index, message }),
        partial,
    }
}

const SAVED: ApplyEditsResult = ApplyEditsResult {
    failed: None,
    partial: false,
};

/// Wakes when the save's run is cancelled. Without a run the future is never ready, so the
/// selects below take the operation arm.
async fn cancelled(token: Option<&CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// The cancel ends the local save. The server kill is best effort, as it is for a run.
fn save_cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Save cancelled")
}

/// The local save stopped, but the commit may still have reached the server.
fn save_maybe_applied() -> AppError {
    save_cancelled().with_detail("The save may have applied before the cancel. Reload to check.")
}

/// Replica set or sharded cluster: all operations in one transaction; a failure saves nothing.
/// A cancel that lands before the commit aborts the transaction, so nothing is saved; one that
/// lands during the commit gets a moment for its reply, like a cancelled write in `run.rs`.
async fn in_transaction(
    client: &Client,
    coll: &Collection<Document>,
    writes: Vec<Write>,
    cancelled_run: Option<&CancellationToken>,
    comment: OpComment<'_>,
) -> Result<ApplyEditsResult, AppError> {
    let mut session = client.start_session().await?;
    session.start_transaction().await?;
    for (index, write) in writes.into_iter().enumerate() {
        if cancelled_run.is_some_and(CancellationToken::is_cancelled) {
            abort(&mut session).await;
            return Err(save_cancelled());
        }
        // The operation races the cancel; the block ends first, so the session is free again.
        let finished = {
            let op = write.run(coll, Some(&mut session), comment);
            tokio::pin!(op);
            tokio::select! {
                biased;
                () = cancelled(cancelled_run) => None,
                r = &mut op => Some(r),
            }
        };
        match finished {
            None => {
                abort(&mut session).await;
                return Err(save_cancelled());
            }
            Some(Some(message)) => {
                abort(&mut session).await;
                return Ok(failed(index, message, false));
            }
            Some(None) => {}
        }
    }
    if cancelled_run.is_some_and(CancellationToken::is_cancelled) {
        abort(&mut session).await;
        return Err(save_cancelled());
    }
    // A commit already sent may still apply: give its reply the same window as a write.
    let commit = async { session.commit_transaction().await };
    tokio::pin!(commit);
    let committed = tokio::select! {
        biased;
        () = cancelled(cancelled_run) => {
            match tokio::time::timeout(WRITE_REPLY_WAIT, &mut commit).await {
                Ok(r) => r,
                Err(_) => return Err(save_maybe_applied()),
            }
        }
        r = &mut commit => r,
    };
    committed?;
    Ok(SAVED)
}

/// Aborts the transaction, best effort: the server may be the reason for the cancel.
async fn abort(session: &mut ClientSession) {
    let _ = tokio::time::timeout(WRITE_REPLY_WAIT, session.abort_transaction()).await;
}

/// Standalone server, no transactions: the operations run one by one and stop at the first
/// failure; the ones before it stay saved. A cancel stops the next operation and says how many
/// changes were already saved.
async fn one_by_one(
    coll: &Collection<Document>,
    writes: Vec<Write>,
    cancelled_run: Option<&CancellationToken>,
    comment: OpComment<'_>,
) -> Result<ApplyEditsResult, AppError> {
    for (index, write) in writes.into_iter().enumerate() {
        if cancelled_run.is_some_and(CancellationToken::is_cancelled) {
            return Err(saved_before_cancel(index));
        }
        let finished = {
            let op = write.run(coll, None, comment);
            tokio::pin!(op);
            tokio::select! {
                biased;
                () = cancelled(cancelled_run) => None,
                r = &mut op => Some(r),
            }
        };
        match finished {
            None => return Err(saved_before_cancel(index)),
            Some(Some(message)) => return Ok(failed(index, message, index > 0)),
            Some(None) => {}
        }
    }
    Ok(SAVED)
}

fn saved_before_cancel(saved: usize) -> AppError {
    let detail = match saved {
        0 => "No change was saved before the cancel.".to_string(),
        1 => "1 change was saved before the cancel.".to_string(),
        n => format!("{n} changes were saved before the cancel."),
    };
    save_cancelled().with_detail(format!("{detail} Reload to check."))
}

/// `edit::apply_edits` for a MongoDB connection; the caller has refused a read-only connection.
pub async fn apply_edits(
    access: &impl Access,
    conn: &Connection,
    request: ApplyEditsRequest,
) -> Result<ApplyEditsResult, AppError> {
    let database = target_database(conn, Some(&request.database));
    let client = client::client(access, &conn.id).await?;
    // The dialog's id puts the save in the editor's registry, so `cancel_query` reaches it with
    // the one mechanism the app has: the flag stops the save, and the kill tries the server.
    let id = request.query_id.as_deref().filter(|id| !id.is_empty());
    let run = match id {
        Some(id) => Some(
            access
                .queries()
                .register(id, &conn.id, &database, Engine::Mongo, 1)?,
        ),
        None => None,
    };
    let cancelled_run = run.as_ref().map(|r| r.cancelled.clone());
    let token = cancelled_run.as_ref();
    // `run::kill` matches `command.comment` against the run's id, so a save with an id sends
    // that id; without one it keeps the plain grid-edit marker.
    let comment = OpComment::new(id.unwrap_or(GRID_EDIT));
    let sql = &request.sql;
    let operations = &request.operations;
    // The first server contact (which collections exist, and their type) can wait long on a
    // server that does not answer, so the cancel stops it here.
    let prepared = async {
        let target = target(&client, &database, sql, comment)
            .await
            .map_err(AppError::from)?;
        let mut writes = Vec::with_capacity(operations.len());
        for (index, op) in operations.iter().enumerate() {
            match Write::prepare(op) {
                Ok(w) => writes.push(w),
                Err(Refusal::Message(message)) => return Ok::<_, AppError>(Err((index, message))),
                Err(Refusal::Invalid(error)) => return Err(error),
            }
        }
        Ok::<_, AppError>(Ok((target, writes)))
    };
    let prepared = tokio::select! {
        biased;
        () = cancelled(token) => {
            return Err(save_cancelled());
        }
        r = prepared => r,
    }?;
    let (target, writes) = match prepared {
        Ok(ready) => ready,
        Err((index, message)) => return Ok(failed(index, message, false)),
    };
    if writes.is_empty() {
        return Ok(SAVED);
    }
    let coll = client
        .database(&database)
        .collection::<Document>(&target.collection);
    let result = if target.transactions {
        in_transaction(&client, &coll, writes, token, comment).await
    } else {
        one_by_one(&coll, writes, token, comment).await
    };
    drop(run);
    result
}

/// The value at a grid column path. A key that holds a `.` itself is tried before the split.
fn lookup<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    if let Some(v) = doc.get(path) {
        return Some(v);
    }
    path.match_indices('.')
        .find_map(|(i, _)| match doc.get(&path[..i]) {
            Some(Bson::Document(d)) => lookup(d, &path[i + 1..]),
            _ => None,
        })
}

/// `edit::cell_value` for a MongoDB connection: a string as it is, any other value as mongosh
/// text, `None` for a missing field.
pub async fn cell_value(
    access: &impl Access,
    conn: &Connection,
    request: CellValueRequest,
) -> Result<Option<String>, AppError> {
    let database = target_database(conn, Some(&request.database));
    let client = client::client(access, &conn.id).await?;
    let comment = OpComment::new(GRID_EDIT);
    let collection = editable_collection(&client, &database, &request.sql)
        .await
        .map_err(AppError::from)?;
    let filter = id_filter(&request.pk).map_err(AppError::from)?;
    let mut o = FindOneOptions::default();
    o.comment = Some(comment.tag());
    o.collation = Some(simple_collation());
    let Some(document) = client
        .database(&database)
        .collection::<Document>(&collection)
        .find_one(filter)
        .with_options(o)
        .await?
    else {
        return Err(AppError::not_found(GONE));
    };
    match lookup(&document, &request.column) {
        None => Ok(None),
        Some(Bson::String(s)) => Ok(Some(s.clone())),
        Some(v) => value::literal(v).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    use mongodb::bson::oid::ObjectId;

    fn op(value: serde_json::Value) -> EditOperation {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn update_filters_on_the_old_values() {
        let w = Write::prepare(&op(json!({
            "key": "0", "kind": "update",
            "pk": { "_id": { "type": "objectId", "value": "65a1b2c3d4e5f60718293a4b" } },
            "set": {
                "a": { "type": "int64", "value": "9007199254740993" },
                "b.c": { "type": "null", "value": null },
                "d": { "type": "string", "value": "x" },
                "e": { "type": "shell", "value": "[1.0, NumberDecimal('2.50')]" },
            },
            "old": {
                "a": { "type": "int32", "value": "1" },
                "b.c": { "type": "null", "value": null },
                "d": { "type": "missing", "value": null },
                "e": { "type": "shell", "value": "[1, 2]" },
            },
        })))
        .unwrap();
        let id = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        let expected = Write::Update {
            filter: doc! {
                "_id": id,
                "a": { "$eq": 1, "$type": "int", "$not": { "$type": "array" } },
                "b.c": { "$type": "null", "$not": { "$type": "array" } },
                "d": { "$exists": false },
                "e": { "$eq": [1, 2], "$not": { "$elemMatch": { "$eq": [1, 2] } } },
            },
            update: doc! { "$set": {
                "a": 9_007_199_254_740_993_i64,
                "b.c": Bson::Null,
                "d": "x",
                "e": [1.0, Bson::Decimal128("2.50".parse().unwrap())],
            } },
        };
        assert_eq!(w, expected);
    }

    #[test]
    fn delete_and_insert_operations() {
        let w = Write::prepare(&op(json!({
            "key": "0", "kind": "delete",
            "pk": { "_id": { "type": "int32", "value": "7" } }, "set": {}
        })))
        .unwrap();
        assert_eq!(
            w,
            Write::Delete {
                filter: doc! { "_id": 7 }
            }
        );
        let w = Write::prepare(&op(json!({
            "key": "1", "kind": "insert", "pk": {}, "set": {},
            "document": "{ _id: 1, n: NumberLong(5) }"
        })))
        .unwrap();
        assert_eq!(
            w,
            Write::Insert {
                document: doc! { "_id": 1, "n": 5_i64 }
            }
        );
    }

    #[test]
    fn bad_operations() {
        // A value that does not convert is the failure of the operation, naming the path.
        let failure = |v: serde_json::Value| match Write::prepare(&op(v)) {
            Err(Refusal::Message(message)) => message,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            failure(json!({
                "key": "0", "kind": "update",
                "pk": { "_id": { "type": "int32", "value": "1" } },
                "set": { "a": { "type": "int32", "value": "x" } },
                "old": { "a": { "type": "int32", "value": "1" } },
            })),
            "a: Invalid int32 value: x"
        );
        assert_eq!(
            failure(json!({
                "key": "0", "kind": "update",
                "pk": { "_id": { "type": "int32", "value": "1" } },
                "set": { "a": { "type": "missing", "value": null } },
                "old": { "a": { "type": "int32", "value": "1" } },
            })),
            "a: a new value cannot be missing"
        );
        assert_eq!(
            failure(json!({
                "key": "0", "kind": "delete",
                "pk": { "_id": { "type": "regex", "value": "/a/" } }, "set": {}
            })),
            "_id: The grid cannot write BSON type regex"
        );
        assert!(
            failure(json!({
                "key": "0", "kind": "insert", "pk": {}, "set": {}, "document": "{a: "
            }))
            .starts_with("New document: Line 1")
        );
        assert_eq!(
            failure(json!({
                "key": "0", "kind": "insert", "pk": {}, "set": {}, "document": "[1]"
            })),
            "The new document must be a document: { … }"
        );
        // A malformed request is an error.
        let error = |v: serde_json::Value| {
            AppError::from(Write::prepare(&op(v)).unwrap_err()).message
        };
        assert_eq!(
            error(json!({
                "key": "0", "kind": "update",
                "pk": { "_id": { "type": "int32", "value": "1" } },
                "set": { "a": { "type": "int32", "value": "2" } },
            })),
            "The change of a has no old value"
        );
        assert_eq!(
            error(json!({
                "key": "0", "kind": "delete", "pk": { "id": { "type": "int32", "value": "1" } }, "set": {}
            })),
            "The request must contain exactly the _id value"
        );
        assert_eq!(
            error(json!({ "key": "0", "kind": "insert", "pk": {}, "set": {} })),
            "An insert needs a document"
        );
    }

    #[test]
    fn lookup_follows_column_paths() {
        let d = doc! { "a": { "b": { "c": 1 } }, "x.y": 2, "s": "t" };
        assert_eq!(lookup(&d, "a.b.c"), Some(&Bson::Int32(1)));
        assert_eq!(lookup(&d, "a.b"), Some(&Bson::Document(doc! { "c": 1 })));
        assert_eq!(lookup(&d, "x.y"), Some(&Bson::Int32(2)));
        assert_eq!(lookup(&d, "a.z"), None);
        assert_eq!(lookup(&d, "s.t"), None);
    }
}
