//! Execution of one parsed write statement. `guard::kind` sends every statement that changes
//! documents or the schema here, including an aggregate whose pipeline ends in `$out` or
//! `$merge`.

use mongodb::{
    Client, Collection, IndexModel,
    bson::{Bson, Document, doc},
    options::{
        CreateIndexOptions, DeleteOptions, DropIndexOptions, FindOneAndDeleteOptions,
        FindOneAndReplaceOptions, FindOneAndUpdateOptions, IndexOptions, InsertManyOptions,
        InsertOneOptions, ReplaceOptions, UpdateModifications, UpdateOptions,
    },
};

use crate::core::error::AppError;
use crate::core::mongo::command::{Command, Method};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::options::{apply_aggregate_options, Options};

const UPDATE_OPTIONS: &[&str] = &["upsert", "arrayFilters", "collation", "hint", "let"];
const REPLACE_OPTIONS: &[&str] = &["upsert", "collation", "hint", "let"];
const FIND_ONE_AND_UPDATE_OPTIONS: &[&str] = &[
    "upsert",
    "returnDocument",
    "projection",
    "sort",
    "arrayFilters",
    "collation",
    "hint",
    "let",
];
const FIND_ONE_AND_REPLACE_OPTIONS: &[&str] = &[
    "upsert",
    "returnDocument",
    "projection",
    "sort",
    "collation",
    "hint",
    "let",
];
const FIND_ONE_AND_DELETE_OPTIONS: &[&str] = &["projection", "sort", "collation", "hint", "let"];
const ORDERED_OPTIONS: &[&str] = &["ordered"];
/// The export script writes a stored index with the same keys.
pub(crate) const CREATE_INDEX_OPTIONS: &[&str] = &[
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
];
const DROP_DATABASE_OPTIONS: &[&str] = &["writeConcern"];

#[derive(Debug)]
pub struct WriteOutcome {
    /// Inserted, modified, deleted or upserted count; 0 for schema changes and an aggregate that
    /// writes.
    pub affected: u64,
    /// One line for `QueryRun.notices`, e.g. "updateMany on orders: matched 3, modified 2".
    pub notice: String,
    /// findOneAndUpdate / findOneAndReplace / findOneAndDelete: the returned document, if any.
    pub document: Option<Document>,
}

/// Runs one statement whose `guard::kind` is Write or Schema. Every operation carries `comment`;
/// a collection `drop` is the one the driver cannot comment.
pub async fn execute(
    client: &Client,
    database: &str,
    cmd: &Command,
    comment: OpComment<'_>,
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
/// as it is. A user `comment` replaces the run's.
async fn create_collection(
    client: &Client,
    database: &str,
    name: &str,
    options: Option<&Document>,
    comment: OpComment<'_>,
) -> Result<WriteOutcome, AppError> {
    let mut command = doc! { "create": name };
    if let Some(options) = options {
        for (key, value) in options {
            command.insert(key.clone(), value.clone());
        }
    }
    comment.apply(&mut command);
    client.database(database).run_command(command).await?;
    Ok(WriteOutcome {
        affected: 0,
        notice: format!("createCollection: created {name}"),
        document: None,
    })
}

/// `db.dropDatabase()`: the server command, so the run's comment goes along. mongosh takes only
/// `writeConcern`; the server validates its value.
async fn drop_database(
    client: &Client,
    database: &str,
    options: Option<&Document>,
    comment: OpComment<'_>,
) -> Result<WriteOutcome, AppError> {
    Options::new("dropDatabase", options).check(DROP_DATABASE_OPTIONS)?;
    let mut command = doc! { "dropDatabase": 1 };
    if let Some(options) = options {
        for (key, value) in options {
            command.insert(key.clone(), value.clone());
        }
    }
    comment.apply(&mut command);
    client.database(database).run_command(command).await?;
    Ok(WriteOutcome {
        affected: 0,
        notice: format!("dropDatabase: dropped {database}"),
        document: None,
    })
}

fn known_options(method: &Method) -> &'static [&'static str] {
    match method {
        Method::InsertMany { .. } | Method::BulkWrite { .. } => ORDERED_OPTIONS,
        Method::UpdateOne { .. } | Method::UpdateMany { .. } => UPDATE_OPTIONS,
        Method::ReplaceOne { .. } => REPLACE_OPTIONS,
        Method::FindOneAndUpdate { .. } => FIND_ONE_AND_UPDATE_OPTIONS,
        Method::FindOneAndReplace { .. } => FIND_ONE_AND_REPLACE_OPTIONS,
        Method::FindOneAndDelete { .. } => FIND_ONE_AND_DELETE_OPTIONS,
        Method::CreateIndex { .. } | Method::CreateIndexes { .. } => CREATE_INDEX_OPTIONS,
        _ => &[],
    }
}

fn method_options<'a>(method: &'a Method, options: Option<&'a Document>) -> Result<Options<'a>, AppError> {
    let o = Options::new(method.name(), options);
    o.check(known_options(method))?;
    Ok(o)
}

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

async fn execute_method(
    client: &Client,
    database: &str,
    collection: &str,
    method: &Method,
    comment: OpComment<'_>,
) -> Result<WriteOutcome, AppError> {
    let coll = client.database(database).collection::<Document>(collection);
    match method {
        Method::InsertOne { document } => {
            let mut options = InsertOneOptions::default();
            options.comment = Some(comment.tag());
            coll.insert_one(document).with_options(options).await?;
            Ok(WriteOutcome {
                affected: 1,
                notice: format!("insertOne on {collection}: inserted 1"),
                document: None,
            })
        }
        Method::InsertMany { documents, options } => {
            let o = method_options(method, options.as_ref())?;
            let mut insert_options = InsertManyOptions::default();
            insert_options.ordered = o.bool("ordered")?;
            insert_options.comment = Some(comment.tag());
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
            let o = method_options(method, options.as_ref())?;
            let result = coll
                .update_one(filter.clone(), modification(update)?)
                .with_options(update_options(&o, comment)?)
                .await?;
            Ok(update_outcome("updateOne", collection, &result))
        }
        Method::UpdateMany {
            filter,
            update,
            options,
        } => {
            let o = method_options(method, options.as_ref())?;
            let result = coll
                .update_many(filter.clone(), modification(update)?)
                .with_options(update_options(&o, comment)?)
                .await?;
            Ok(update_outcome("updateMany", collection, &result))
        }
        Method::ReplaceOne {
            filter,
            replacement,
            options,
        } => {
            let o = method_options(method, options.as_ref())?;
            let result = coll
                .replace_one(filter.clone(), replacement)
                .with_options(replace_options(&o, comment)?)
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
            let o = method_options(method, options.as_ref())?;
            let mut find_options = FindOneAndUpdateOptions::default();
            find_options.upsert = o.bool("upsert")?;
            find_options.return_document = o.return_document("returnDocument")?;
            find_options.projection = o.document("projection")?.cloned();
            find_options.sort = o.document("sort")?.cloned();
            find_options.array_filters = o.array_filters("arrayFilters")?;
            find_options.collation = o.collation("collation")?;
            find_options.hint = o.hint("hint")?;
            find_options.let_vars = o.document("let")?.cloned();
            find_options.comment = Some(comment.tag());
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
            let o = method_options(method, options.as_ref())?;
            let mut find_options = FindOneAndReplaceOptions::default();
            find_options.upsert = o.bool("upsert")?;
            find_options.return_document = o.return_document("returnDocument")?;
            find_options.projection = o.document("projection")?.cloned();
            find_options.sort = o.document("sort")?.cloned();
            find_options.collation = o.collation("collation")?;
            find_options.hint = o.hint("hint")?;
            find_options.let_vars = o.document("let")?.cloned();
            find_options.comment = Some(comment.tag());
            let document = coll
                .find_one_and_replace(filter.clone(), replacement)
                .with_options(find_options)
                .await?;
            Ok(found_outcome("findOneAndReplace", collection, document))
        }
        Method::FindOneAndDelete { filter, options } => {
            let o = method_options(method, options.as_ref())?;
            let mut find_options = FindOneAndDeleteOptions::default();
            find_options.projection = o.document("projection")?.cloned();
            find_options.sort = o.document("sort")?.cloned();
            find_options.collation = o.collation("collation")?;
            find_options.hint = o.hint("hint")?;
            find_options.let_vars = o.document("let")?.cloned();
            find_options.comment = Some(comment.tag());
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
            let o = method_options(method, options.as_ref())?;
            let ordered = o.bool("ordered")?.unwrap_or(true);
            bulk_write(&coll, collection, operations, ordered, comment).await
        }
        Method::CreateIndex { keys, options } => {
            let o = method_options(method, options.as_ref())?;
            let model = IndexModel::builder()
                .keys(keys.clone())
                .options(index_options(&o)?)
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
            let o = method_options(method, options.as_ref())?;
            let shared = index_options(&o)?;
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
        Method::DropIndex { index } => drop_index(&coll, database, collection, index, comment).await,
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
        // The driver's `drop` has no `comment` option, so a cancel cannot find this one in
        // `$currentOp`; it still stops the run before the next statement.
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
            };
            if *drop_target {
                command.insert("dropTarget", true);
            }
            comment.apply(&mut command);
            client.database("admin").run_command(command).await?;
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("renameCollection: {collection} -> {to}"),
                document: None,
            })
        }
        // An aggregate whose pipeline has `$out` or `$merge`: the server writes while the cursor
        // runs empty, so the whole pipeline is streamed and the counts are the server's. The
        // options are the read path's document, mapped by the same helper.
        Method::Aggregate { pipeline, options } => {
            let o = Options::new("aggregate", options.as_ref());
            let action = coll.aggregate(pipeline.clone()).comment(comment.tag());
            let mut cursor = apply_aggregate_options(action, &o)?.await?;
            while cursor.advance().await? {}
            Ok(WriteOutcome {
                affected: 0,
                notice: format!("aggregate on {collection}: wrote the pipeline result"),
                document: None,
            })
        }
        Method::Find(_)
        | Method::FindOne { .. }
        | Method::CountDocuments { .. }
        | Method::EstimatedDocumentCount
        | Method::Distinct { .. }
        | Method::GetIndexes
        | Method::Stats => Err(AppError::internal("not a write")),
    }
}

fn update_options(o: &Options<'_>, comment: OpComment<'_>) -> Result<UpdateOptions, AppError> {
    let mut update = UpdateOptions::default();
    update.upsert = o.bool("upsert")?;
    update.array_filters = o.array_filters("arrayFilters")?;
    update.collation = o.collation("collation")?;
    update.hint = o.hint("hint")?;
    update.let_vars = o.document("let")?.cloned();
    update.comment = Some(comment.tag());
    Ok(update)
}

fn replace_options(o: &Options<'_>, comment: OpComment<'_>) -> Result<ReplaceOptions, AppError> {
    let mut replace = ReplaceOptions::default();
    replace.upsert = o.bool("upsert")?;
    replace.collation = o.collation("collation")?;
    replace.hint = o.hint("hint")?;
    replace.let_vars = o.document("let")?.cloned();
    replace.comment = Some(comment.tag());
    Ok(replace)
}

/// The options of `deleteOne`/`deleteMany`: the parser takes no user options for them, so only
/// the run's comment goes through.
fn delete_options(comment: OpComment<'_>) -> DeleteOptions {
    let mut delete = DeleteOptions::default();
    delete.comment = Some(comment.tag());
    delete
}

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
    comment: OpComment<'_>,
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

async fn run_operation(
    coll: &Collection<Document>,
    operation: &Document,
    ordered: bool,
    comment: OpComment<'_>,
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
            Options::new(model, Some(body)).check(&["document"])?;
            let document = operation_document(Some(body), "document", model)?;
            let mut options = InsertOneOptions::default();
            options.comment = Some(comment.tag());
            coll.insert_one(document).with_options(options).await?;
            Ok(OpCounts {
                inserted: 1,
                ..OpCounts::default()
            })
        }
        "insertMany" => {
            Options::new(model, Some(body)).check(&["documents"])?;
            let documents = operation_documents(Some(body), "documents", model)?;
            let mut options = InsertManyOptions::default();
            options.ordered = Some(ordered);
            options.comment = Some(comment.tag());
            let result = coll.insert_many(documents).with_options(options).await?;
            Ok(OpCounts {
                inserted: result.inserted_ids.len() as u64,
                ..OpCounts::default()
            })
        }
        "updateOne" | "updateMany" => {
            let o = Options::new(model, Some(body));
            o.check(&[&["filter", "update"], UPDATE_OPTIONS].concat())?;
            let filter = operation_document(Some(body), "filter", model)?;
            let update = body.get("update").ok_or_else(|| {
                AppError::invalid(format!("{model} needs an update document or pipeline"))
            })?;
            let result = if model == "updateOne" {
                coll.update_one(filter.clone(), modification(update)?)
                    .with_options(update_options(&o, comment)?)
                    .await?
            } else {
                coll.update_many(filter.clone(), modification(update)?)
                    .with_options(update_options(&o, comment)?)
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
            let o = Options::new(model, Some(body));
            o.check(&[&["filter", "replacement"], REPLACE_OPTIONS].concat())?;
            let filter = operation_document(Some(body), "filter", model)?;
            let replacement = operation_document(Some(body), "replacement", model)?;
            let result = coll
                .replace_one(filter.clone(), replacement)
                .with_options(replace_options(&o, comment)?)
                .await?;
            Ok(OpCounts {
                matched: result.matched_count,
                modified: result.modified_count,
                upserted: u64::from(result.upserted_id.is_some()),
                ..OpCounts::default()
            })
        }
        "deleteOne" | "deleteMany" => {
            let o = Options::new(model, Some(body));
            o.check(&["filter", "collation", "hint"])?;
            let filter = operation_document(Some(body), "filter", model)?;
            let mut options = DeleteOptions::default();
            options.collation = o.collation("collation")?;
            options.hint = o.hint("hint")?;
            options.comment = Some(comment.tag());
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

fn operation_document<'a>(
    body: Option<&'a Document>,
    key: &str,
    model: &str,
) -> Result<&'a Document, AppError> {
    body.and_then(|body| match body.get(key) {
        None | Some(Bson::Null) => None,
        Some(value) => value.as_document(),
    })
    .ok_or_else(|| AppError::invalid(format!("{model} needs {key}")))
}

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

fn index_options(o: &Options<'_>) -> Result<Option<IndexOptions>, AppError> {
    let mut index = IndexOptions::default();
    index.name = o.string("name")?.map(str::to_string);
    index.unique = o.bool("unique")?;
    index.sparse = o.bool("sparse")?;
    index.expire_after = o.seconds("expireAfterSeconds")?;
    index.partial_filter_expression = o.document("partialFilterExpression")?.cloned();
    index.collation = o.collation("collation")?;
    index.hidden = o.bool("hidden")?;
    index.weights = o.document("weights")?.cloned();
    index.default_language = o.string("default_language")?.map(str::to_string);
    index.wildcard_projection = o.document("wildcardProjection")?.cloned();
    index.language_override = o.string("language_override")?.map(str::to_string);
    index.text_index_version = o.version("textIndexVersion")?;
    index.sphere_2d_index_version = o.version("2dsphereIndexVersion")?;
    index.bits = o
        .whole("bits", i64::from(u32::MAX))?
        .map(|bits| bits as u32);
    index.min = o.number("min")?;
    index.max = o.number("max")?;
    index.storage_engine = o.document("storageEngine")?.cloned();
    // The server ignores `background`; the driver keeps the field deprecated.
    #[allow(deprecated)]
    {
        index.background = o.bool("background")?;
    }
    Ok(Some(index))
}

/// The options of an index build: the run's comment, so a cancel finds it in `$currentOp`. The
/// build itself is not interrupted by a kill; the comment only names it.
fn create_index_comment(comment: OpComment<'_>) -> CreateIndexOptions {
    let mut options = CreateIndexOptions::default();
    options.comment = Some(comment.tag());
    options
}

/// The options of a drop: the run's comment, so a cancel finds it in `$currentOp`.
fn drop_index_comment(comment: OpComment<'_>) -> DropIndexOptions {
    let mut options = DropIndexOptions::default();
    options.comment = Some(comment.tag());
    options
}

/// `dropIndex` with an index name or a key document. A key document goes to the server as it is,
/// so the server resolves the index itself.
async fn drop_index(
    coll: &Collection<Document>,
    database: &str,
    collection: &str,
    index: &Bson,
    comment: OpComment<'_>,
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
            let mut command = doc! {
                "dropIndexes": collection,
                "index": keys.clone(),
            };
            comment.apply(&mut command);
            coll.client().database(database).run_command(command).await?;
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

fn value_name(value: &Bson) -> String {
    match value {
        Bson::Int32(value) => value.to_string(),
        Bson::Int64(value) => value.to_string(),
        Bson::Double(value) => value.to_string(),
        Bson::String(value) => value.clone(),
        other => other.to_string(),
    }
}
