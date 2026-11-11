//! Execution of one parsed write statement.

use std::time::Duration;

use mongodb::{
    Client, Collection, IndexModel,
    bson::{Bson, Document, doc},
    options::{
        Collation, CreateIndexOptions, DeleteOptions, DropIndexOptions, FindOneAndDeleteOptions,
        FindOneAndReplaceOptions, FindOneAndUpdateOptions, Hint, IndexOptions, InsertManyOptions,
        InsertOneOptions, ReplaceOptions, ReturnDocument, UpdateModifications, UpdateOptions,
    },
};

use crate::core::error::AppError;
use crate::core::mongo::command::{Command, Method};

/// The result of one write statement: the server's own counts plus one notice line.
#[derive(Debug)]
pub struct WriteOutcome {
    /// Inserted, modified, deleted or upserted count; 0 for schema changes.
    pub affected: u64,
    /// One line for `QueryRun.notices`, e.g. "updateMany on orders: matched 3, modified 2".
    pub notice: String,
    /// findOneAndUpdate / findOneAndReplace / findOneAndDelete: the returned document, if any.
    pub document: Option<Document>,
}

/// Runs one statement whose `guard::kind` is Write or Schema. Every operation carries `comment`.
pub async fn execute(
    client: &Client,
    database: &str,
    cmd: &Command,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    match cmd {
        Command::Collection { collection, method } => {
            execute_method(client, database, collection, method, comment).await
        }
        Command::CreateCollection { name, options } => {
            create_collection(client, database, name, options.as_ref(), comment).await
        }
        Command::DropDatabase { options } => {
            drop_database(client, database, options.as_ref(), comment).await
        }
        Command::ShowDbs
        | Command::ShowCollections
        | Command::Use(_)
        | Command::RunCommand { .. } => Err(AppError::internal("not a write")),
    }
}

/// `create` takes many options and the server validates them, so the user's document goes through
/// as it is. A user `comment` replaces the query id.
async fn create_collection(
    client: &Client,
    database: &str,
    name: &str,
    options: Option<&Document>,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    let mut command = doc! { "create": name, "comment": comment };
    if let Some(options) = options {
        for (key, value) in options {
            command.insert(key.clone(), value.clone());
        }
    }
    client.database(database).run_command(command).await?;
    Ok(WriteOutcome {
        affected: 0,
        notice: format!("createCollection: created {name}"),
        document: None,
    })
}

/// `db.dropDatabase()`: the server command, so the query id goes along as `comment`. mongosh
/// takes only `writeConcern`; the server validates its value.
async fn drop_database(
    client: &Client,
    database: &str,
    options: Option<&Document>,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    check_keys("dropDatabase", options, &["writeConcern"])?;
    let mut command = doc! { "dropDatabase": 1, "comment": comment };
    if let Some(options) = options {
        for (key, value) in options {
            command.insert(key.clone(), value.clone());
        }
    }
    client.database(database).run_command(command).await?;
    Ok(WriteOutcome {
        affected: 0,
        notice: format!("dropDatabase: dropped {database}"),
        document: None,
    })
}

/// The option keys each write method knows; anything else is refused before the run.
fn known_options(method: &Method) -> &'static [&'static str] {
    match method {
        Method::InsertMany { .. } | Method::BulkWrite { .. } => &["ordered"],
        Method::UpdateOne { .. } | Method::UpdateMany { .. } => {
            &["upsert", "arrayFilters", "collation", "hint", "let"]
        }
        Method::ReplaceOne { .. } => &["upsert", "collation", "hint", "let"],
        Method::FindOneAndUpdate { .. } => &[
            "upsert",
            "returnDocument",
            "projection",
            "sort",
            "arrayFilters",
            "collation",
            "hint",
            "let",
        ],
        Method::FindOneAndReplace { .. } => &[
            "upsert",
            "returnDocument",
            "projection",
            "sort",
            "collation",
            "hint",
            "let",
        ],
        Method::FindOneAndDelete { .. } => &["projection", "sort", "collation", "hint", "let"],
        Method::CreateIndex { .. } | Method::CreateIndexes { .. } => &[
            "name",
            "unique",
            "sparse",
            "expireAfterSeconds",
            "partialFilterExpression",
            "collation",
            "hidden",
            "weights",
            "default_language",
            "wildcardProjection",
            "language_override",
            "textIndexVersion",
            "2dsphereIndexVersion",
            "bits",
            "min",
            "max",
            "storageEngine",
            "background",
        ],
        _ => &[],
    }
}

/// Refuses keys `method` does not know, before anything runs.
fn check_options(method: &Method, options: Option<&Document>) -> Result<(), AppError> {
    check_keys(method.name(), options, known_options(method))
}

/// Refuses keys the named operation does not know, before anything runs.
fn check_keys(method: &str, options: Option<&Document>, known: &[&str]) -> Result<(), AppError> {
    let Some(options) = options else {
        return Ok(());
    };
    for key in options.keys() {
        if !known.contains(&key.as_str()) {
            return Err(AppError::invalid(format!("Unknown {method} option: {key}")));
        }
    }
    Ok(())
}

/// Reads a boolean option; `null` counts as absent.
fn opt_bool(options: Option<&Document>, key: &str) -> Result<Option<bool>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(AppError::invalid(format!("{key} must be true or false"))),
    }
}

/// Reads a string option; `null` counts as absent.
fn opt_str<'a>(options: Option<&'a Document>, key: &str) -> Result<Option<&'a str>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(AppError::invalid(format!("{key} must be a string"))),
    }
}

/// Reads a document option; `null` counts as absent.
fn opt_document<'a>(
    options: Option<&'a Document>,
    key: &str,
) -> Result<Option<&'a Document>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(value @ Bson::Document(_)) => Ok(value.as_document()),
        Some(_) => Err(AppError::invalid(format!("{key} must be a document"))),
    }
}

/// Reads a number of seconds as a duration, for `expireAfterSeconds`.
fn opt_seconds(options: Option<&Document>, key: &str) -> Result<Option<Duration>, AppError> {
    let seconds = match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => return Ok(None),
        Some(Bson::Int32(value)) if *value >= 0 => f64::from(*value),
        Some(Bson::Int64(value)) if *value >= 0 => *value as f64,
        Some(Bson::Double(value)) if *value >= 0.0 => *value,
        Some(_) => {
            return Err(AppError::invalid(format!(
                "{key} must be a number of seconds"
            )));
        }
    };
    Ok(Some(Duration::from_secs(seconds as u64)))
}

/// Reads a whole number option, as `bits` is.
fn opt_u32(options: Option<&Document>, key: &str) -> Result<Option<u32>, AppError> {
    let bad = || {
        AppError::invalid(format!(
            "{key} must be a whole number from 0 to {}",
            u32::MAX
        ))
    };
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Int32(value)) if *value >= 0 => Ok(Some(*value as u32)),
        Some(Bson::Int64(value)) if (0..=i64::from(u32::MAX)).contains(value) => {
            Ok(Some(*value as u32))
        }
        Some(Bson::Double(value))
            if value.fract() == 0.0 && *value >= 0.0 && *value <= f64::from(u32::MAX) =>
        {
            Ok(Some(*value as u32))
        }
        Some(_) => Err(bad()),
    }
}

/// Reads a number option, as `min` and `max` are.
fn opt_f64(options: Option<&Document>, key: &str) -> Result<Option<f64>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Int32(value)) => Ok(Some(f64::from(*value))),
        Some(Bson::Int64(value)) => Ok(Some(*value as f64)),
        Some(Bson::Double(value)) => Ok(Some(*value)),
        Some(_) => Err(AppError::invalid(format!("{key} must be a number"))),
    }
}

/// Reads a version option the driver models as an enum, as `textIndexVersion` is.
fn opt_version<T: serde::de::DeserializeOwned>(
    options: Option<&Document>,
    key: &str,
) -> Result<Option<T>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(value @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => {
            mongodb::bson::from_bson::<T>(value.clone())
                .map(Some)
                .map_err(|e| AppError::invalid(format!("Invalid {key}: {e}")))
        }
        Some(_) => Err(AppError::invalid(format!("{key} must be a whole number"))),
    }
}

/// Reads an index hint: an index name or a key document.
fn opt_hint(options: Option<&Document>, key: &str) -> Result<Option<Hint>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(name)) => Ok(Some(Hint::Name(name.clone()))),
        Some(Bson::Document(keys)) => Ok(Some(Hint::Keys(keys.clone()))),
        Some(_) => Err(AppError::invalid(format!(
            "{key} must be an index name or a key document"
        ))),
    }
}

/// Reads a collation document.
fn opt_collation(options: Option<&Document>, key: &str) -> Result<Option<Collation>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Document(value)) => Ok(Some(
            mongodb::bson::from_document(value.clone())
                .map_err(|e| AppError::invalid(format!("Invalid {key}: {e}")))?,
        )),
        Some(_) => Err(AppError::invalid(format!("{key} must be a document"))),
    }
}

/// Reads `returnDocument: 'before' | 'after'`.
fn opt_return_document(
    options: Option<&Document>,
    key: &str,
) -> Result<Option<ReturnDocument>, AppError> {
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::String(value)) if value == "after" => Ok(Some(ReturnDocument::After)),
        Some(Bson::String(value)) if value == "before" => Ok(Some(ReturnDocument::Before)),
        Some(_) => Err(AppError::invalid(format!(
            "{key} must be 'before' or 'after'"
        ))),
    }
}

/// Reads `arrayFilters`, an array of filter documents.
fn opt_array_filters(
    options: Option<&Document>,
    key: &str,
) -> Result<Option<Vec<Document>>, AppError> {
    let wrong = || AppError::invalid(format!("{key} must be an array of documents"));
    match options.and_then(|o| o.get(key)) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Array(filters)) => filters
            .iter()
            .map(|filter| filter.as_document().cloned().ok_or_else(wrong))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(wrong()),
    }
}

/// The `update` argument of an update: a document of update operators or an array of pipeline
/// stages.
fn modification(update: &Bson) -> Result<UpdateModifications, AppError> {
    match update {
        Bson::Document(document) => Ok(UpdateModifications::Document(document.clone())),
        Bson::Array(stages) => Ok(UpdateModifications::Pipeline(
            stages
                .iter()
                .map(|stage| {
                    stage.as_document().cloned().ok_or_else(|| {
                        AppError::invalid("update pipeline stages must be documents")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
        _ => Err(AppError::invalid(
            "update must be a document or an array of pipeline stages",
        )),
    }
}

/// Runs one `db.<collection>.<method>(…)` statement.
async fn execute_method(
    client: &Client,
    database: &str,
    collection: &str,
    method: &Method,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    let coll = client.database(database).collection::<Document>(collection);
    match method {
        Method::InsertOne { document } => {
            let mut options = InsertOneOptions::default();
            options.comment = Some(comment.into());
            coll.insert_one(document).with_options(options).await?;
            Ok(WriteOutcome {
                affected: 1,
                notice: format!("insertOne on {collection}: inserted 1"),
                document: None,
            })
        }
        Method::InsertMany { documents, options } => {
            check_options(method, options.as_ref())?;
            let mut insert_options = InsertManyOptions::default();
            insert_options.ordered = opt_bool(options.as_ref(), "ordered")?;
            insert_options.comment = Some(comment.into());
            let result = coll
                .insert_many(documents)
                .with_options(insert_options)
                .await?;
            let inserted = result.inserted_ids.len() as u64;
            Ok(WriteOutcome {
                affected: inserted,
                notice: format!("insertMany on {collection}: inserted {inserted}"),
                document: None,
            })
        }
        Method::UpdateOne {
            filter,
            update,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let result = coll
                .update_one(filter.clone(), modification(update)?)
                .with_options(update_options(options.as_ref(), comment)?)
                .await?;
            Ok(update_outcome("updateOne", collection, &result))
        }
        Method::UpdateMany {
            filter,
            update,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let result = coll
                .update_many(filter.clone(), modification(update)?)
                .with_options(update_options(options.as_ref(), comment)?)
                .await?;
            Ok(update_outcome("updateMany", collection, &result))
        }
        Method::ReplaceOne {
            filter,
            replacement,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let mut replace_options = ReplaceOptions::default();
            replace_options.upsert = opt_bool(options.as_ref(), "upsert")?;
            replace_options.collation = opt_collation(options.as_ref(), "collation")?;
            replace_options.hint = opt_hint(options.as_ref(), "hint")?;
            replace_options.let_vars = opt_document(options.as_ref(), "let")?.cloned();
            replace_options.comment = Some(comment.into());
            let result = coll
                .replace_one(filter.clone(), replacement)
                .with_options(replace_options)
                .await?;
            Ok(update_outcome("replaceOne", collection, &result))
        }
        Method::DeleteOne { filter } => {
            let result = coll
                .delete_one(filter.clone())
                .with_options(delete_options(comment))
                .await?;
            Ok(WriteOutcome {
                affected: result.deleted_count,
                notice: format!(
                    "deleteOne on {collection}: deleted {}",
                    result.deleted_count
                ),
                document: None,
            })
        }
        Method::DeleteMany { filter } => {
            let result = coll
                .delete_many(filter.clone())
                .with_options(delete_options(comment))
                .await?;
            Ok(WriteOutcome {
                affected: result.deleted_count,
                notice: format!(
                    "deleteMany on {collection}: deleted {}",
                    result.deleted_count
                ),
                document: None,
            })
        }
        Method::FindOneAndUpdate {
            filter,
            update,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let mut find_options = FindOneAndUpdateOptions::default();
            find_options.upsert = opt_bool(options.as_ref(), "upsert")?;
            find_options.return_document = opt_return_document(options.as_ref(), "returnDocument")?;
            find_options.projection = opt_document(options.as_ref(), "projection")?.cloned();
            find_options.sort = opt_document(options.as_ref(), "sort")?.cloned();
            find_options.array_filters = opt_array_filters(options.as_ref(), "arrayFilters")?;
            find_options.collation = opt_collation(options.as_ref(), "collation")?;
            find_options.hint = opt_hint(options.as_ref(), "hint")?;
            find_options.let_vars = opt_document(options.as_ref(), "let")?.cloned();
            find_options.comment = Some(comment.into());
            let document = coll
                .find_one_and_update(filter.clone(), modification(update)?)
                .with_options(find_options)
                .await?;
            Ok(found_outcome("findOneAndUpdate", collection, document))
        }
        Method::FindOneAndReplace {
            filter,
            replacement,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let mut find_options = FindOneAndReplaceOptions::default();
            find_options.upsert = opt_bool(options.as_ref(), "upsert")?;
            find_options.return_document = opt_return_document(options.as_ref(), "returnDocument")?;
            find_options.projection = opt_document(options.as_ref(), "projection")?.cloned();
            find_options.sort = opt_document(options.as_ref(), "sort")?.cloned();
            find_options.collation = opt_collation(options.as_ref(), "collation")?;
            find_options.hint = opt_hint(options.as_ref(), "hint")?;
            find_options.let_vars = opt_document(options.as_ref(), "let")?.cloned();
            find_options.comment = Some(comment.into());
            let document = coll
                .find_one_and_replace(filter.clone(), replacement)
                .with_options(find_options)
                .await?;
            Ok(found_outcome("findOneAndReplace", collection, document))
        }
        Method::FindOneAndDelete { filter, options } => {
            check_options(method, options.as_ref())?;
            let mut find_options = FindOneAndDeleteOptions::default();
            find_options.projection = opt_document(options.as_ref(), "projection")?.cloned();
            find_options.sort = opt_document(options.as_ref(), "sort")?.cloned();
            find_options.collation = opt_collation(options.as_ref(), "collation")?;
            find_options.hint = opt_hint(options.as_ref(), "hint")?;
            find_options.let_vars = opt_document(options.as_ref(), "let")?.cloned();
            find_options.comment = Some(comment.into());
            let document = coll
                .find_one_and_delete(filter.clone())
                .with_options(find_options)
                .await?;
            Ok(found_outcome("findOneAndDelete", collection, document))
        }
        Method::BulkWrite {
            operations,
            options,
        } => {
            check_options(method, options.as_ref())?;
            let ordered = opt_bool(options.as_ref(), "ordered")?.unwrap_or(true);
            bulk_write(&coll, collection, operations, ordered, comment).await
        }
        Method::CreateIndex { keys, options } => {
            check_options(method, options.as_ref())?;
            let model = IndexModel::builder()
                .keys(keys.clone())
                .options(index_options(options.as_ref())?)
                .build();
            let result = coll
                .create_index(model)
                .with_options(create_index_comment(comment))
                .await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("createIndex on {collection}: created {}", result.index_name),
                document: None,
            })
        }
        Method::CreateIndexes { keys, options } => {
            check_options(method, options.as_ref())?;
            let shared = index_options(options.as_ref())?;
            let models = keys
                .iter()
                .map(|key| {
                    IndexModel::builder()
                        .keys(key.clone())
                        .options(shared.clone())
                        .build()
                })
                .collect::<Vec<_>>();
            let result = coll
                .create_indexes(models)
                .with_options(create_index_comment(comment))
                .await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!(
                    "createIndexes on {collection}: created {}",
                    result.index_names.join(", ")
                ),
                document: None,
            })
        }
        Method::DropIndex { index } => {
            drop_index(&coll, database, collection, index, comment).await
        }
        Method::DropIndexes => {
            coll.drop_indexes()
                .with_options(drop_index_comment(comment))
                .await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("dropIndexes on {collection}: dropped all but _id"),
                document: None,
            })
        }
        Method::Drop => {
            coll.drop().await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("drop: dropped {collection}"),
                document: None,
            })
        }
        Method::RenameCollection { to, drop_target } => {
            let mut command = doc! {
                "renameCollection": format!("{database}.{collection}"),
                "to": format!("{database}.{to}"),
                "comment": comment,
            };
            if *drop_target {
                command.insert("dropTarget", true);
            }
            client.database("admin").run_command(command).await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("renameCollection: {collection} -> {to}"),
                document: None,
            })
        }
        Method::Find(_)
        | Method::FindOne { .. }
        | Method::Aggregate { .. }
        | Method::CountDocuments { .. }
        | Method::EstimatedDocumentCount
        | Method::Distinct { .. }
        | Method::GetIndexes
        | Method::Stats => Err(AppError::internal("not a write")),
    }
}

/// The options of `updateOne`/`updateMany`.
fn update_options(options: Option<&Document>, comment: &str) -> Result<UpdateOptions, AppError> {
    let mut update = UpdateOptions::default();
    update.upsert = opt_bool(options, "upsert")?;
    update.array_filters = opt_array_filters(options, "arrayFilters")?;
    update.collation = opt_collation(options, "collation")?;
    update.hint = opt_hint(options, "hint")?;
    update.let_vars = opt_document(options, "let")?.cloned();
    update.comment = Some(comment.into());
    Ok(update)
}

/// The options of `deleteOne`/`deleteMany`: the parser takes no user options for them, so only
/// the query id comment goes through.
fn delete_options(comment: &str) -> DeleteOptions {
    let mut delete = DeleteOptions::default();
    delete.comment = Some(comment.into());
    delete
}

/// Notice and counts of an update or replace.
fn update_outcome(
    method: &str,
    collection: &str,
    result: &mongodb::results::UpdateResult,
) -> WriteOutcome {
    let upserted = u64::from(result.upserted_id.is_some());
    let mut notice = format!(
        "{method} on {collection}: matched {}, modified {}",
        result.matched_count, result.modified_count
    );
    if upserted > 0 {
        notice.push_str(&format!(", upserted {upserted}"));
    }
    WriteOutcome {
        affected: result.modified_count + upserted,
        notice,
        document: None,
    }
}

/// Notice and counts of a find-one-and-* that returned `document`.
fn found_outcome(method: &str, collection: &str, document: Option<Document>) -> WriteOutcome {
    WriteOutcome {
        affected: u64::from(document.is_some()),
        notice: format!(
            "{method} on {collection}: {}",
            if document.is_some() {
                "1 document"
            } else {
                "no document matched"
            }
        ),
        document,
    }
}

/// The counts one bulkWrite operation adds to the result.
#[derive(Default)]
struct OpCounts {
    inserted: u64,
    matched: u64,
    modified: u64,
    deleted: u64,
    upserted: u64,
}

/// Runs the mongosh models of a `bulkWrite` in order through the collection methods. With
/// `ordered: false` a failing operation does not stop the rest; the first error is reported at
/// the end.
async fn bulk_write(
    coll: &Collection<Document>,
    collection: &str,
    operations: &[Document],
    ordered: bool,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    let mut total = OpCounts::default();
    let mut first_error = None;
    for operation in operations {
        match run_operation(coll, operation, ordered, comment).await {
            Ok(counts) => {
                total.inserted += counts.inserted;
                total.matched += counts.matched;
                total.modified += counts.modified;
                total.deleted += counts.deleted;
                total.upserted += counts.upserted;
            }
            Err(error) => {
                if ordered {
                    return Err(error);
                }
                first_error.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(WriteOutcome {
        affected: total.inserted + total.modified + total.deleted + total.upserted,
        notice: format!(
            "bulkWrite on {collection}: inserted {}, matched {}, modified {}, deleted {}, upserted {}",
            total.inserted, total.matched, total.modified, total.deleted, total.upserted
        ),
        document: None,
    })
}

/// Runs one mongosh model of a `bulkWrite`, e.g. `{insertOne: {document: {...}}}`.
async fn run_operation(
    coll: &Collection<Document>,
    operation: &Document,
    ordered: bool,
    comment: &str,
) -> Result<OpCounts, AppError> {
    if operation.len() != 1 {
        return Err(AppError::invalid(
            "bulkWrite operation must be one model like {insertOne: {document: {...}}}",
        ));
    }
    let Some((model, value)) = operation.iter().next() else {
        return Err(AppError::invalid("bulkWrite operation is empty"));
    };
    let model = model.as_str();
    let Some(body) = value.as_document() else {
        return Err(AppError::invalid(format!(
            "bulkWrite {model} needs a document"
        )));
    };
    match model {
        "insertOne" => {
            check_keys(model, Some(body), &["document"])?;
            let document = operation_document(Some(body), "document", model)?;
            let mut options = InsertOneOptions::default();
            options.comment = Some(comment.into());
            coll.insert_one(document).with_options(options).await?;
            Ok(OpCounts {
                inserted: 1,
                ..OpCounts::default()
            })
        }
        "insertMany" => {
            check_keys(model, Some(body), &["documents"])?;
            let documents = operation_documents(Some(body), "documents", model)?;
            let mut options = InsertManyOptions::default();
            options.ordered = Some(ordered);
            options.comment = Some(comment.into());
            let result = coll.insert_many(documents).with_options(options).await?;
            Ok(OpCounts {
                inserted: result.inserted_ids.len() as u64,
                ..OpCounts::default()
            })
        }
        "updateOne" | "updateMany" => {
            check_keys(
                model,
                Some(body),
                &[
                    "filter",
                    "update",
                    "upsert",
                    "arrayFilters",
                    "collation",
                    "hint",
                    "let",
                ],
            )?;
            let filter = operation_document(Some(body), "filter", model)?;
            let update = body.get("update").ok_or_else(|| {
                AppError::invalid(format!("{model} needs an update document or pipeline"))
            })?;
            let result = if model == "updateOne" {
                coll.update_one(filter.clone(), modification(update)?)
                    .with_options(update_options(Some(body), comment)?)
                    .await?
            } else {
                coll.update_many(filter.clone(), modification(update)?)
                    .with_options(update_options(Some(body), comment)?)
                    .await?
            };
            Ok(OpCounts {
                matched: result.matched_count,
                modified: result.modified_count,
                upserted: u64::from(result.upserted_id.is_some()),
                ..OpCounts::default()
            })
        }
        "replaceOne" => {
            check_keys(
                model,
                Some(body),
                &[
                    "filter",
                    "replacement",
                    "upsert",
                    "collation",
                    "hint",
                    "let",
                ],
            )?;
            let filter = operation_document(Some(body), "filter", model)?;
            let replacement = operation_document(Some(body), "replacement", model)?;
            let mut options = ReplaceOptions::default();
            options.upsert = opt_bool(Some(body), "upsert")?;
            options.collation = opt_collation(Some(body), "collation")?;
            options.hint = opt_hint(Some(body), "hint")?;
            options.let_vars = opt_document(Some(body), "let")?.cloned();
            options.comment = Some(comment.into());
            let result = coll
                .replace_one(filter.clone(), replacement)
                .with_options(options)
                .await?;
            Ok(OpCounts {
                matched: result.matched_count,
                modified: result.modified_count,
                upserted: u64::from(result.upserted_id.is_some()),
                ..OpCounts::default()
            })
        }
        "deleteOne" | "deleteMany" => {
            check_keys(model, Some(body), &["filter", "collation", "hint"])?;
            let filter = operation_document(Some(body), "filter", model)?;
            let mut options = DeleteOptions::default();
            options.collation = opt_collation(Some(body), "collation")?;
            options.hint = opt_hint(Some(body), "hint")?;
            options.comment = Some(comment.into());
            let result = if model == "deleteOne" {
                coll.delete_one(filter.clone())
                    .with_options(options)
                    .await?
            } else {
                coll.delete_many(filter.clone())
                    .with_options(options)
                    .await?
            };
            Ok(OpCounts {
                deleted: result.deleted_count,
                ..OpCounts::default()
            })
        }
        other => Err(AppError::invalid(format!(
            "Unknown bulkWrite operation: {other}"
        ))),
    }
}

/// The one document an operation model needs under `key`.
fn operation_document<'a>(
    body: Option<&'a Document>,
    key: &str,
    model: &str,
) -> Result<&'a Document, AppError> {
    opt_document(body, key)?.ok_or_else(|| AppError::invalid(format!("{model} needs {key}")))
}

/// The array of documents an operation model needs under `key`.
fn operation_documents(
    body: Option<&Document>,
    key: &str,
    model: &str,
) -> Result<Vec<Document>, AppError> {
    let Some(Bson::Array(documents)) = body.and_then(|body| body.get(key)) else {
        return Err(AppError::invalid(format!(
            "{model} needs an array of documents"
        )));
    };
    documents
        .iter()
        .map(|document| {
            document
                .as_document()
                .cloned()
                .ok_or_else(|| AppError::invalid(format!("{model} needs an array of documents")))
        })
        .collect()
}

/// The shared index options of `createIndex`/`createIndexes`.
fn index_options(options: Option<&Document>) -> Result<Option<IndexOptions>, AppError> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mut index = IndexOptions::default();
    index.name = opt_str(Some(options), "name")?.map(str::to_string);
    index.unique = opt_bool(Some(options), "unique")?;
    index.sparse = opt_bool(Some(options), "sparse")?;
    index.expire_after = opt_seconds(Some(options), "expireAfterSeconds")?;
    index.partial_filter_expression =
        opt_document(Some(options), "partialFilterExpression")?.cloned();
    index.collation = opt_collation(Some(options), "collation")?;
    index.hidden = opt_bool(Some(options), "hidden")?;
    index.weights = opt_document(Some(options), "weights")?.cloned();
    index.default_language = opt_str(Some(options), "default_language")?.map(str::to_string);
    index.wildcard_projection = opt_document(Some(options), "wildcardProjection")?.cloned();
    index.language_override = opt_str(Some(options), "language_override")?.map(str::to_string);
    index.text_index_version = opt_version(Some(options), "textIndexVersion")?;
    index.sphere_2d_index_version = opt_version(Some(options), "2dsphereIndexVersion")?;
    index.bits = opt_u32(Some(options), "bits")?;
    index.min = opt_f64(Some(options), "min")?;
    index.max = opt_f64(Some(options), "max")?;
    index.storage_engine = opt_document(Some(options), "storageEngine")?.cloned();
    // The server ignores `background`; the driver keeps the field deprecated.
    #[allow(deprecated)]
    {
        index.background = opt_bool(Some(options), "background")?;
    }
    Ok(Some(index))
}

/// The options of an index build: the query id as `comment`, so a cancel finds it in
/// `$currentOp`. The build itself is not interrupted by a kill; the comment only names it.
fn create_index_comment(comment: &str) -> CreateIndexOptions {
    let mut options = CreateIndexOptions::default();
    options.comment = Some(Bson::String(comment.to_string()));
    options
}

/// The options of a drop: the query id as `comment`, so a cancel finds it in `$currentOp`.
fn drop_index_comment(comment: &str) -> DropIndexOptions {
    let mut options = DropIndexOptions::default();
    options.comment = Some(Bson::String(comment.to_string()));
    options
}

/// `dropIndex` with an index name or a key document. A key document goes to the server as it is,
/// so the server resolves the index itself.
async fn drop_index(
    coll: &Collection<Document>,
    database: &str,
    collection: &str,
    index: &Bson,
    comment: &str,
) -> Result<WriteOutcome, AppError> {
    match index {
        Bson::String(name) => {
            coll.drop_index(name)
                .with_options(drop_index_comment(comment))
                .await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("dropIndex on {collection}: dropped {name}"),
                document: None,
            })
        }
        Bson::Document(keys) => {
            let command = doc! {
                "dropIndexes": collection,
                "index": keys.clone(),
                "comment": comment,
            };
            coll.client()
                .database(database)
                .run_command(command)
                .await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("dropIndex on {collection}: dropped {}", index_name(keys)),
                document: None,
            })
        }
        _ => Err(AppError::invalid(
            "dropIndex needs an index name or a key document",
        )),
    }
}

/// The name the server generates for a key document, e.g. `{a: 1, b: -1}` → `a_1_b_-1`.
fn index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(field, value)| format!("{field}_{}", value_name(value)))
        .collect::<Vec<_>>()
        .join("_")
}

/// The name part of one key value: a sort direction or an index type like `text`.
fn value_name(value: &Bson) -> String {
    match value {
        Bson::Int32(value) => value.to_string(),
        Bson::Int64(value) => value.to_string(),
        Bson::Double(value) => value.to_string(),
        Bson::String(value) => value.clone(),
        other => other.to_string(),
    }
}
