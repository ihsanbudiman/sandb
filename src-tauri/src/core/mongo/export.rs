//! CSV export of one MongoDB read, with the grid's flattened columns. Documents have no fixed
//! columns, so the read runs twice: the first pass only collects the column paths, the second
//! writes the rows. Neither pass keeps the documents.

use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mongodb::bson::Bson;
use mongodb::bson::spec::BinarySubtype;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::core::connections::{self, Connection, Engine};
use crate::core::db::export::{
    DocumentExportRequest, DocumentFormat, EjsonFlavor, ExportRequest, ExportResult, field, line,
    temp_path,
};
use crate::core::error::AppError;
use crate::core::mongo::command::{Command, Statement};
use crate::core::mongo::flatten::{cell, for_each_cell};
use crate::core::mongo::guard::{self, Kind};
use crate::core::mongo::{client, parse, run};
use crate::core::state::AppState;

/// Exactly one read that returns documents, so the export never repeats a write.
pub fn check(stmts: &[Statement]) -> Result<(), AppError> {
    match stmts {
        [s] if !matches!(s.command, Command::Use(_)) && guard::kind(&s.command) == Kind::Read => {
            Ok(())
        }
        _ => Err(AppError::invalid(
            "Export needs exactly one read (find or aggregate)",
        )),
    }
}

/// Streams the read as CSV to a temp file next to `path`, then renames it over `path`.
pub async fn export_csv(
    state: &AppState,
    conn: &Connection,
    req: ExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let stmts = parse::parse(&req.sql)?;
    check(&stmts)?;
    if !path.is_absolute() {
        return Err(AppError::invalid("Choose a file to export to"));
    }
    let cmd = &stmts[0].command;
    let database = connections::target_database(conn, req.database.as_deref());
    let client = client::client(state, &conn.id).await?;
    let comment = uuid::Uuid::new_v4().to_string();

    // Pass 1: the columns, in order of first appearance.
    let mut columns: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut stream = run::open_read(&client, &database, cmd, None, &comment, None).await?;
    while let Some((doc, _)) = stream.next().await? {
        for_each_cell(&doc, &mut |path, _| {
            if !index.contains_key(path) {
                index.insert(path.to_string(), columns.len());
                columns.push(path.to_string());
            }
        });
    }

    let tmp = temp_path(path);
    let written = async {
        let mut out = BufWriter::new(tokio::fs::File::create(&tmp).await?);
        out.write_all(line(columns.iter().map(String::as_str)).as_bytes())
            .await?;
        // Pass 2: the rows. A field that appeared between the passes has no column and is left out.
        let mut rows = 0u64;
        let mut stream = run::open_read(&client, &database, cmd, None, &comment, None).await?;
        let mut row = vec![String::new(); columns.len()];
        while let Some((doc, _)) = stream.next().await? {
            row.iter_mut().for_each(String::clear);
            for_each_cell(&doc, &mut |path, value| {
                if let Some(&i) = index.get(path) {
                    row[i] = csv_value(value);
                }
            });
            out.write_all(line(row.iter().map(String::as_str)).as_bytes())
                .await?;
            rows += 1;
        }
        out.flush().await?;
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

/// The full value of one cell: text and binary uncut, arrays and summary documents as compact
/// relaxed Extended JSON, everything else as the grid shows it.
fn csv_value(v: &Bson) -> String {
    match v {
        Bson::String(s) | Bson::Symbol(s) => s.clone(),
        Bson::JavaScriptCode(code) => code.clone(),
        Bson::Array(_) | Bson::Document(_) => v.clone().into_relaxed_extjson().to_string(),
        Bson::Binary(b) if b.subtype != BinarySubtype::Uuid => B64.encode(&b.bytes),
        _ => field(&cell(v)),
    }
}

/// Streams the read as Extended JSON to a temp file next to `path`, then renames it over `path`.
/// A JSON array is one document per line between `[` and `]`; NDJSON puts one compact document
/// on every line. Neither form holds more than one document.
pub async fn export_documents(
    state: &AppState,
    conn: &Connection,
    req: DocumentExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    if conn.engine != Engine::Mongo {
        return Err(AppError::invalid(
            "Extended JSON export needs a MongoDB connection",
        ));
    }
    let stmts = parse::parse(&req.query.sql)?;
    check(&stmts)?;
    if !path.is_absolute() {
        return Err(AppError::invalid("Choose a file to export to"));
    }
    let cmd = &stmts[0].command;
    let database = connections::target_database(conn, req.query.database.as_deref());
    let client = client::client(state, &conn.id).await?;
    let comment = uuid::Uuid::new_v4().to_string();
    let array = req.format == DocumentFormat::Json;
    let tmp = temp_path(path);
    let written = async {
        let mut out = BufWriter::new(tokio::fs::File::create(&tmp).await?);
        let mut stream = run::open_read(&client, &database, cmd, None, &comment, None).await?;
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
                out.write_all(if rows == 0 { b"\n" } else { b",\n" })
                    .await?;
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
        out.flush().await?;
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
