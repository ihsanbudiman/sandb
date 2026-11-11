//! Grid edits on MongoDB: which `find` results can be edited, the typed values of the cell editor
//! and the save of a batch of changes.
//!
//! Every value keeps its BSON type: the grid sends the type with the text
//! (`{ "type": "int64", "value": "42" }`) and `typed` builds exactly that type, so an Int64 or a
//! Decimal128 never comes back as a Double. An update filters on `_id` and on the old value of each
//! changed field, so a document that changed since it was loaded is never overwritten.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::TryStreamExt;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Document, doc};
use mongodb::options::{Collation, DeleteOptions, FindOneOptions, InsertOneOptions, UpdateOptions};
use mongodb::results::CollectionType;
use mongodb::{Client, ClientSession, Collection};
use serde::Deserialize;
use uuid::Uuid;

use crate::core::connections::{self, Connection, Topology};
use crate::core::db::edit::{
    ApplyEditsRequest, ApplyEditsResult, CellValue, CellValueRequest, EditFailure, EditKind,
    EditOperation, EditableRequest, EditableResult,
};
use crate::core::db::query::Session;
use crate::core::error::{AppError, ErrorCode};
use crate::core::mongo::command::{Command, Method};
use crate::core::mongo::conn::topology_of;
use crate::core::mongo::schema::type_name;
use crate::core::mongo::{client, parse, shell};
use crate::core::state::AppState;

/// The `comment` of every server operation of a grid edit.
const COMMENT: &str = "sandb grid edit";
const NOT_FIND: &str = "Only the result of one find() on a collection can be edited";
const COMPUTED: &str = "The projection computes or hides fields";
const CONFLICT: &str =
    "The document changed or no longer exists since it was loaded. Nothing was overwritten.";
const GONE: &str = "The document no longer exists";

// ---- typed values

/// The BSON value of one typed grid value. The type decides, not the text: `typed("double", "42")`
/// is the Double 42.0. `null` ignores `text`; `shell` is one mongosh value literal.
pub fn typed(ty: &str, text: &str) -> Result<Bson, AppError> {
    let bad = || AppError::invalid(format!("Invalid {ty} value: {text}"));
    let t = text.trim();
    Ok(match ty {
        "string" => Bson::String(text.to_string()),
        "int32" => Bson::Int32(t.parse().map_err(|_| bad())?),
        "int64" => Bson::Int64(t.parse().map_err(|_| bad())?),
        "double" => Bson::Double(double(t).ok_or_else(bad)?),
        "decimal128" => Bson::Decimal128(t.parse().map_err(|_| bad())?),
        "bool" => match t {
            "true" => Bson::Boolean(true),
            "false" => Bson::Boolean(false),
            _ => return Err(bad()),
        },
        "date" => Bson::DateTime(date(t).ok_or_else(bad)?),
        "objectId" => Bson::ObjectId(ObjectId::parse_str(t).map_err(|_| bad())?),
        "uuid" => Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: Uuid::parse_str(t).map_err(|_| bad())?.as_bytes().to_vec(),
        }),
        "null" => Bson::Null,
        "shell" => parse::parse_value(text)?,
        _ => return Err(cannot_write(ty)),
    })
}

fn cannot_write(ty: &str) -> AppError {
    AppError::invalid(format!("The grid cannot write BSON type {ty}"))
}

/// The JavaScript number texts; Rust alone would also read `inf` and `nan`.
fn double(t: &str) -> Option<f64> {
    match t {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ if t
            .bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) =>
        {
            t.parse().ok()
        }
        _ => None,
    }
}

/// RFC 3339, or the milliseconds the grid shows for a date outside the years 0 to 9999.
fn date(t: &str) -> Option<DateTime> {
    match t.parse::<i64>() {
        Ok(ms) => Some(DateTime::from_millis(ms)),
        Err(_) => DateTime::parse_rfc3339_str(t).ok(),
    }
}

/// The text of a date in the grid: RFC 3339, or the milliseconds outside the years 0 to 9999.
fn date_text(d: &DateTime) -> String {
    d.try_to_rfc3339_string()
        .unwrap_or_else(|_| d.timestamp_millis().to_string())
}

/// A double as JavaScript's `String(n)` writes it (`42`, `1.5`, `1e+21`, `1e-7`), except that
/// -0 keeps its sign.
fn js_number(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let a = d.abs();
    if a != 0.0 && !(1e-6..1e21).contains(&a) {
        let s = format!("{d:e}");
        return match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        };
    }
    format!("{d}")
}

/// The text of a scalar value in the inline cell editor: the same text `flatten::cell` puts in
/// the grid. `typed(type_name(v), &edit_text(v)?)` gives `v` back.
pub fn edit_text(v: &Bson) -> Result<String, AppError> {
    Ok(match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => js_number(*d),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::DateTime(d) => date_text(d),
        Bson::ObjectId(id) => id.to_hex(),
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            // The length check makes `from_slice` infallible.
            Uuid::from_slice(&b.bytes).expect("16 bytes").to_string()
        }
        Bson::Null => "null".into(),
        _ => return Err(cannot_write(type_name(v))),
    })
}

/// One typed value of the wire
#[derive(Deserialize)]
struct Typed {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    value: Option<String>,
}

/// The BSON value of a wire value; `None` for `missing`.
fn from_wire(v: &CellValue) -> Result<Option<Bson>, AppError> {
    let t =
        Typed::deserialize(v).map_err(|_| AppError::invalid("A value needs a type and a text"))?;
    match (t.ty.as_str(), t.value.as_deref()) {
        ("missing", _) => Ok(None),
        ("null", _) => Ok(Some(Bson::Null)),
        (ty, Some(text)) => typed(ty, text).map(Some),
        (ty, None) => Err(AppError::invalid(format!("A {ty} value needs a text"))),
    }
}

/// The value at `path`, or the failure message that names the path.
fn value_at(path: &str, v: &CellValue) -> Result<Option<Bson>, String> {
    from_wire(v).map_err(|e| format!("{path}: {}", e.message))
}

// ---- editable results

/// The collection and the server of an editable result.
struct Target {
    collection: String,
    transactions: bool,
}

/// The collection of a result from its statement text, or the reason it cannot be edited.
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

/// The text checks, then the server checks: the collection exists and is not a view, and the
/// server kind. `Ok(Err(reason))` is a result that cannot be edited.
async fn target(
    client: &Client,
    database: &str,
    sql: &str,
    comment: &str,
) -> Result<Result<Target, String>, AppError> {
    if database.is_empty() {
        return Ok(Err("Choose a database for this tab first".into()));
    }
    let collection = match find_collection(sql) {
        Ok(c) => c,
        Err(reason) => return Ok(Err(reason.into())),
    };
    let spec = client
        .database(database)
        .list_collections()
        .filter(doc! { "name": &collection })
        .authorized_collections(true)
        .comment(comment)
        .await?
        .try_next()
        .await?;
    match spec {
        None => return Ok(Err("The collection does not exist".into())),
        Some(s) if matches!(s.collection_type, CollectionType::View) => {
            return Ok(Err("Views are read-only".into()));
        }
        Some(_) => {}
    }
    let hello = client
        .database("admin")
        .run_command(doc! { "hello": 1, "comment": comment })
        .await?;
    Ok(Ok(Target {
        collection,
        transactions: topology_of(&hello) != Topology::Standalone,
    }))
}

/// `edit::editable_result` for a MongoDB connection.
pub async fn editable_result(
    state: &AppState,
    conn: &Connection,
    request: EditableRequest,
) -> Result<EditableResult, AppError> {
    if conn.read_only {
        return Ok(EditableResult::not_editable("The connection is read-only"));
    }
    let database = connections::target_database(conn, Some(&request.database));
    let client = client::client(state, &conn.id).await?;
    Ok(
        match target(&client, &database, &request.sql, COMMENT).await? {
            Err(reason) => EditableResult::not_editable(reason),
            Ok(t) => EditableResult {
                table: Some(t.collection),
                schema: None,
                pk: vec!["_id".into()],
                reason: None,
                transactions: Some(t.transactions),
            },
        },
    )
}

// ---- saves

/// One operation of a save, ready to run.
#[derive(Debug, PartialEq)]
enum Write {
    Update { filter: Document, update: Document },
    Delete { filter: Document },
    Insert { document: Document },
}

/// `{ _id: <value> }` from the `pk` of an operation.
fn id_filter(pk: &BTreeMap<String, CellValue>) -> Result<Result<Document, String>, AppError> {
    let (Some(id), 1) = (pk.get("_id"), pk.len()) else {
        return Err(AppError::invalid(
            "The request must contain exactly the _id value",
        ));
    };
    Ok(match value_at("_id", id) {
        Ok(Some(id)) => Ok(doc! { "_id": id }),
        Ok(None) => Err("_id: a document always has an _id".into()),
        Err(message) => Err(message),
    })
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

/// The server's `$type` alias of a value.
fn type_alias(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// Binary comparison, so a case-insensitive default collation of the collection does not loosen
/// the match on `_id` or on the old values.
fn simple_collation() -> Collation {
    Collation::builder().locale("simple").build()
}

/// Builds one operation. A malformed request is an error; a value that does not convert is the
/// failure message of the operation.
fn write_op(op: &EditOperation) -> Result<Result<Write, String>, AppError> {
    match op.kind {
        EditKind::Insert => {
            let Some(text) = &op.document else {
                return Err(AppError::invalid("An insert needs a document"));
            };
            Ok(match parse::parse_value(text) {
                Ok(Bson::Document(document)) => Ok(Write::Insert { document }),
                Ok(_) => Err("The new document must be a document: { … }".into()),
                Err(e) => Err(format!("New document: {e}")),
            })
        }
        EditKind::Delete => Ok(id_filter(&op.pk)?.map(|filter| Write::Delete { filter })),
        EditKind::Update => {
            if op.set.is_empty() {
                return Err(AppError::invalid("An update needs at least one field"));
            }
            if let Some(path) = op.set.keys().find(|p| !op.old.contains_key(*p)) {
                return Err(AppError::invalid(format!(
                    "The change of {path} has no old value"
                )));
            }
            let mut filter = match id_filter(&op.pk)? {
                Ok(f) => f,
                Err(message) => return Ok(Err(message)),
            };
            let mut set = Document::new();
            for (path, new) in &op.set {
                let new = match value_at(path, new) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(Err(format!("{path}: a new value cannot be missing"))),
                    Err(message) => return Ok(Err(message)),
                };
                let old = match value_at(path, &op.old[path]) {
                    Ok(v) => v,
                    Err(message) => return Ok(Err(message)),
                };
                filter.insert(path.clone(), old_condition(old));
                set.insert(path.clone(), new);
            }
            Ok(Ok(Write::Update {
                filter,
                update: doc! { "$set": set },
            }))
        }
    }
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

/// Runs one operation, in `session` when given. `Some(message)` when it failed or changed nothing.
async fn run_op(
    coll: &Collection<Document>,
    write: Write,
    session: Option<&mut ClientSession>,
    comment: &str,
) -> Option<String> {
    let r = match write {
        Write::Update { filter, update } => {
            let mut o = UpdateOptions::default();
            o.comment = Some(Bson::String(comment.to_string()));
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
            o.comment = Some(Bson::String(comment.to_string()));
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
            o.comment = Some(Bson::String(comment.to_string()));
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

/// How long a cancelled save waits for the reply of `commitTransaction`, so it can say whether
/// the change applied. The same window `run.rs` gives a cancelled write.
const COMMIT_REPLY_WAIT: Duration = Duration::from_secs(2);

/// Wakes when the save's `query_id` is cancelled. Without an id it never wakes, and the selects
/// below take the normal arm.
async fn until_cancelled(cancelled: Option<&AtomicBool>) {
    let Some(cancelled) = cancelled else {
        core::future::pending::<()>().await;
        return;
    };
    while !cancelled.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|c| c.load(Ordering::SeqCst))
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
    cancelled: Option<&AtomicBool>,
    comment: &str,
) -> Result<ApplyEditsResult, AppError> {
    let mut session = client.start_session().await?;
    session.start_transaction().await?;
    for (index, write) in writes.into_iter().enumerate() {
        if is_cancelled(cancelled) {
            abort(&mut session).await;
            return Err(save_cancelled());
        }
        // The operation races the cancel; the block ends first, so the session is free again.
        let finished = {
            let op = run_op(coll, write, Some(&mut session), comment);
            tokio::pin!(op);
            tokio::select! {
                r = &mut op => Some(r),
                () = until_cancelled(cancelled) => None,
            }
        };
        match finished {
            None => {
                // The transaction is open: abort it, so nothing stays saved.
                abort(&mut session).await;
                return Err(save_cancelled());
            }
            Some(Some(message)) => {
                // The failure is the result, unless the cancel caused it: `query::cancel` sets
                // the flag before it kills the operation, so the kill's error is no surprise.
                abort(&mut session).await;
                if is_cancelled(cancelled) {
                    return Err(save_cancelled());
                }
                return Ok(failed(index, message, false));
            }
            Some(None) => {}
        }
    }
    if is_cancelled(cancelled) {
        abort(&mut session).await;
        return Err(save_cancelled());
    }
    // A commit already sent may still apply: give its reply the same window as a write.
    // The action is not a `Future` itself; the block awaits it and can still be raced.
    let commit = async { session.commit_transaction().await };
    tokio::pin!(commit);
    let committed = tokio::select! {
        r = &mut commit => r,
        () = until_cancelled(cancelled) => {
            match tokio::time::timeout(COMMIT_REPLY_WAIT, &mut commit).await {
                Ok(r) => r,
                Err(_) => return Err(save_maybe_applied()),
            }
        }
    };
    committed?;
    Ok(SAVED)
}

/// Aborts the transaction, best effort: the server may be the reason for the cancel.
async fn abort(session: &mut ClientSession) {
    let _ = tokio::time::timeout(COMMIT_REPLY_WAIT, session.abort_transaction()).await;
}

/// Standalone server, no transactions: the operations run one by one and stop at the first
/// failure; the ones before it stay saved. A cancel stops the next operation and says how many
/// changes were already saved.
async fn one_by_one(
    coll: &Collection<Document>,
    writes: Vec<Write>,
    cancelled: Option<&AtomicBool>,
    comment: &str,
) -> Result<ApplyEditsResult, AppError> {
    for (index, write) in writes.into_iter().enumerate() {
        if is_cancelled(cancelled) {
            return Err(saved_before_cancel(index));
        }
        let finished = {
            let op = run_op(coll, write, None, comment);
            tokio::pin!(op);
            tokio::select! {
                r = &mut op => Some(r),
                () = until_cancelled(cancelled) => None,
            }
        };
        match finished {
            None => return Err(saved_before_cancel(index)),
            Some(Some(message)) => {
                if is_cancelled(cancelled) {
                    return Err(saved_before_cancel(index));
                }
                return Ok(failed(index, message, index > 0));
            }
            Some(None) => {}
        }
    }
    Ok(SAVED)
}

/// The cancel error on a server without transactions, naming what stays saved.
fn saved_before_cancel(saved: usize) -> AppError {
    let detail = match saved {
        0 => "No change was saved before the cancel.".to_string(),
        1 => "1 change was saved before the cancel.".to_string(),
        n => format!("{n} changes were saved before the cancel."),
    };
    save_cancelled().with_detail(format!("{detail} Reload to check."))
}

/// `edit::apply_edits` for a MongoDB connection; the caller has refused a read-only connection.
/// Every operation is built before any runs, so a value that does not convert saves nothing.
pub async fn apply_edits(
    state: &AppState,
    conn: &Connection,
    request: ApplyEditsRequest,
) -> Result<ApplyEditsResult, AppError> {
    let database = connections::target_database(conn, Some(&request.database));
    let client = client::client(state, &conn.id).await?;
    // The dialog's id puts the save in the editor's registry, so `cancel_query` reaches it with
    // the one mechanism the app has: the flag stops the save, and the kill tries the server.
    let id = request.query_id.as_deref().filter(|id| !id.is_empty());
    let run = match id {
        Some(id) => Some(state.queries.register(id, &conn.id, &database, 1)?),
        None => None,
    };
    // Held for the save, so a cancel also names the open transaction on the server.
    let attached = id.map(|id| state.queries.attach(id, Session::Mongo));
    let cancelled = run.as_ref().map(|r| r.cancelled.clone());
    let flag = cancelled.as_deref();
    // `run::kill` matches `command.comment` against the run's id, so a save with an id sends
    // that id; without one it keeps the plain grid-edit marker.
    let comment = id.unwrap_or(COMMENT);
    let sql = &request.sql;
    let operations = &request.operations;
    // The first server contact (which collections exist, and their type) can wait long on a
    // server that does not answer, so the cancel stops it here.
    let prepared = async {
        let target = target(&client, &database, sql, comment)
            .await?
            .map_err(AppError::invalid)?;
        let mut writes = Vec::with_capacity(operations.len());
        for (index, op) in operations.iter().enumerate() {
            match write_op(op)? {
                Ok(w) => writes.push(w),
                Err(message) => return Ok::<_, AppError>(Err((index, message))),
            }
        }
        Ok::<_, AppError>(Ok((target, writes)))
    };
    let prepared = tokio::select! {
        r = prepared => r,
        () = until_cancelled(flag) => {
            return Err(save_cancelled());
        }
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
        in_transaction(&client, &coll, writes, flag, comment).await
    } else {
        one_by_one(&coll, writes, flag, comment).await
    };
    drop(attached);
    drop(run);
    result
}

// ---- cell values

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

/// `edit::cell_value` for a MongoDB connection: a string as it is, any other value as `shell`
/// text, `None` for a missing field.
pub async fn cell_value(
    state: &AppState,
    conn: &Connection,
    request: CellValueRequest,
) -> Result<Option<String>, AppError> {
    let database = connections::target_database(conn, Some(&request.database));
    let client = client::client(state, &conn.id).await?;
    let target = target(&client, &database, &request.sql, COMMENT)
        .await?
        .map_err(AppError::invalid)?;
    let filter = id_filter(&request.pk)?.map_err(AppError::invalid)?;
    let mut o = FindOneOptions::default();
    o.comment = Some(COMMENT.into());
    o.collation = Some(simple_collation());
    let Some(document) = client
        .database(&database)
        .collection::<Document>(&target.collection)
        .find_one(filter)
        .with_options(o)
        .await?
    else {
        return Err(AppError::not_found(GONE));
    };
    match lookup(&document, &request.column) {
        None => Ok(None),
        Some(Bson::String(s)) => Ok(Some(s.clone())),
        Some(v) => shell::literal(v).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{Decimal128, JavaScriptCodeWithScope, Regex, Timestamp};
    use serde_json::json;

    use super::*;

    /// The BSON bytes of a value: equal bytes are the same type and the same bits (NaN too).
    fn bits(v: &Bson) -> Vec<u8> {
        let mut out = Vec::new();
        doc! { "v": v.clone() }.to_writer(&mut out).unwrap();
        out
    }

    fn dec(s: &str) -> Bson {
        Bson::Decimal128(s.parse::<Decimal128>().unwrap())
    }

    fn uuid_value() -> Bson {
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e")
                .unwrap()
                .as_bytes()
                .to_vec(),
        })
    }

    /// Every scalar value the inline editor can edit, with its edge cases.
    fn scalars() -> Vec<Bson> {
        let mut v = vec![
            Bson::String(String::new()),
            Bson::String("héllo \"q\" \\ \n\t…".into()),
            Bson::String("x".repeat(10_000)),
            Bson::Int32(0),
            Bson::Int32(-1),
            Bson::Int32(i32::MIN),
            Bson::Int32(i32::MAX),
            Bson::Int64(0),
            Bson::Int64(i64::MIN),
            Bson::Int64(i64::MAX),
            Bson::Int64((1 << 53) + 1),
            Bson::Boolean(true),
            Bson::Boolean(false),
            Bson::ObjectId(ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap()),
            uuid_value(),
            Bson::Null,
        ];
        for d in [
            0.0,
            -0.0,
            1.5,
            -1.5,
            42.0,
            -42.0,
            0.1,
            1e21,
            -1e21,
            1e20,
            1e16,
            1e-6,
            1e-7,
            123_456_789.123_456_79,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            5e-324,
            9_007_199_254_740_993.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            v.push(Bson::Double(d));
        }
        for s in [
            "0",
            "1.50",
            "0.00",
            "-0",
            "-0.0001",
            "12345.6789",
            "1E+3",
            "1.000000000000000000000000000000000",
            "9999999999999999999999999999999999",
            "NaN",
            "Infinity",
            "-Infinity",
        ] {
            v.push(dec(s));
        }
        for ms in [
            0,
            -1,
            1_700_000_000_123,
            253_402_300_799_999, // 9999-12-31T23:59:59.999Z
            253_402_300_800_000, // year 10000: milliseconds
            -62_167_219_200_000, // 0000-01-01
            -62_167_219_200_001, // before year 0: milliseconds
            i64::MAX,
            i64::MIN,
        ] {
            v.push(Bson::DateTime(DateTime::from_millis(ms)));
        }
        v
    }

    #[test]
    fn scalars_round_trip_through_edit_text() {
        for v in scalars() {
            let text = edit_text(&v).unwrap();
            let back = typed(type_name(&v), &text).unwrap();
            assert_eq!(bits(&back), bits(&v), "{v:?} → {text:?} → {back:?}");
        }
    }

    #[test]
    fn edit_text_matches_the_grid_cell() {
        assert_eq!(edit_text(&Bson::Double(42.0)).unwrap(), "42");
        assert_eq!(edit_text(&Bson::Double(1e21)).unwrap(), "1e+21");
        assert_eq!(edit_text(&Bson::Double(1.5e-7)).unwrap(), "1.5e-7");
        assert_eq!(
            edit_text(&Bson::Double(1e20)).unwrap(),
            "100000000000000000000"
        );
        assert_eq!(edit_text(&Bson::Double(f64::NAN)).unwrap(), "NaN");
        assert_eq!(edit_text(&Bson::Double(-0.0)).unwrap(), "-0");
        assert_eq!(edit_text(&dec("1.50")).unwrap(), "1.50");
        assert_eq!(
            edit_text(&uuid_value()).unwrap(),
            "0f8fad5b-d9cb-469f-a165-70867728950e"
        );
        assert_eq!(
            edit_text(&Bson::DateTime(DateTime::from_millis(1_700_000_000_123))).unwrap(),
            "2023-11-14T22:13:20.123Z"
        );
        // The texts of `flatten::cell` for the same values.
        for v in [
            Bson::Int32(7),
            Bson::Int64(i64::MAX),
            Bson::Double(1.5),
            dec("1.50"),
            Bson::DateTime(DateTime::from_millis(1_700_000_000_123)),
            Bson::DateTime(DateTime::from_millis(253_402_300_800_000)),
            Bson::ObjectId(ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap()),
            uuid_value(),
        ] {
            let cell = crate::core::mongo::flatten::cell(&v);
            let cell = cell
                .as_str()
                .map_or_else(|| cell.to_string(), str::to_string);
            assert_eq!(edit_text(&v).unwrap(), cell, "{v:?}");
        }
    }

    #[test]
    fn the_type_decides_not_the_text() {
        assert_eq!(
            bits(&typed("double", "42").unwrap()),
            bits(&Bson::Double(42.0))
        );
        assert_eq!(bits(&typed("int64", "42").unwrap()), bits(&Bson::Int64(42)));
        assert_eq!(bits(&typed("decimal128", "42").unwrap()), bits(&dec("42")));
        assert_eq!(
            typed("string", " 42 ").unwrap(),
            Bson::String(" 42 ".into())
        );
        assert_eq!(typed("null", "whatever").unwrap(), Bson::Null);
        assert_eq!(
            bits(&typed("shell", "{a: 42.0}").unwrap()),
            bits(&Bson::Document(doc! { "a": 42.0 }))
        );
    }

    #[test]
    fn unknown_type_names_are_refused() {
        for ty in [
            "int", "object", "array", "missing", "regex", "binData", "Double", "",
        ] {
            let e = typed(ty, "1").unwrap_err();
            assert_eq!(e.message, format!("The grid cannot write BSON type {ty}"));
        }
    }

    #[test]
    fn invalid_texts_are_refused() {
        for (ty, text) in [
            ("int32", "2147483648"),
            ("int32", "1.5"),
            ("int32", ""),
            ("int64", "9223372036854775808"),
            ("int64", "abc"),
            ("double", "inf"),
            ("double", "nan"),
            ("double", "1,5"),
            ("double", ""),
            ("decimal128", "x"),
            ("bool", "yes"),
            ("bool", "True"),
            ("date", "2024-13-01T00:00:00Z"),
            ("date", "yesterday"),
            ("objectId", "123"),
            ("uuid", "xyz"),
            ("shell", "{a:"),
            ("shell", "1 2"),
            ("shell", ""),
            ("shell", "function () {}"),
        ] {
            assert!(typed(ty, text).is_err(), "{ty} {text:?}");
        }
        assert_eq!(
            typed("int32", "x").unwrap_err().message,
            "Invalid int32 value: x"
        );
    }

    #[test]
    fn edit_text_refuses_non_scalars() {
        for v in [
            Bson::Document(doc! { "a": 1 }),
            Bson::Array(vec![]),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![1],
            }),
            Bson::MinKey,
        ] {
            assert!(edit_text(&v).is_err(), "{v:?}");
        }
    }

    /// Every type the printer writes, nested in documents and arrays.
    fn printable() -> Document {
        let mut scalars: Vec<Bson> = scalars()
            .into_iter()
            .filter(|v| !matches!(v, Bson::DateTime(d) if d.timestamp_millis().unsigned_abs() > (1 << 53)))
            .collect();
        scalars.extend([
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![0, 1, 2, 255],
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::UuidOld,
                bytes: vec![7; 16],
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: vec![1, 2, 3],
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::UserDefined(0x80),
                bytes: vec![],
            }),
            Bson::Timestamp(Timestamp {
                time: u32::MAX,
                increment: 7,
            }),
            Bson::Timestamp(Timestamp {
                time: 0,
                increment: 0,
            }),
            Bson::RegularExpression(Regex {
                pattern: "^a[/]b\\/c$".into(),
                options: "imsx".into(),
            }),
            Bson::RegularExpression(Regex {
                pattern: "x".into(),
                options: String::new(),
            }),
            Bson::MinKey,
            Bson::MaxKey,
            Bson::Document(Document::new()),
            Bson::Array(vec![]),
        ]);
        let mut inner = Document::new();
        for (i, v) in scalars.iter().enumerate() {
            inner.insert(format!("k{i}"), v.clone());
        }
        let long = Bson::Int64(3);
        doc! {
            "_id": ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap(),
            "": "empty key",
            "with \"quote\" and.dot": 1,
            "naïve 😀": "ü",
            "whole": [1.0, 2.0, -0.0, 3, long, [[4.0], {"x": 5.0}]],
            "items": Bson::Array(scalars),
            "nested": { "deeper": { "deepest": inner } },
        }
    }

    #[test]
    fn literal_round_trips() {
        let d = Bson::Document(printable());
        let text = shell::literal(&d).unwrap();
        let back = parse::parse_value(&text).unwrap();
        assert_eq!(bits(&back), bits(&d), "{text}");
        for v in scalars() {
            let Ok(text) = shell::literal(&v) else {
                continue;
            };
            assert!(!text.contains('\n'), "{text}");
            let back = parse::parse_value(&text).unwrap();
            assert_eq!(bits(&back), bits(&v), "{text}");
        }
    }

    #[test]
    fn literal_shapes() {
        let d = Bson::Document(doc! { "a": [1, 2.0], "b": {}, "c": [] });
        assert_eq!(
            shell::literal(&d).unwrap(),
            "{\n  \"a\": [\n    1,\n    Double(2.0)\n  ],\n  \"b\": {},\n  \"c\": []\n}"
        );
        // An integral double keeps its constructor: a bare `42` would read back as an Int32.
        assert_eq!(shell::literal(&Bson::Double(42.0)).unwrap(), "Double(42.0)");
        assert_eq!(shell::literal(&Bson::Double(1e21)).unwrap(), "Double(1e21)");
        assert_eq!(shell::literal(&Bson::Double(-0.0)).unwrap(), "Double(-0.0)");
        assert_eq!(shell::literal(&Bson::Double(1.5)).unwrap(), "1.5");
        assert_eq!(
            shell::literal(&Bson::Int64(5)).unwrap(),
            "NumberLong(\"5\")"
        );
        assert_eq!(
            shell::literal(&dec("1.50")).unwrap(),
            "NumberDecimal(\"1.50\")"
        );
        assert_eq!(
            shell::literal(&Bson::DateTime(DateTime::from_millis(0))).unwrap(),
            "ISODate(\"1970-01-01T00:00:00Z\")"
        );
        assert_eq!(
            shell::literal(&Bson::DateTime(DateTime::from_millis(253_402_300_800_000))).unwrap(),
            "new Date(253402300800000)"
        );
        assert_eq!(
            shell::literal(&uuid_value()).unwrap(),
            "UUID(\"0f8fad5b-d9cb-469f-a165-70867728950e\")"
        );
        assert_eq!(
            shell::literal(&Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![1, 2, 3],
            }))
            .unwrap(),
            "BinData(0, \"AQID\")"
        );
        assert_eq!(shell::literal(&Bson::MinKey).unwrap(), "MinKey()");
        assert_eq!(
            shell::literal(&Bson::String("a\"b".into())).unwrap(),
            "\"a\\\"b\""
        );
    }

    #[test]
    fn unprintable_values_are_refused() {
        let pointer = Bson::try_from(json!({
            "$dbPointer": { "$ref": "c", "$id": { "$oid": "65a1b2c3d4e5f60718293a4b" } }
        }))
        .unwrap();
        assert!(matches!(pointer, Bson::DbPointer(_)));
        for (v, ty) in [
            (Bson::Undefined, "undefined"),
            (Bson::Symbol("s".into()), "symbol"),
            (
                Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                    code: "f()".into(),
                    scope: doc! {},
                }),
                "javascriptWithScope",
            ),
            (pointer, "dbPointer"),
        ] {
            let nested = Bson::Document(doc! { "a": [ { "b": v.clone() } ] });
            let message = format!("Cannot write BSON type {ty}: mongosh has no constructor for it");
            for value in [v, nested] {
                assert_eq!(shell::literal(&value).unwrap_err().message, message);
            }
        }
        assert!(shell::literal(&Bson::DateTime(DateTime::from_millis(i64::MAX))).is_err());
    }

    #[test]
    fn parse_value_reads_one_value_only() {
        assert_eq!(parse::parse_value(" 42 ").unwrap(), Bson::Int32(42));
        assert_eq!(
            parse::parse_value("/a/i").unwrap(),
            Bson::RegularExpression(Regex {
                pattern: "a".into(),
                options: "i".into(),
            })
        );
        let e = parse::parse_value("{a: 1} x").unwrap_err();
        assert_eq!(e.message, "Expected the end of the value, found 'x'");
        assert!(parse::parse_value("1; 2").is_err());
    }

    #[test]
    fn only_a_plain_find_is_editable() {
        assert_eq!(find_collection("db.orders.find()"), Ok("orders".into()));
        assert_eq!(
            find_collection("db.getCollection('a b').find({x: 1}).sort({x: -1}).limit(5)"),
            Ok("a b".into())
        );
        assert_eq!(
            find_collection("db.orders.find({}, {status: 1, total: true, note: 0})"),
            Ok("orders".into())
        );
        assert_eq!(
            find_collection("db.orders.find().project({_id: 1, a: 1.0, b: NumberLong(0)})"),
            Ok("orders".into())
        );
        for sql in [
            "db.orders.aggregate([])",
            "db.orders.find().count()",
            "db.orders.countDocuments()",
            "db.orders.findOne()",
            "db.orders.distinct('a')",
            "db.orders.find(); db.orders.find()",
            "show collections",
            "db.orders.find(",
            "",
        ] {
            assert_eq!(find_collection(sql), Err(NOT_FIND), "{sql}");
        }
        for sql in [
            "db.orders.find({}, {total: {$multiply: ['$a', 2]}})",
            "db.orders.find({}, {a: 2})",
            "db.orders.find({}, {a: 'x'})",
            "db.orders.find({}, {'items.$': 1})",
            "db.orders.find({}, {items: {$slice: 1}})",
        ] {
            assert_eq!(find_collection(sql), Err(COMPUTED), "{sql}");
        }
        for sql in [
            "db.orders.find({}, {_id: 0})",
            "db.orders.find({}, {_id: false, a: 1})",
            "db.orders.find().project({_id: 0.0})",
        ] {
            assert_eq!(
                find_collection(sql),
                Err("The projection hides _id"),
                "{sql}"
            );
        }
        assert_eq!(
            find_collection("db.system.profile.find()"),
            Err("System collections are read-only")
        );
    }

    fn op(value: serde_json::Value) -> EditOperation {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn update_filters_on_the_old_values() {
        let w = write_op(&op(json!({
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
        .unwrap()
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
        let w = write_op(&op(json!({
            "key": "0", "kind": "delete",
            "pk": { "_id": { "type": "int32", "value": "7" } }, "set": {}
        })))
        .unwrap()
        .unwrap();
        assert_eq!(
            w,
            Write::Delete {
                filter: doc! { "_id": 7 }
            }
        );
        let w = write_op(&op(json!({
            "key": "1", "kind": "insert", "pk": {}, "set": {},
            "document": "{ _id: 1, n: NumberLong(5) }"
        })))
        .unwrap()
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
        let failure = |v: serde_json::Value| write_op(&op(v)).unwrap().unwrap_err();
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
        let error = |v: serde_json::Value| write_op(&op(v)).unwrap_err().message;
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
