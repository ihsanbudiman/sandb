//! Runs the mongosh statements of an editor run. Reads stream from the server and stop at the
//! row limit or the page byte limit, so a large collection or large documents never land in
//! memory whole.
//!
//! Routing is `guard::kind` and nothing else: a statement that writes runs through `write`, and
//! everything else reads, including a statement the guard cannot vouch for (the server decides
//! those on a writable connection). Every server operation carries the run's comment, which is
//! how a cancel finds and stops it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use mongodb::bson::{Document, RawDocumentBuf, doc};
use mongodb::{Client, Cursor};

use crate::core::connections::{target_database, Connection};
use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::mongo::client::Access;
use crate::core::mongo::command::{Command, Method, Statement};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::flatten::{Flattener, Page};
use crate::core::mongo::guard::{self, Kind};
use crate::core::mongo::options::{
    apply_aggregate_options, collation, hint, Options, COUNT_DOCUMENTS_OPTIONS,
};
use crate::core::mongo::{client, introspect, parse, reply, write, WRITE_REPLY_WAIT};
use crate::core::result::{limit, QueryRun, RunRequest, RunSort, StatementResult};

/// `query::run` for a MongoDB connection.
pub async fn run(
    access: &impl Access,
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
    if check && !conn.may_write() {
        guard::check_read_only(conn, &stmts)?;
    }
    if req.sort.is_some() && !(stmts.len() == 1 && sortable(&stmts[0].command)) {
        return Err(AppError::invalid(
            "Sorting requires exactly one find or aggregate",
        ));
    }
    let mut database = target_database(conn, req.database.as_deref());
    // The last statement's own `.limit(n)` sizes the page when the caller sets none.
    let limit = limit(req.limit, || match &stmts.last()?.command {
        Command::Collection {
            method: Method::Find(f),
            ..
        } => f.limit.and_then(|n| u64::try_from(n).ok()),
        _ => None,
    });
    let started = Instant::now();
    let run = access.queries().register(
        &req.query_id,
        &conn.id,
        &database,
        Engine::Mongo,
        stmts.len(),
    )?;
    let cancelled = run.cancelled.clone();
    if cancelled.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let client = client::client(access, &conn.id).await?;
    let comment = OpComment::new(&req.query_id);
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
            r.limit = limit;
            results.push(r);
        } else {
            if database.is_empty() {
                return Err(AppError::invalid("Choose a database for this tab first"));
            }
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
                comment,
                &mut notices,
            );
            tokio::pin!(work);
            let write = !matches!(guard::kind(&s.command), Kind::Read);
            let n = results.len() + 1;
            let r = tokio::select! {
                r = &mut work => r,
                () = cancelled.cancelled() => {
                    if !write {
                        return Err(AppError::cancelled());
                    }
                    // A write that reached the server can still apply. Wait a moment for its
                    // reply (the kill runs meanwhile), so the user learns what happened.
                    match tokio::time::timeout(WRITE_REPLY_WAIT, &mut work).await {
                        Ok(r) => r,
                        Err(_) => {
                            return Err(AppError::cancelled().with_detail(format!(
                                "Statement {n} may have changed data before the cancel. Reload to check."
                            )));
                        }
                    }
                }
            };
            // A killed operation fails with the server's "interrupted" error.
            if cancelled.is_cancelled() {
                let e = AppError::cancelled();
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
            r.limit = limit;
            r.database = Some(database.clone());
            results.push(r);
        }
        run.done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    drop(run);
    Ok(QueryRun {
        statements: results,
        notices,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// A `.count()` result is one number, so a header click has nothing to sort.
fn sortable(cmd: &Command) -> bool {
    match cmd {
        Command::Collection {
            method: Method::Find(f),
            ..
        } => !f.count,
        Command::Collection {
            method: Method::Aggregate { .. },
            ..
        } => true,
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
    comment: OpComment<'_>,
    notices: &mut Vec<String>,
) -> Result<StatementResult, AppError> {
    if matches!(guard::kind(&s.command), Kind::Write | Kind::Schema) {
        let out = write::execute(client, database, &s.command, comment).await?;
        notices.push(out.notice);
        let mut r = match out.document {
            Some(d) => {
                let mut f = Flattener::default();
                f.push(d);
                page_result(&s.text, f.finish(), false, false, false)
            }
            None => empty_result(&s.text, false),
        };
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
        comment,
        batch,
        None,
    )
    .await?;
    let (page, end) = page(&mut stream, limit).await?;
    if end == PageEnd::Bytes {
        notices.push(format!(
            "Stopped at the 16 MB page limit after {} documents. Use a projection or a smaller limit.",
            page.rows.len()
        ));
    }
    if page.rows.is_empty() {
        notices.push(format!("{}: no documents", label(&s.command)));
    }
    let guard_is_read = guard::kind(&s.command) == Kind::Read;
    Ok(page_result(
        &s.text,
        page,
        end != PageEnd::Stream,
        lone && sortable(&s.command),
        guard_is_read,
    ))
}

/// The notice name of a statement: the method and its collection, or the command itself.
fn label(cmd: &Command) -> String {
    match cmd {
        Command::Collection { collection, .. } => {
            format!("{} on {collection}", guard::name(cmd))
        }
        other => guard::name(other).to_string(),
    }
}

fn page_result(
    text: &str,
    page: Page,
    truncated: bool,
    sortable: bool,
    is_read: bool,
) -> StatementResult {
    StatementResult {
        sql: text.to_string(),
        row_count: page.rows.len(),
        columns: page.columns,
        rows: page.rows,
        truncated,
        limit: 0,
        affected: None,
        // Only a provable read says so; a statement the guard cannot vouch for is not one, even
        // when it runs through the read path.
        is_read,
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
        limit: 0,
        affected: None,
        is_read,
        sortable: false,
        elapsed_ms: 0,
        documents: None,
        cell_types: None,
        database: None,
    }
}

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

/// Byte limit of one page: the sum of the BSON sizes of its documents.
pub const PAGE_BYTES: usize = 16 * 1024 * 1024;

/// Where a page stopped: the stream ended, or one of its two caps cut it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEnd {
    Stream,
    Rows,
    Bytes,
}

/// Reads a read stream into one page: at most `limit` rows and at most [`PAGE_BYTES`] of BSON,
/// so a large collection or large documents never land in memory whole.
pub(crate) async fn page(
    stream: &mut ReadStream,
    limit: usize,
) -> Result<(Page, PageEnd), AppError> {
    let mut f = Flattener::default();
    let mut bytes = 0usize;
    let end = loop {
        let Some((doc, size)) = stream.next().await? else {
            break PageEnd::Stream;
        };
        if f.len() == limit {
            break PageEnd::Rows;
        }
        if !f.is_empty() && bytes + size > PAGE_BYTES {
            break PageEnd::Bytes;
        }
        bytes += size;
        f.push(doc);
    };
    Ok((f.finish(), end))
}

/// Opens a read: the user's own skip, limit, sort and `maxTimeMS` apply; the caller applies the
/// page limits. `sort` (a header click) replaces the find sort or adds a `$sort` stage to the
/// pipeline; `run` refuses it for anything else. `batch` sizes the server batches of a cursor.
pub(crate) async fn open_read(
    client: &Client,
    database: &str,
    cmd: &Command,
    sort: Option<&RunSort>,
    comment: OpComment<'_>,
    batch: Option<u32>,
    max_time: Option<Duration>,
) -> Result<ReadStream, AppError> {
    let db = client.database(database);
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
                .comment(comment.tag())
                .await?;
            let mut out = Vec::new();
            while c.advance().await? {
                let spec = c.deserialize_current()?;
                let kind = reply::driver_kind(spec.collection_type);
                out.push(doc! { "name": spec.name, "type": reply::kind_label(kind) });
            }
            out.sort_by(|a, b| a.get_str("name").ok().cmp(&b.get_str("name").ok()));
            Ok(ReadStream::docs(out))
        }
        Command::RunCommand { admin, command } => {
            let target = if *admin { client.database("admin") } else { db };
            let mut command = command.clone();
            comment.apply(&mut command);
            Ok(ReadStream::docs(reply::reply_docs(
                &target.run_command(command).await?,
            )))
        }
        Command::Use(_) | Command::CreateCollection { .. } | Command::DropDatabase { .. } => {
            Err(AppError::internal("Not a read statement"))
        }
        Command::Collection { collection, method } => {
            let coll = db.collection::<RawDocumentBuf>(collection);
            match method {
                Method::Find(f) if f.count => {
                    let mut a = coll.count_documents(f.filter.clone()).comment(comment.tag());
                    if let Some(n) = f.skip {
                        a = a.skip(n);
                    }
                    if let Some(n) = f.limit.filter(|n| *n > 0) {
                        a = a.limit(n as u64);
                    }
                    if let Some(h) = &f.hint {
                        a = a.hint(hint("hint", h)?);
                    }
                    if let Some(c) = &f.collation {
                        a = a.collation(collation("collation", c)?);
                    }
                    if let Some(ms) = f.max_time_ms {
                        a = a.max_time(Duration::from_millis(ms));
                    }
                    Ok(count_stream(a.await?))
                }
                Method::Find(f) => {
                    let mut a = coll.find(f.filter.clone()).comment(comment.tag());
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
                        a = a.hint(hint("hint", h)?);
                    }
                    if let Some(c) = &f.collation {
                        a = a.collation(collation("collation", c)?);
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
                    let mut a = coll.find(filter.clone()).limit(1).comment(comment.tag());
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
                        .comment(comment.tag());
                    if let Some(b) = batch {
                        a = a.batch_size(b);
                    }
                    let o = Options::new("aggregate", options.as_ref());
                    Ok(cursor(apply_aggregate_options(a, &o)?.await?))
                }
                Method::CountDocuments { filter, options } => {
                    let mut a = coll.count_documents(filter.clone()).comment(comment.tag());
                    let o = Options::new("countDocuments", options.as_ref());
                    o.check(COUNT_DOCUMENTS_OPTIONS)?;
                    if let Some(n) = o.whole("limit", i64::MAX)? {
                        a = a.limit(n as u64);
                    }
                    if let Some(n) = o.whole("skip", i64::MAX)? {
                        a = a.skip(n as u64);
                    }
                    if let Some(v) = o.hint("hint")? {
                        a = a.hint(v);
                    }
                    if let Some(v) = o.collation("collation")? {
                        a = a.collation(v);
                    }
                    if let Some(ms) = o.whole("maxTimeMS", i64::MAX)? {
                        a = a.max_time(Duration::from_millis(ms as u64));
                    }
                    Ok(count_stream(a.await?))
                }
                // `distinct` and `estimatedDocumentCount` take no parsed options, so a caller
                // that needs a server time limit passes it to `open_read`.
                Method::EstimatedDocumentCount => {
                    let mut a = coll.estimated_document_count().comment(comment.tag());
                    if let Some(t) = max_time {
                        a = a.max_time(t);
                    }
                    Ok(count_stream(a.await?))
                }
                Method::Distinct { field, filter } => {
                    let mut a = coll.distinct(field, filter.clone()).comment(comment.tag());
                    if let Some(t) = max_time {
                        a = a.max_time(t);
                    }
                    let values = a.await?;
                    Ok(ReadStream::docs(
                        values.into_iter().map(reply::value_row).collect(),
                    ))
                }
                Method::GetIndexes => {
                    let command = doc! { "listIndexes": collection, "comment": comment.as_str() };
                    Ok(ReadStream::docs(reply::reply_docs(
                        &db.run_command(command).await?,
                    )))
                }
                Method::Stats => {
                    let command = doc! { "collStats": collection, "comment": comment.as_str() };
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

fn count_stream(n: u64) -> ReadStream {
    ReadStream::docs(vec![reply::count_row(n)])
}

fn sort_doc(s: &RunSort) -> Document {
    let mut d = Document::new();
    d.insert(s.column.clone(), if s.ascending { 1 } else { -1 });
    d
}

/// Stops the server operations of a run: they carry its comment. Only the user's own operations
/// are listed, which needs no extra privilege.
pub(crate) async fn kill(access: &impl Access, conn_id: &str, query_id: &str) -> Result<(), AppError> {
    let client = client::client(access, conn_id).await?;
    let comment = OpComment::new(query_id);
    let admin = client.database("admin");
    let mut ops = admin
        .aggregate(vec![doc! { "$currentOp": {} }, comment.current_op()])
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
