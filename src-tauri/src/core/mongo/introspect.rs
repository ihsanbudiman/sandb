//! Schema browsing for MongoDB: databases, collections and views, and a field summary per
//! collection. There is no schema level and no fixed columns: `columns` samples documents and
//! `snapshot` does the same with a smaller sample so autocomplete stays fast.

use futures_util::TryStreamExt;
use mongodb::bson::{Bson, Document, RawDocumentBuf, doc};
use mongodb::error::{Error as MongoError, ErrorKind};
use mongodb::results::CollectionType;
use mongodb::{Client, Database};

use crate::core::db::introspect::{
    self, ColumnInfo, IndexInfo, MongoCollectionInfo, MongoCollectionKind, MongoCollectionStats,
    MongoIndexInfo, SNAPSHOT_MAX_COLUMNS, SNAPSHOT_MAX_TABLES, SchemaSnapshot, SnapRow, SnapTable,
    TableInfo, TableKind, TableProperties,
};
use crate::core::error::AppError;
use crate::core::mongo::schema::{
    FieldSummary, SAMPLE_BYTES, SAMPLE_DOCS, SNAPSHOT_SAMPLE_BYTES, SNAPSHOT_SAMPLE_DOCS,
};
use crate::core::mongo::shell;

/// MongoDB `Unauthorized`: the authenticated user may not run the command.
const UNAUTHORIZED: i32 = 13;
/// Instance databases that are never the user's own data.
const HIDDEN_DATABASES: [&str; 3] = ["admin", "config", "local"];

fn unauthorized(e: &MongoError) -> bool {
    matches!(&*e.kind, ErrorKind::Command(c) if c.code == UNAUTHORIZED)
}

/// Databases the connection's user may see. `authorizedDatabases` asks the server for exactly
/// those; a compatible server that does not know the option (Azure Cosmos DB for MongoDB) refuses
/// the command, so the list is asked again without it. A user that may not run `listDatabases`
/// at all gets the connection's own database.
pub async fn databases(client: &Client, own: &str) -> Result<Vec<String>, AppError> {
    let listed = match client
        .list_database_names()
        .authorized_databases(true)
        .await
    {
        Err(e) if matches!(&*e.kind, ErrorKind::Command(_)) && !unauthorized(&e) => {
            client.list_database_names().await
        }
        listed => listed,
    };
    let mut names = match listed {
        Ok(names) => names,
        Err(e) if unauthorized(&e) && !own.is_empty() => return Ok(vec![own.to_string()]),
        Err(e) => return Err(e.into()),
    };
    names.retain(|name| !HIDDEN_DATABASES.contains(&name.as_str()));
    names.sort();
    if names.is_empty() && !own.is_empty() {
        return Ok(vec![own.to_string()]);
    }
    Ok(names)
}

/// Collections and views of `db`, sorted by name. `system.*` is internal and never listed.
/// `row_estimate` is the metadata count; a view has none, and a failed count leaves the field
/// out instead of failing the whole list.
pub async fn tables(client: &Client, db: &str) -> Result<Vec<TableInfo>, AppError> {
    let db = client.database(db);
    let mut cursor = db.list_collections().authorized_collections(true).await?;
    let mut tables = Vec::new();
    while let Some(spec) = cursor.try_next().await? {
        if spec.name.starts_with("system.") {
            continue;
        }
        let kind = match spec.collection_type {
            CollectionType::View => TableKind::View,
            _ => TableKind::Table,
        };
        let row_estimate = if kind == TableKind::Table {
            db.collection::<Document>(&spec.name)
                .estimated_document_count()
                .await
                .ok()
                .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
        } else {
            None
        };
        tables.push(TableInfo {
            name: spec.name,
            kind,
            row_estimate,
            has_rowid: None,
            partition_of: None,
        });
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tables)
}

/// The field summary of `coll`: the `$jsonSchema` validator merged with a `$sample` stream.
/// An empty collection yields an empty list, not `not_found`; an unknown name does.
pub async fn columns(client: &Client, db: &str, coll: &str) -> Result<Vec<ColumnInfo>, AppError> {
    let db = client.database(db);
    let names = db
        .list_collection_names()
        .authorized_collections(true)
        .filter(doc! { "name": coll })
        .await?;
    if names.is_empty() {
        return Err(introspect::not_found_table(coll));
    }
    let mut summary = FieldSummary::default();
    if let Some(validator) = validator(&db, coll).await? {
        summary.add_validator(&validator);
    }
    add_sample(&db, coll, SAMPLE_DOCS, SAMPLE_BYTES, &mut summary).await?;
    Ok(summary.into_columns())
}

/// Collections and their field paths for autocomplete. Views contribute their name only;
/// a collection the user may not read contributes nothing. Both caps end the walk early.
pub async fn snapshot(client: &Client, db: &str) -> Result<SchemaSnapshot, AppError> {
    let infos = tables(client, db)
        .await?
        .into_iter()
        .take(SNAPSHOT_MAX_TABLES + 1);
    let mut snap_tables = Vec::new();
    let mut rows = Vec::new();
    for info in infos {
        snap_tables.push(SnapTable {
            schema: None,
            table: info.name.clone(),
            kind: info.kind,
        });
        if info.kind != TableKind::Table || rows.len() > SNAPSHOT_MAX_COLUMNS {
            continue;
        }
        let mut summary = FieldSummary::default();
        // A collection the user cannot read keeps its name but no columns.
        if add_sample(
            &client.database(db),
            &info.name,
            SNAPSHOT_SAMPLE_DOCS,
            SNAPSHOT_SAMPLE_BYTES,
            &mut summary,
        )
        .await
        .is_err()
        {
            continue;
        }
        let take = SNAPSHOT_MAX_COLUMNS + 1 - rows.len();
        rows.extend(
            summary
                .into_columns()
                .into_iter()
                .take(take)
                .map(|c| SnapRow {
                    schema: None,
                    table: info.name.clone(),
                    column: c.name,
                    data_type: c.data_type,
                }),
        );
    }
    Ok(introspect::group_snapshot(
        db.to_string(),
        snap_tables,
        rows,
    ))
}

/// The `options.validator` of `coll`, when the collection has one.
async fn validator(db: &Database, coll: &str) -> Result<Option<Document>, AppError> {
    let mut cursor = db
        .list_collections()
        .authorized_collections(true)
        .filter(doc! { "name": coll })
        .await?;
    while let Some(spec) = cursor.try_next().await? {
        if spec.name == coll {
            return Ok(spec.options.validator);
        }
    }
    Ok(None)
}

/// Streams `aggregate([{ $sample: { size } }])` into `summary`. Documents stay raw until the
/// merge so `as_bytes` gives the size for free; the document that crosses `bytes` is still
/// merged, then the loop stops. Nothing is collected into a Vec.
async fn add_sample(
    db: &Database,
    coll: &str,
    docs: i64,
    bytes: usize,
    summary: &mut FieldSummary,
) -> Result<(), AppError> {
    let mut cursor = db
        .collection::<Document>(coll)
        .aggregate([doc! { "$sample": { "size": docs } }])
        .batch_size(100)
        .with_type::<RawDocumentBuf>()
        .await?;
    let mut total = 0usize;
    while let Some(raw) = cursor.try_next().await? {
        total += raw.as_bytes().len();
        let doc = raw.to_document().map_err(|e| {
            AppError::internal(format!("A sampled document is not valid BSON: {e}"))
        })?;
        summary.add(&doc);
        if total > bytes {
            break;
        }
    }
    Ok(())
}

// ---- collection properties and the creation script

/// The `listCollections` entry of one collection and its index specs.
pub(crate) struct CollectionSpec {
    pub kind: MongoCollectionKind,
    /// The raw `options` of the entry: validator, validationLevel, validationAction, capped,
    /// timeseries, collation, viewOn, pipeline, ...
    pub options: Document,
    /// The `listIndexes` entries; empty for a view, which has no indexes.
    pub indexes: Vec<Document>,
}

/// The spec of `coll`. An unknown name is `not_found`, as `columns` has it.
pub(crate) async fn spec(
    client: &Client,
    db: &str,
    coll: &str,
) -> Result<CollectionSpec, AppError> {
    let database = client.database(db);
    let reply = database
        .run_command(doc! { "listCollections": 1, "filter": { "name": coll }, "cursor": {} })
        .await?;
    let entry = first_batch(&reply)?
        .into_iter()
        .find(|entry| entry.get_str("name").is_ok_and(|name| name == coll))
        .ok_or_else(|| introspect::not_found_table(coll))?;
    let kind = match entry.get_str("type").unwrap_or("collection") {
        "view" => MongoCollectionKind::View,
        "timeseries" => MongoCollectionKind::Timeseries,
        _ => MongoCollectionKind::Collection,
    };
    let options = entry
        .get_document("options")
        .ok()
        .cloned()
        .unwrap_or_default();
    let indexes = if kind == MongoCollectionKind::View {
        Vec::new()
    } else {
        let reply = database
            .run_command(doc! { "listIndexes": coll, "cursor": {} })
            .await?;
        first_batch(&reply)?
    };
    Ok(CollectionSpec {
        kind,
        options,
        indexes,
    })
}

/// Indexes, validator, options and stats of one collection or view. A view has neither indexes
/// nor stats; a refused stats read (not authorized) leaves `stats` out instead of failing.
pub async fn properties(
    client: &Client,
    db: &str,
    coll: &str,
) -> Result<TableProperties, AppError> {
    let spec = spec(client, db, coll).await?;
    let mut options = spec.options.clone();
    let validator = options.remove("validator").and_then(into_document);
    let validation_level = options.remove("validationLevel").and_then(into_string);
    let validation_action = options.remove("validationAction").and_then(into_string);
    let indexes = spec
        .indexes
        .iter()
        .map(|index| index_info(coll, index))
        .collect::<Result<Vec<_>, AppError>>()?;
    let stats = match spec.kind {
        MongoCollectionKind::View => None,
        _ => storage_stats(&client.database(db), coll).await,
    };
    Ok(TableProperties {
        indexes,
        constraints: Vec::new(),
        foreign_keys: Vec::new(),
        references: Vec::new(),
        mongo: Some(MongoCollectionInfo {
            kind: spec.kind,
            validator: validator.map(|v| ejson_pretty(Bson::Document(v))),
            validation_level,
            validation_action,
            options: ejson_pretty(Bson::Document(options)),
            stats,
        }),
    })
}

/// One `listIndexes` entry as the properties view shows it.
fn index_info(coll: &str, spec: &Document) -> Result<IndexInfo, AppError> {
    let key = spec.get_document("key").ok().cloned().unwrap_or_default();
    let name = spec.get_str("name").unwrap_or_default().to_string();
    let options = index_options(spec);
    // The definition is mongosh text, so a regex or a date option reads back as itself.
    let key_text = shell::compact(&Bson::Document(key.clone()))?;
    let definition = if options.is_empty() {
        format!(
            "db.getCollection({}).createIndex({key_text})",
            shell::quoted(coll)
        )
    } else {
        format!(
            "db.getCollection({}).createIndex({key_text}, {})",
            shell::quoted(coll),
            shell::compact(&Bson::Document(options))?
        )
    };
    // Everything the fields above already say, and `v` (the server's index format version),
    // is not an option the user set.
    let mut rest = spec.clone();
    for field in [
        "v",
        "key",
        "name",
        "unique",
        "sparse",
        "hidden",
        "expireAfterSeconds",
        "partialFilterExpression",
    ] {
        rest.remove(field);
    }
    Ok(IndexInfo {
        name: name.clone(),
        columns: key.keys().cloned().collect(),
        unique: spec.get_bool("unique").unwrap_or(false),
        primary: name == "_id_",
        method: index_method(&key),
        definition: Some(definition),
        mongo: Some(MongoIndexInfo {
            keys: ejson(Bson::Document(key)),
            sparse: spec.get_bool("sparse").unwrap_or(false),
            hidden: spec.get_bool("hidden").unwrap_or(false),
            ttl_seconds: spec.get("expireAfterSeconds").and_then(number),
            partial_filter: spec
                .get_document("partialFilterExpression")
                .ok()
                .map(|filter| ejson(Bson::Document(filter.clone()))),
            options: (!rest.is_empty()).then(|| ejson(Bson::Document(rest))),
        }),
    })
}

/// The index options that `createIndex` accepts, in the order the script writes them. Every
/// option here changes the index, so a script that leaves one out builds a different index; the
/// rest of a `listIndexes` entry is a field of its own (`v`, `ns`, `key`) or a field the shape
/// of the entry already reports (`unique`, `sparse`, ...).
pub(crate) fn index_options(spec: &Document) -> Document {
    const OPTIONS: [&str; 17] = [
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
    ];
    let mut options = Document::new();
    for key in OPTIONS {
        if let Some(value) = spec.get(key) {
            options.insert(key, value.clone());
        }
    }
    options
}

/// The special index types of a key document, joined by ", "; absent for plain 1/-1 indexes.
fn index_method(key: &Document) -> Option<String> {
    let mut kinds: Vec<&str> = Vec::new();
    for (field, value) in key {
        let kind = match value {
            Bson::String(kind)
                if matches!(kind.as_str(), "text" | "2dsphere" | "2d" | "hashed") =>
            {
                kind.as_str()
            }
            _ if field == "$**" || field.ends_with(".$**") => "wildcard",
            _ => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    (!kinds.is_empty()).then(|| kinds.join(", "))
}

/// The `storageStats` of `coll`; `None` when the server refuses it, as a user without the
/// `collStats` privilege gets, or for a shape sandb does not know.
async fn storage_stats(db: &Database, coll: &str) -> Option<MongoCollectionStats> {
    // `$collStats` replaces the deprecated `collStats` command (6.2+).
    let mut cursor = db
        .collection::<Document>(coll)
        .aggregate([doc! { "$collStats": { "storageStats": {} } }])
        .await
        .ok()?;
    let doc = cursor.try_next().await.ok().flatten()?;
    let stats = doc.get_document("storageStats").ok()?;
    let int = |key: &str| stats.get(key).and_then(number).unwrap_or(0);
    let avg_obj_size = match stats.get("avgObjSize") {
        Some(Bson::Double(avg)) => *avg,
        Some(value) => number(value).map_or(0.0, |n| n as f64),
        None => 0.0,
    };
    Some(MongoCollectionStats {
        count: int("count"),
        size: int("size"),
        avg_obj_size,
        storage_size: int("storageSize"),
        total_index_size: int("totalIndexSize"),
        nindexes: int("nindexes"),
    })
}

/// The documents of the first batch of a cursor reply.
fn first_batch(reply: &Document) -> Result<Vec<Document>, AppError> {
    let batch = reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .map_err(|e| {
            AppError::internal("The server reply has no cursor").with_detail(e.to_string())
        })?;
    Ok(batch
        .iter()
        .filter_map(|v| v.as_document().cloned())
        .collect())
}

/// A whole number of any BSON numeric type.
fn number(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.is_finite() && d.fract() == 0.0 => Some(*d as i64),
        _ => None,
    }
}

fn into_document(v: Bson) -> Option<Document> {
    match v {
        Bson::Document(d) => Some(d),
        _ => None,
    }
}

fn into_string(v: Bson) -> Option<String> {
    match v {
        Bson::String(s) => Some(s),
        _ => None,
    }
}

/// A value as compact relaxed Extended JSON.
fn ejson(v: Bson) -> String {
    v.into_relaxed_extjson().to_string()
}

/// A value as pretty (2-space) relaxed Extended JSON.
fn ejson_pretty(v: Bson) -> String {
    serde_json::to_string_pretty(&v.into_relaxed_extjson()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{Bson, Document, doc};

    use super::{index_method, index_options};

    fn keys(pairs: &[(&str, Bson)]) -> Document {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn the_method_names_the_special_index_types() {
        let one = |pairs: &[(&str, Bson)]| index_method(&keys(pairs));
        assert_eq!(one(&[("a", 1.into()), ("b", (-1).into())]), None);
        assert_eq!(one(&[("title", "text".into())]).as_deref(), Some("text"));
        assert_eq!(
            one(&[("loc", "2dsphere".into()), ("g", "2d".into())]).as_deref(),
            Some("2dsphere, 2d")
        );
        assert_eq!(one(&[("h", "hashed".into())]).as_deref(), Some("hashed"));
        // The whole-document wildcard and the single-field path wildcard.
        assert_eq!(one(&[("$**", 1.into())]).as_deref(), Some("wildcard"));
        assert_eq!(
            one(&[("attributes.$**", 1.into())]).as_deref(),
            Some("wildcard")
        );
        assert_eq!(
            one(&[("$**", 1.into()), ("a.$**", 1.into())]).as_deref(),
            Some("wildcard")
        );
    }

    #[test]
    fn every_creation_option_is_kept_in_order() {
        let spec = doc! {
            "v": 2,
            "key": { "a": 1 },
            "name": "a_1",
            "unique": true,
            "sparse": true,
            "hidden": true,
            "expireAfterSeconds": 60,
            "partialFilterExpression": { "a": { "$gt": 1 } },
            "collation": { "locale": "fr" },
            "weights": { "title": 5 },
            "default_language": "spanish",
            "wildcardProjection": { "a.b": 1 },
            "language_override": "lang",
            "textIndexVersion": 3,
            "2dsphereIndexVersion": 3,
            "bits": 30,
            "min": -180.0,
            "max": 180.0,
            "storageEngine": { "wiredTiger": {} },
            "ns": "db.c",
        };
        let options = index_options(&spec);
        let names: Vec<&str> = options.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
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
            ]
        );
        assert_eq!(
            options.get("wildcardProjection"),
            spec.get("wildcardProjection")
        );
        assert_eq!(
            options.get("language_override"),
            spec.get("language_override")
        );
        assert!(!options.contains_key("v") && !options.contains_key("ns"));
        assert!(!options.contains_key("key"));
    }
}
