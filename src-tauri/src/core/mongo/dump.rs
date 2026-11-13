//! A MongoDB collection as a mongosh script: the creation of the collection with its options,
//! validator and indexes, and, when asked, one `insertMany` per batch of documents.
//!
//! The script runs in the sandb editor. Values go through `value`, so a document comes back as
//! the document it was, type by type. The documents stream from the server: the collection is
//! never held in memory.

use std::path::Path;

use mongodb::Client;
use mongodb::bson::Bson;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::core::connections::{Connection, target_database};
use crate::core::error::AppError;
use crate::core::export_file::ExportFile;
use crate::core::mongo::command::{Command, Find, Method};
use crate::core::mongo::introspect::{self, CollectionSpec};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::value;
use crate::core::mongo::{client, run};
use crate::core::mongo::client::Access;
use crate::core::result::{ExportResult, MongoCollectionKind, TableExportRequest};

/// Documents per `insertMany` statement.
const INSERT_BATCH: usize = 1000;
/// Text per `insertMany` statement. A single larger document still gets its own statement.
const INSERT_BYTES: usize = 4 * 1024 * 1024;

pub async fn collection_ddl(client: &Client, db: &str, coll: &str) -> Result<String, AppError> {
    let spec = introspect::spec(client, db, coll).await?;
    ddl_text(coll, &spec)
}

/// `db.createCollection` (a view keeps `viewOn` and `pipeline` in its options), then one
/// `createIndex` per index but `_id_`, whose index the collection comes with.
fn ddl_text(coll: &str, spec: &CollectionSpec) -> Result<String, AppError> {
    let name = value::quoted(coll);
    let options = value::compact(&Bson::Document(spec.options.clone()))?;
    let mut out = format!("// sandb MongoDB export: collection {name}\n");
    out.push_str(&format!("db.createCollection({name}, {options});\n"));
    for index in &spec.indexes {
        if index.get_str("name").is_ok_and(|name| name == "_id_") {
            continue;
        }
        out.push_str(&format!(
            "{};\n",
            introspect::create_index_text(coll, index)?
        ));
    }
    Ok(out)
}

pub async fn export_collection(
    access: &impl Access,
    conn: &Connection,
    req: TableExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let database = target_database(conn, Some(&req.database));
    let client = client::client(access, &conn.id).await?;
    let spec = introspect::spec(&client, &database, &req.table).await?;
    let ddl = ddl_text(&req.table, &spec)?;
    let with_data = req.data && spec.kind != MongoCollectionKind::View;
    let mut file =
        ExportFile::for_query(path, access.queries(), &conn.id, &database, conn.engine).await?;
    let id = file
        .query_id()
        .expect("for_query registers a run")
        .to_string();
    let comment = OpComment::new(&id);
    let rows = {
        let out = file.out();
        out.write_all(ddl.as_bytes()).await?;
        if with_data {
            write_documents(&client, &database, &req.table, out, comment).await?
        } else {
            0
        }
    };
    file.finish(rows).await
}

async fn write_documents<W>(
    client: &Client,
    database: &str,
    coll: &str,
    out: &mut W,
    comment: OpComment<'_>,
) -> Result<u64, AppError>
where
    W: AsyncWrite + Unpin,
{
    let cmd = Command::Collection {
        collection: coll.to_string(),
        method: Method::Find(Find::default()),
    };
    let mut stream = run::open_read(
        client,
        database,
        &cmd,
        None,
        comment,
        Some(INSERT_BATCH as u32),
        None,
    )
    .await?;
    let head = format!("db.getCollection({}).insertMany([", value::quoted(coll));
    let mut batch: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut rows = 0u64;
    while let Some((doc, _)) = stream.next().await? {
        let text = value::document(&doc)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mongo::command::Method;
    use crate::core::mongo::parse;

    const HEAD: &str = "db.getCollection(\"t\").insertMany([";

    /// A batch becomes one `insertMany` statement, and that statement parses back as the write
    /// it is.
    #[tokio::test]
    async fn flush_writes_one_insert_many_and_clears_the_batch() {
        let mut out: Vec<u8> = Vec::new();
        let mut batch = vec!["{ a: 1 }".to_string(), "{ b: 2 }".to_string()];
        let mut bytes: usize = batch.iter().map(String::len).sum();
        flush(&mut out, HEAD, &mut batch, &mut bytes)
            .await
            .unwrap();
        assert!(batch.is_empty() && bytes == 0);
        let text = String::from_utf8(out).unwrap();
        let stmts = parse::parse(&text).unwrap();
        assert_eq!(stmts.len(), 1, "{text}");
        match &stmts[0].command {
            Command::Collection {
                collection,
                method: Method::InsertMany { documents, .. },
            } => {
                assert_eq!(collection, "t");
                assert_eq!(documents.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_batch_writes_nothing() {
        let mut out: Vec<u8> = Vec::new();
        let mut batch: Vec<String> = Vec::new();
        let mut bytes = 0;
        flush(&mut out, HEAD, &mut batch, &mut bytes)
            .await
            .unwrap();
        assert!(out.is_empty());
    }
}
