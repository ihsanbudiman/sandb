//! Export of one MongoDB read, as CSV with the grid's flattened columns or as Extended JSON.

use std::collections::HashMap;
use std::path::Path;

use mongodb::bson::Bson;
use tokio::io::AsyncWriteExt;

use crate::core::cell::line;
use crate::core::connections::{target_database, Connection};
use crate::core::error::AppError;
use crate::core::export_file::ExportFile;
use crate::core::mongo::client::Access;
use crate::core::mongo::command::{Command, Statement};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::flatten::for_each_cell;
use crate::core::mongo::guard::{self, Kind};
use crate::core::mongo::value;
use crate::core::mongo::{client, parse, run};
use crate::core::result::{
    DocumentExportRequest, DocumentFormat, EjsonFlavor, ExportRequest, ExportResult,
};

pub fn check(stmts: &[Statement]) -> Result<(), AppError> {
    match stmts {
        [s] if !matches!(s.command, Command::Use(_)) && guard::kind(&s.command) == Kind::Read => {
            Ok(())
        }
        _ => Err(AppError::invalid("Export needs exactly one read statement")),
    }
}

pub async fn export_csv(
    access: &impl Access,
    conn: &Connection,
    req: ExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let stmts = parse::parse(&req.sql)?;
    check(&stmts)?;
    let cmd = &stmts[0].command;
    let database = target_database(conn, req.database.as_deref());
    let client = client::client(access, &conn.id).await?;
    let mut file =
        ExportFile::for_query(path, access.queries(), &conn.id, &database, conn.engine).await?;
    let id = file
        .query_id()
        .expect("for_query registers a run")
        .to_string();
    let comment = OpComment::new(&id);

    let mut columns: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut stream = run::open_read(&client, &database, cmd, None, comment, None, None).await?;
    while let Some((doc, _)) = stream.next().await? {
        for_each_cell(&doc, &mut |path, _| {
            if !index.contains_key(path) {
                index.insert(path.to_string(), columns.len());
                columns.push(path.to_string());
            }
        });
    }

    let rows = {
        let out = file.out();
        out.write_all(line(columns.iter().map(String::as_str)).as_bytes())
            .await?;
        // A field that appeared between the passes has no column and is left out.
        let mut rows = 0u64;
        let mut stream = run::open_read(&client, &database, cmd, None, comment, None, None).await?;
        let mut row = vec![String::new(); columns.len()];
        while let Some((doc, _)) = stream.next().await? {
            row.iter_mut().for_each(String::clear);
            for_each_cell(&doc, &mut |path, v| {
                if let Some(&i) = index.get(path) {
                    row[i] = value::csv(v);
                }
            });
            out.write_all(line(row.iter().map(String::as_str)).as_bytes())
                .await?;
            rows += 1;
        }
        rows
    };
    file.finish(rows).await
}

/// A JSON array is one document per line between `[` and `]`; NDJSON puts one compact document
/// on every line. Neither form holds more than one document.
pub async fn export_documents(
    access: &impl Access,
    conn: &Connection,
    req: DocumentExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let stmts = parse::parse(&req.query.sql)?;
    check(&stmts)?;
    let cmd = &stmts[0].command;
    let database = target_database(conn, req.query.database.as_deref());
    let client = client::client(access, &conn.id).await?;
    let mut file =
        ExportFile::for_query(path, access.queries(), &conn.id, &database, conn.engine).await?;
    let id = file
        .query_id()
        .expect("for_query registers a run")
        .to_string();
    let comment = OpComment::new(&id);
    let array = req.format == DocumentFormat::Json;
    let rows = {
        let out = file.out();
        let mut stream = run::open_read(&client, &database, cmd, None, comment, None, None).await?;
        if array {
            out.write_all(b"[").await?;
        }
        let mut rows = 0u64;
        while let Some((doc, _)) = stream.next().await? {
            let value = match req.ejson {
                EjsonFlavor::Relaxed => Bson::Document(doc).into_relaxed_extjson(),
                EjsonFlavor::Canonical => Bson::Document(doc).into_canonical_extjson(),
            };
            if array {
                out.write_all(if rows == 0 { b"\n" } else { b",\n" }).await?;
            }
            out.write_all(value.to_string().as_bytes()).await?;
            if !array {
                out.write_all(b"\n").await?;
            }
            rows += 1;
        }
        if array {
            out.write_all(if rows == 0 { b"]\n" } else { b"\n]\n" })
                .await?;
        }
        rows
    };
    file.finish(rows).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorCode;
    use crate::core::mongo::parse;

    /// Export takes exactly one read: the statement that produced the grid.
    #[test]
    fn export_accepts_exactly_one_read() {
        let check_text = |sql: &str| check(&parse::parse(sql).unwrap());
        assert!(check_text("db.orders.find({ status: 'open' })").is_ok());
        for sql in [
            "db.orders.find({}); db.orders.find({})",
            "db.orders.drop()",
            "use shop",
            "db.orders.insertOne({ a: 1 })",
            "db.orders.createIndex({ a: 1 })",
        ] {
            let e = check_text(sql).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{sql}");
        }
    }
}
