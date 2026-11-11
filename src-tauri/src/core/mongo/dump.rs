//! A MongoDB collection as a mongosh script: the creation of the collection with its options,
//! validator and indexes, and, when asked, one `insertMany` per batch of documents.
//!
//! The script runs in the sandb editor. Values go through `shell`, so a document comes back as
//! the document it was, type by type. The documents stream from the server: the collection is
//! never held in memory.

use std::path::Path;

use mongodb::Client;
use mongodb::bson::Bson;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use crate::core::connections::{self, Connection};
use crate::core::db::dump::TableExportRequest;
use crate::core::db::export::{self, ExportResult};
use crate::core::db::introspect::MongoCollectionKind;
use crate::core::error::AppError;
use crate::core::mongo::command::{Command, Find, Method};
use crate::core::mongo::introspect::{self, CollectionSpec};
use crate::core::mongo::{client, run, shell};
use crate::core::state::AppState;

/// Documents per `insertMany` statement.
const INSERT_BATCH: usize = 1000;
/// Text per `insertMany` statement. A single larger document still gets its own statement.
const INSERT_BYTES: usize = 4 * 1024 * 1024;

/// The mongosh text that creates `coll` with its options, validator and indexes.
pub async fn collection_ddl(client: &Client, db: &str, coll: &str) -> Result<String, AppError> {
    let spec = introspect::spec(client, db, coll).await?;
    ddl_text(coll, &spec)
}

/// `db.createCollection` (a view keeps `viewOn` and `pipeline` in its options), then one
/// `createIndex` per index but `_id_`, whose index the collection comes with.
fn ddl_text(coll: &str, spec: &CollectionSpec) -> Result<String, AppError> {
    let name = shell::quoted(coll);
    let options = shell::compact(&Bson::Document(spec.options.clone()))?;
    let mut out = format!("// sandb MongoDB export: collection {name}\n");
    out.push_str(&format!("db.createCollection({name}, {options});\n"));
    for index in &spec.indexes {
        if index.get_str("name").is_ok_and(|name| name == "_id_") {
            continue;
        }
        let Ok(key) = index.get_document("key") else {
            continue;
        };
        let key = shell::compact(&Bson::Document(key.clone()))?;
        let options = introspect::index_options(index);
        if options.is_empty() {
            out.push_str(&format!("db.getCollection({name}).createIndex({key});\n"));
        } else {
            let options = shell::compact(&Bson::Document(options))?;
            out.push_str(&format!(
                "db.getCollection({name}).createIndex({key}, {options});\n"
            ));
        }
    }
    Ok(out)
}

/// Writes the script of `req.table` and, when `req.data` and it is not a view, one `insertMany`
/// statement per batch to a temp file next to `path`, then renames it over `path`.
pub async fn export_collection(
    state: &AppState,
    conn: &Connection,
    req: TableExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    if !path.is_absolute() {
        return Err(AppError::invalid("Choose a file to export to"));
    }
    let database = connections::target_database(conn, Some(&req.database));
    let client = client::client(state, &conn.id).await?;
    let spec = introspect::spec(&client, &database, &req.table).await?;
    let ddl = ddl_text(&req.table, &spec)?;
    let with_data = req.data && spec.kind != MongoCollectionKind::View;
    let tmp = export::temp_path(path);
    let written = async {
        let file = tokio::fs::File::create(&tmp).await?;
        let mut out = BufWriter::new(file);
        out.write_all(ddl.as_bytes()).await?;
        let rows = if with_data {
            write_documents(&client, &database, &req.table, &mut out).await?
        } else {
            0
        };
        out.flush().await?;
        out.get_ref().sync_all().await?;
        Ok::<u64, AppError>(rows)
    }
    .await;
    let res = match written {
        Ok(rows) => tokio::fs::rename(&tmp, path)
            .await
            .map(|()| rows)
            .map_err(AppError::from),
        Err(e) => Err(e),
    };
    match res {
        Ok(rows) => Ok(ExportResult {
            cancelled: false,
            rows,
            path: path.display().to_string(),
        }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

/// One `insertMany` statement per `INSERT_BATCH` documents or `INSERT_BYTES` of text.
async fn write_documents<W>(
    client: &Client,
    database: &str,
    coll: &str,
    out: &mut W,
) -> Result<u64, AppError>
where
    W: AsyncWrite + Unpin,
{
    let cmd = Command::Collection {
        collection: coll.to_string(),
        method: Method::Find(Find::default()),
    };
    let comment = uuid::Uuid::new_v4().to_string();
    let mut stream = run::open_read(
        client,
        database,
        &cmd,
        None,
        &comment,
        Some(INSERT_BATCH as u32),
    )
    .await?;
    let head = format!("db.getCollection({}).insertMany([", shell::quoted(coll));
    let mut batch: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut rows = 0u64;
    while let Some((doc, _)) = stream.next().await? {
        let text = shell::document(&doc)?;
        bytes += text.len();
        batch.push(text);
        rows += 1;
        if batch.len() >= INSERT_BATCH || bytes >= INSERT_BYTES {
            flush(out, &head, &mut batch, &mut bytes).await?;
        }
    }
    flush(out, &head, &mut batch, &mut bytes).await?;
    Ok(rows)
}

async fn flush<W>(
    out: &mut W,
    head: &str,
    batch: &mut Vec<String>,
    bytes: &mut usize,
) -> Result<(), AppError>
where
    W: AsyncWrite + Unpin,
{
    if batch.is_empty() {
        return Ok(());
    }
    let mut text = String::with_capacity(head.len() + *bytes + batch.len() + 4);
    text.push_str(head);
    text.push_str(&batch.join(","));
    text.push_str("]);\n");
    out.write_all(text.as_bytes()).await?;
    batch.clear();
    *bytes = 0;
    Ok(())
}
