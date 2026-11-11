//! Runs the mongosh statements of an editor run. Reads stream from the server and stop at the
//! row limit or the page byte limit, so a large collection or large documents never land in
//! memory whole. Every server operation except a collection `drop` (the driver has no `comment`
//! option for it) carries the query id as `comment`, which is how a cancel finds and stops it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mongodb::bson::{Bson, Document, RawDocumentBuf, doc};
use mongodb::options::{Collation, Hint};
use mongodb::results::CollectionType;
use mongodb::{Client, Cursor};

use crate::core::connections::{self, Connection};
use crate::core::db::query::{self, QueryRun, RunRequest, RunSort, Session, StatementResult};
use crate::core::error::AppError;
use crate::core::mongo::command::{Command, Method, Statement};
use crate::core::mongo::flatten::{Flattener, Page};
use crate::core::mongo::guard::{self, Kind};
use crate::core::mongo::{client, introspect, parse, write};
use crate::core::state::AppState;

/// Byte limit of one read result: the sum of the BSON sizes of its documents.
pub const PAGE_BYTES: usize = 16 * 1024 * 1024;
/// How long a cancelled write may still answer, so the run can tell whether it applied.
const WRITE_REPLY_WAIT: Duration = Duration::from_secs(2);

/// `query::run` for a MongoDB connection.
pub async fn run(
    state: &AppState,
    conn: &Connection,
    req: RunRequest,
    check: bool,
) -> Result<QueryRun, AppError> {
    let stmts = parse::parse(&req.sql)?;
    if stmts.is_empty() {
        return Err(AppError::invalid("Nothing to run"));
    }
    // No session can make MongoDB read-only, so this check is the app's barrier; a user with
    // the `read` role is the server's.
    if check && conn.read_only {
        guard::check_read_only(conn, &stmts)?;
    }
    if req.sort.is_some() && !(stmts.len() == 1 && sortable(&stmts[0].command)) {
        return Err(AppError::invalid(
            "Sorting requires exactly one find or aggregate",
        ));
    }
    let mut database = connections::target_database(conn, req.database.as_deref());
    let limit = query::clamp_limit(req.limit);
    let started = Instant::now();
    let run = state
        .queries
        .register(&req.query_id, &conn.id, &database, stmts.len())?;
    let cancelled = run.cancelled.clone();
    if cancelled.load(Ordering::SeqCst) {
        return Err(query::cancelled_error());
    }
    let client = client::client(state, &conn.id).await?;
    let lone = stmts.len() == 1;
    let mut results = Vec::with_capacity(stmts.len());
    let mut notices = Vec::new();
    for s in &stmts {
        let t0 = Instant::now();
        if let Command::Use(name) = &s.command {
            database = name.clone();
            notices.push(format!("Using database {name} for the rest of this run"));
            let mut r = empty_result(&s.text, true);
            r.database = Some(database.clone());
            results.push(r);
        } else {
            if database.is_empty() {
                return Err(AppError::invalid("Choose a database for this tab first"));
            }
            let session = state.queries.attach(&req.query_id, Session::Mongo);
            // The driver can wait long on a server that does not answer (up to its monitor
            // timeout), and the kill cannot reach that server either. So a cancel also stops
            // the wait here; the server kill stays best effort.
            let work = statement(
                &client,
                &database,
                s,
                &req,
                limit,
                lone,
                &cancelled,
                &mut notices,
            );
            tokio::pin!(work);
            let write = !matches!(guard::kind(&s.command), Kind::Read);
            let n = results.len() + 1;
            let r = tokio::select! {
                r = &mut work => r,
                () = until_cancelled(&cancelled) => {
                    if !write {
                        return Err(query::cancelled_error());
                    }
                    // A write that reached the server can still apply. Wait a moment for its
                    // reply (the kill runs meanwhile), so the user learns what happened.
                    match tokio::time::timeout(WRITE_REPLY_WAIT, &mut work).await {
                        Ok(r) => r,
                        Err(_) => {
                            return Err(query::cancelled_error().with_detail(format!(
                                "Statement {n} may have changed data before the cancel. Reload to check."
                            )));
                        }
                    }
                }
            };
            drop(session);
            // A killed operation fails with the server's "interrupted" error.
            if cancelled.load(Ordering::SeqCst) {
                let e = query::cancelled_error();
                return Err(if write && r.is_ok() {
                    e.with_detail(format!(
                        "Statement {n} finished before the cancel, so its change stays."
                    ))
                } else {
                    e
                });
            }
            let mut r = r?;
            r.elapsed_ms = t0.elapsed().as_millis() as u64;
            r.database = Some(database.clone());
            results.push(r);
        }
        run.done.fetch_add(1, Ordering::SeqCst);
    }
    drop(run);
    Ok(QueryRun {
        statements: results,
        notices,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Ends when the run's cancel flag is set.
async fn until_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn sortable(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Collection {
            method: Method::Find(_) | Method::Aggregate { .. },
            ..
        }
    )
}

/// Statements that `write::execute` runs; everything else reads through `open_read`.
pub(crate) fn is_write(cmd: &Command) -> bool {
    match cmd {
        Command::CreateCollection { .. } | Command::DropDatabase { .. } => true,
        Command::Collection { method, .. } => matches!(
            method,
            Method::InsertOne { .. }
                | Method::InsertMany { .. }
                | Method::UpdateOne { .. }
                | Method::UpdateMany { .. }
                | Method::ReplaceOne { .. }
                | Method::DeleteOne { .. }
                | Method::DeleteMany { .. }
                | Method::FindOneAndUpdate { .. }
                | Method::FindOneAndReplace { .. }
                | Method::FindOneAndDelete { .. }
                | Method::BulkWrite { .. }
                | Method::CreateIndex { .. }
                | Method::CreateIndexes { .. }
                | Method::DropIndex { .. }
                | Method::DropIndexes
                | Method::Drop
                | Method::RenameCollection { .. }
        ),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn statement(
    client: &Client,
    database: &str,
    s: &Statement,
    req: &RunRequest,
    limit: usize,
    lone: bool,
    cancelled: &AtomicBool,
    notices: &mut Vec<String>,
) -> Result<StatementResult, AppError> {
    if is_write(&s.command) {
        let out = write::execute(client, database, &s.command, &req.query_id).await?;
        notices.push(out.notice);
        let mut r = match out.document {
            Some(d) => {
                let mut f = Flattener::default();
                f.push(d);
                page_result(&s.text, f.finish(), false, false)
            }
            None => empty_result(&s.text, false),
        };
        r.is_read = false;
        r.affected = Some(out.affected);
        return Ok(r);
    }
    // The page size plus the one document that tells whether more exist.
    let batch = u32::try_from(limit + 1).ok();
    let mut stream = open_read(
        client,
        database,
        &s.command,
        req.sort.as_ref(),
        &req.query_id,
        batch,
    )
    .await?;
    let mut f = Flattener::default();
    let mut bytes = 0usize;
    let mut truncated = false;
    while let Some((doc, size)) = stream.next().await? {
        if cancelled.load(Ordering::SeqCst) {
            return Err(query::cancelled_error());
        }
        if f.len() == limit {
            truncated = true;
            break;
        }
        if !f.is_empty() && bytes + size > PAGE_BYTES {
            truncated = true;
            notices.push(format!(
                "Stopped at the 16 MB page limit after {} documents. Use a projection or a smaller limit.",
                f.len()
            ));
            break;
        }
        bytes += size;
        f.push(doc);
    }
    if f.is_empty() {
        notices.push(format!("{}: no documents", label(&s.command)));
    }
    let mut r = page_result(&s.text, f.finish(), truncated, lone && sortable(&s.command));
    r.is_read = guard::kind(&s.command) == Kind::Read;
    Ok(r)
}

fn label(cmd: &Command) -> String {
    match cmd {
        Command::Collection { collection, method } => format!("{} on {collection}", method.name()),
        Command::ShowDbs => "show dbs".into(),
        Command::ShowCollections => "show collections".into(),
        Command::RunCommand { .. } => "runCommand".into(),
        Command::Use(_) => "use".into(),
        Command::CreateCollection { .. } => "createCollection".into(),
        Command::DropDatabase { .. } => "dropDatabase".into(),
    }
}

fn page_result(text: &str, page: Page, truncated: bool, sortable: bool) -> StatementResult {
    StatementResult {
        sql: text.to_string(),
        row_count: page.rows.len(),
        columns: page.columns,
        rows: page.rows,
        truncated,
        affected: None,
        is_read: true,
        sortable,
        elapsed_ms: 0,
        documents: Some(page.documents),
        cell_types: Some(page.cell_types),
        database: None,
    }
}

fn empty_result(text: &str, is_read: bool) -> StatementResult {
    StatementResult {
        sql: text.to_string(),
        columns: Vec::new(),
        rows: Vec::new(),
        row_count: 0,
        truncated: false,
        affected: None,
        is_read,
        sortable: false,
        elapsed_ms: 0,
        documents: None,
        cell_types: None,
        database: None,
    }
}

// ---- reads

/// Documents of one read statement, streamed from the server or from a single reply.
pub(crate) struct ReadStream {
    source: Source,
}

enum Source {
    Cursor(Box<Cursor<RawDocumentBuf>>),
    Docs(VecDeque<Document>),
}

impl ReadStream {
    pub(crate) fn docs(docs: Vec<Document>) -> Self {
        ReadStream {
            source: Source::Docs(docs.into()),
        }
    }

    /// The next document and its BSON size in bytes.
    pub(crate) async fn next(&mut self) -> Result<Option<(Document, usize)>, AppError> {
        match &mut self.source {
            Source::Cursor(c) => {
                if !c.advance().await? {
                    return Ok(None);
                }
                let raw = c.current();
                let doc = Document::try_from(raw).map_err(|e| {
                    AppError::internal("Cannot read a document").with_detail(e.to_string())
                })?;
                Ok(Some((doc, raw.as_bytes().len())))
            }
            Source::Docs(d) => Ok(d.pop_front().map(|doc| {
                let size = RawDocumentBuf::from_document(&doc).map_or(0, |r| r.as_bytes().len());
                (doc, size)
            })),
        }
    }
}

/// Opens a read: the user's own skip, limit, sort and `maxTimeMS` apply; the caller applies the
/// page limits. `sort` (a header click) replaces the find sort or adds a `$sort` stage to the
/// pipeline. `batch` sizes the server batches of a cursor.
pub(crate) async fn open_read(
    client: &Client,
    database: &str,
    cmd: &Command,
    sort: Option<&RunSort>,
    comment: &str,
    batch: Option<u32>,
) -> Result<ReadStream, AppError> {
    let db = client.database(database);
    let tag = Bson::String(comment.to_string());
    if sort.is_some() && !sortable(cmd) {
        return Err(AppError::invalid(
            "Sorting requires exactly one find or aggregate",
        ));
    }
    match cmd {
        Command::ShowDbs => {
            let names = introspect::databases(client, database).await?;
            Ok(ReadStream::docs(
                names.into_iter().map(|n| doc! { "name": n }).collect(),
            ))
        }
        Command::ShowCollections => {
            let mut c = db
                .list_collections()
                .authorized_collections(true)
                .comment(tag)
                .await?;
            let mut out = Vec::new();
            while c.advance().await? {
                let spec = c.deserialize_current()?;
                let kind = match spec.collection_type {
                    CollectionType::View => "view",
                    CollectionType::Timeseries => "timeseries",
                    _ => "collection",
                };
                out.push(doc! { "name": spec.name, "type": kind });
            }
            out.sort_by(|a, b| a.get_str("name").ok().cmp(&b.get_str("name").ok()));
            Ok(ReadStream::docs(out))
        }
        Command::RunCommand { admin, command } => {
            let target = if *admin { client.database("admin") } else { db };
            let mut command = command.clone();
            if !command.contains_key("comment") {
                command.insert("comment", tag);
            }
            Ok(ReadStream::docs(reply_docs(
                target.run_command(command).await?,
            )))
        }
        Command::Use(_) | Command::CreateCollection { .. } | Command::DropDatabase { .. } => {
            Err(AppError::internal("Not a read statement"))
        }
        Command::Collection { collection, method } => {
            let coll = db.collection::<RawDocumentBuf>(collection);
            match method {
                Method::Find(f) if f.count => {
                    let mut a = coll.count_documents(f.filter.clone()).comment(tag);
                    if let Some(n) = f.skip {
                        a = a.skip(n);
                    }
                    if let Some(n) = f.limit.filter(|n| *n > 0) {
                        a = a.limit(n as u64);
                    }
                    if let Some(h) = &f.hint {
                        a = a.hint(hint(h)?);
                    }
                    if let Some(c) = &f.collation {
                        a = a.collation(collation(c)?);
                    }
                    if let Some(ms) = f.max_time_ms {
                        a = a.max_time(Duration::from_millis(ms));
                    }
                    Ok(count_doc(a.await?))
                }
                Method::Find(f) => {
                    let mut a = coll.find(f.filter.clone()).comment(tag);
                    if let Some(p) = &f.projection {
                        a = a.projection(p.clone());
                    }
                    match sort {
                        Some(s) => a = a.sort(sort_doc(s)),
                        None => {
                            if let Some(s) = &f.sort {
                                a = a.sort(s.clone());
                            }
                        }
                    }
                    if let Some(n) = f.skip {
                        a = a.skip(n);
                    }
                    if let Some(n) = f.limit.filter(|n| *n > 0) {
                        a = a.limit(n);
                    }
                    if let Some(h) = &f.hint {
                        a = a.hint(hint(h)?);
                    }
                    if let Some(c) = &f.collation {
                        a = a.collation(collation(c)?);
                    }
                    if let Some(b) = batch {
                        a = a.batch_size(b);
                    }
                    if let Some(ms) = f.max_time_ms {
                        a = a.max_time(Duration::from_millis(ms));
                    }
                    Ok(cursor(a.await?))
                }
                Method::FindOne { filter, projection } => {
                    let mut a = coll.find(filter.clone()).limit(1).comment(tag);
                    if let Some(p) = projection {
                        a = a.projection(p.clone());
                    }
                    Ok(cursor(a.await?))
                }
                Method::Aggregate { pipeline, options } => {
                    let mut pipeline = pipeline.clone();
                    if let Some(s) = sort {
                        pipeline.push(doc! { "$sort": sort_doc(s) });
                    }
                    let mut a = coll
                        .aggregate(pipeline)
                        .with_type::<RawDocumentBuf>()
                        .comment(tag);
                    if let Some(b) = batch {
                        a = a.batch_size(b);
                    }
                    for (k, v) in options.iter().flatten() {
                        a = match k.as_str() {
                            "allowDiskUse" => a.allow_disk_use(opt_bool(k, v)?),
                            "collation" => a.collation(collation(opt_doc(k, v)?)?),
                            "hint" => a.hint(hint(v)?),
                            "let" => a.let_vars(opt_doc(k, v)?.clone()),
                            "batchSize" => a.batch_size(opt_u32(k, v)?),
                            "maxTimeMS" => a.max_time(Duration::from_millis(opt_u64(k, v)?)),
                            _ => return Err(unknown_option("aggregate", k)),
                        };
                    }
                    Ok(cursor(a.await?))
                }
                Method::CountDocuments { filter, options } => {
                    let mut a = coll.count_documents(filter.clone()).comment(tag);
                    for (k, v) in options.iter().flatten() {
                        a = match k.as_str() {
                            "limit" => a.limit(opt_u64(k, v)?),
                            "skip" => a.skip(opt_u64(k, v)?),
                            "hint" => a.hint(hint(v)?),
                            "collation" => a.collation(collation(opt_doc(k, v)?)?),
                            "maxTimeMS" => a.max_time(Duration::from_millis(opt_u64(k, v)?)),
                            _ => return Err(unknown_option("countDocuments", k)),
                        };
                    }
                    Ok(count_doc(a.await?))
                }
                Method::EstimatedDocumentCount => Ok(count_doc(
                    coll.estimated_document_count().comment(tag).await?,
                )),
                Method::Distinct { field, filter } => {
                    let values = coll.distinct(field, filter.clone()).comment(tag).await?;
                    Ok(ReadStream::docs(
                        values.into_iter().map(|v| doc! { "value": v }).collect(),
                    ))
                }
                Method::GetIndexes => {
                    let command = doc! { "listIndexes": collection, "comment": tag };
                    Ok(ReadStream::docs(reply_docs(db.run_command(command).await?)))
                }
                Method::Stats => {
                    let command = doc! { "collStats": collection, "comment": tag };
                    Ok(ReadStream::docs(vec![db.run_command(command).await?]))
                }
                _ => Err(AppError::internal("Not a read statement")),
            }
        }
    }
}

fn cursor(c: Cursor<RawDocumentBuf>) -> ReadStream {
    ReadStream {
        source: Source::Cursor(Box::new(c)),
    }
}

fn count_doc(n: u64) -> ReadStream {
    ReadStream::docs(vec![doc! { "count": i64::try_from(n).unwrap_or(i64::MAX) }])
}

/// The documents of a command reply: the first batch when the reply is a cursor (as for
/// `listCollections` or `listIndexes`), else the reply itself.
pub(crate) fn reply_docs(mut reply: Document) -> Vec<Document> {
    let batch = reply
        .get_document_mut("cursor")
        .ok()
        .and_then(|c| c.remove("firstBatch"));
    match batch {
        Some(Bson::Array(items)) => items
            .into_iter()
            .filter_map(|b| match b {
                Bson::Document(d) => Some(d),
                _ => None,
            })
            .collect(),
        _ => vec![reply],
    }
}

fn sort_doc(s: &RunSort) -> Document {
    let mut d = Document::new();
    d.insert(s.column.clone(), if s.ascending { 1 } else { -1 });
    d
}

fn unknown_option(method: &str, key: &str) -> AppError {
    AppError::invalid(format!("Unknown {method} option: {key}"))
}

fn opt_bool(key: &str, v: &Bson) -> Result<bool, AppError> {
    v.as_bool()
        .ok_or_else(|| AppError::invalid(format!("{key} must be true or false")))
}

fn opt_doc<'a>(key: &str, v: &'a Bson) -> Result<&'a Document, AppError> {
    v.as_document()
        .ok_or_else(|| AppError::invalid(format!("{key} must be a document")))
}

fn opt_u64(key: &str, v: &Bson) -> Result<u64, AppError> {
    let n = match v {
        Bson::Int32(i) => i64::from(*i),
        Bson::Int64(i) => *i,
        Bson::Double(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => *f as i64,
        _ => -1,
    };
    u64::try_from(n)
        .map_err(|_| AppError::invalid(format!("{key} must be a whole number of 0 or more")))
}

fn opt_u32(key: &str, v: &Bson) -> Result<u32, AppError> {
    u32::try_from(opt_u64(key, v)?).map_err(|_| AppError::invalid(format!("{key} is too large")))
}

fn hint(v: &Bson) -> Result<Hint, AppError> {
    match v {
        Bson::String(name) => Ok(Hint::Name(name.clone())),
        Bson::Document(keys) => Ok(Hint::Keys(keys.clone())),
        _ => Err(AppError::invalid(
            "hint must be an index name or a key document",
        )),
    }
}

fn collation(d: &Document) -> Result<Collation, AppError> {
    mongodb::bson::from_document(d.clone())
        .map_err(|e| AppError::invalid("Invalid collation").with_detail(e.to_string()))
}

// ---- cancel

/// Stops the server operations of a run: they carry `query_id` as their comment. Only the
/// user's own operations are listed, which needs no extra privilege.
pub(crate) async fn kill(state: &AppState, conn_id: &str, query_id: &str) -> Result<(), AppError> {
    let client = client::client(state, conn_id).await?;
    let admin = client.database("admin");
    let mut ops = admin
        .aggregate(vec![
            doc! { "$currentOp": {} },
            doc! { "$match": { "command.comment": query_id } },
        ])
        .await?;
    let mut ids = Vec::new();
    while ops.advance().await? {
        let op = ops.deserialize_current()?;
        if let Some(id) = op.get("opid") {
            ids.push(id.clone());
        }
    }
    for id in ids {
        admin.run_command(doc! { "killOp": 1, "op": id }).await?;
    }
    Ok(())
}
