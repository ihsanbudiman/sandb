//! The tools the model can call. Reads go through the AI read path (guard, read-only session,
//! limits); `render_chart` reads the same way and sends the rows to the UI as a chart;
//! `propose_write` shows the statement to the user, and runs it only after the user approves it.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::core::ai::approval::APPROVAL_TIMEOUT;
use crate::core::ai::session::ChatEvent;
use crate::core::ai::settings::{AiSettings, Privacy};
use crate::core::conn_ro;
use crate::core::connections::{Connection, Engine};
use crate::core::db::introspect;
use crate::core::db::readonly::{self, AiReadResult, ReadLimits};
use crate::core::db::write;
use crate::core::error::{AppError, ErrorCode};
use crate::core::guard;
use crate::core::mongo::{self, guard as mongo_guard};
use crate::core::state::AppState;

/// Largest tool output sent to the model; longer output is cut with a marker.
pub const MAX_CONTENT_BYTES: usize = 32 * 1024;
/// Rows of a query result sent to the UI.
pub const UI_ROWS: usize = 50;
/// Rows a chart keeps. Charts are saved with the chat, so this stays small.
pub const CHART_ROWS: usize = 1000;
const CHART_TYPES: [&str; 5] = ["bar", "line", "area", "pie", "scatter"];
/// Most value columns of one chart.
const MAX_SERIES: usize = 8;
const CUT_MARKER: &str = "\n[cut: the tool output was longer than 32 KiB]";

/// Which sources a tool serves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Serves {
    All,
    Sql,
    Mongo,
}

/// OpenAI function tool definitions for the engines of `sources`. `list_tables` and
/// `describe_table` serve every source; the SQL read/chart/write tools exist when a SQL source
/// does, the Mongo ones when a Mongo source does. Every tool takes `source`, an enum of the ids
/// it serves; it is required when the chat has more than one source. A SQL-only chat gets the
/// same definitions as before MongoDB.
pub fn definitions(sources: &[Source]) -> Value {
    let ids = |serves: Serves| -> Vec<&str> {
        sources
            .iter()
            .filter(|s| match serves {
                Serves::All => true,
                Serves::Sql => s.conn.engine != Engine::Mongo,
                Serves::Mongo => s.conn.engine == Engine::Mongo,
            })
            .map(|s| s.id.as_str())
            .collect()
    };
    let mongo = !ids(Serves::Mongo).is_empty();
    let mixed = mongo && !ids(Serves::Sql).is_empty();
    let mut defs = Vec::new();
    for (serves, mut def, hint) in base_definitions(mongo) {
        let ids = ids(serves);
        if ids.is_empty() {
            continue;
        }
        if mixed && let Some(hint) = hint {
            let f = &mut def["function"]["description"];
            *f = json!(format!("{} {hint}", f.as_str().unwrap_or_default()));
        }
        let params = &mut def["function"]["parameters"];
        params["properties"]["source"] = json!({
            "type": "string",
            "enum": ids,
            "description": "Id of the database to use, from the system prompt (for example db1).",
        });
        if sources.len() > 1 {
            match params["required"].as_array_mut() {
                Some(r) => r.push(json!("source")),
                None => params["required"] = json!(["source"]),
            }
        }
        defs.push(def);
    }
    Value::Array(defs)
}

/// The tools, whom they serve, and the hint a mixed SQL + MongoDB chat adds to the description.
/// `mongo`: a Mongo source exists, so `list_tables` and `describe_table` describe it too.
fn base_definitions(mongo: bool) -> Vec<(Serves, Value, Option<&'static str>)> {
    let (list, list_schema, describe, describe_table, describe_schema) = if mongo {
        (
            "List the tables and views (MongoDB: collections and views) of one database.",
            "PostgreSQL schema. Default: the current schema. Ignored on MySQL, SQLite and MongoDB.",
            "List the columns of one table or view: name, type, nullable, default, primary key. On MongoDB, the fields from a sample, with their types and the share of documents that have them.",
            "Table or collection name without schema.",
            "PostgreSQL schema. Default: the current schema. Ignored on MongoDB.",
        )
    } else {
        (
            "List the tables and views of one database.",
            "PostgreSQL schema. Default: the current schema. Ignored on MySQL and SQLite.",
            "List the columns of one table or view: name, type, nullable, default, primary key.",
            "Table name without schema.",
            "PostgreSQL schema. Default: the current schema.",
        )
    };
    vec![
        (
            Serves::All,
            json!({
                "type": "function",
                "function": {
                    "name": "list_tables",
                    "description": list,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "schema": {"type": "string", "description": list_schema}
                        }
                    }
                }
            }),
            None,
        ),
        (
            Serves::All,
            json!({
                "type": "function",
                "function": {
                    "name": "describe_table",
                    "description": describe,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "table": {"type": "string", "description": describe_table},
                            "schema": {"type": "string", "description": describe_schema}
                        },
                        "required": ["table"]
                    }
                }
            }),
            None,
        ),
        (
            Serves::Sql,
            json!({
                "type": "function",
                "function": {
                    "name": "run_read_query",
                    "description": "Run one read-only SQL statement (SELECT, SHOW, DESCRIBE, EXPLAIN, PRAGMA) and get the result. Writes are refused.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "sql": {"type": "string", "description": "Exactly one read-only statement."}
                        },
                        "required": ["sql"]
                    }
                }
            }),
            Some("SQL sources only: on MongoDB use run_mongo_read."),
        ),
        (
            Serves::Sql,
            json!({
                "type": "function",
                "function": {
                    "name": "render_chart",
                    "description": "Run one read-only SQL statement and show its result to the user as a chart in the chat. This is the only way to show a chart: call it every time the user asks for a chart, graph or plot. The chart is also in the PDF export of the reply. Shape the data in SQL: one row per point, sorted, already aggregated.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "sql": {"type": "string", "description": "Exactly one read-only statement."},
                            "type": {"type": "string", "enum": CHART_TYPES, "description": "bar, line, area, pie or scatter."},
                            "title": {"type": "string", "description": "Short chart title."},
                            "x": {"type": "string", "description": "Result column for the x axis (pie: the slice labels)."},
                            "y": {"type": "array", "items": {"type": "string"}, "description": "Result columns with the numeric values, one series each (pie: exactly one)."},
                            "series": {"type": "string", "description": "Optional result column that splits one y column into one series per distinct value (long format). Not for pie."},
                            "stacked": {"type": "boolean", "description": "Stack the series (bar and area)."}
                        },
                        "required": ["sql", "type", "x", "y"]
                    }
                }
            }),
            Some("SQL sources only: on MongoDB use render_mongo_chart."),
        ),
        (
            Serves::Sql,
            json!({
                "type": "function",
                "function": {
                    "name": "propose_write",
                    "description": "Propose SQL that changes data or schema. The user sees it and approves or denies it. Approved SQL runs in one transaction; the result says whether it ran and how many rows changed.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "sql": {"type": "string", "description": "The SQL to propose."}
                        },
                        "required": ["sql"]
                    }
                }
            }),
            Some("SQL sources only: on MongoDB use propose_mongo_write."),
        ),
        (
            Serves::Mongo,
            json!({
                "type": "function",
                "function": {
                    "name": "run_mongo_read",
                    "description": "Run one read-only mongosh statement and get the result. Exactly one statement: db.<collection>.find(...), aggregate([...]), countDocuments, distinct, findOne, estimatedDocumentCount, or a read runCommand. Writes are refused.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Exactly one read-only mongosh statement, for example db.orders.find({status: \"paid\"}, {total: 1}).sort({_id: -1}).limit(20) or db.orders.aggregate([...])."}
                        },
                        "required": ["query"]
                    }
                }
            }),
            Some("MongoDB sources only: on SQL sources use run_read_query."),
        ),
        (
            Serves::Mongo,
            json!({
                "type": "function",
                "function": {
                    "name": "render_mongo_chart",
                    "description": "Run one read-only mongosh statement and show its result to the user as a chart in the chat. This is the only way to show a chart on MongoDB: call it every time the user asks for a chart, graph or plot. The chart is also in the PDF export of the reply. Shape the data in the pipeline: one document per point, sorted, already aggregated; $project flat fields. x, y and series name result fields (flattened paths such as _id or month).",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Exactly one read-only mongosh statement whose result has one document per point."},
                            "type": {"type": "string", "enum": CHART_TYPES, "description": "bar, line, area, pie or scatter."},
                            "title": {"type": "string", "description": "Short chart title."},
                            "x": {"type": "string", "description": "Result field for the x axis (pie: the slice labels)."},
                            "y": {"type": "array", "items": {"type": "string"}, "description": "Result fields with the numeric values, one series each (pie: exactly one)."},
                            "series": {"type": "string", "description": "Optional result field that splits one y field into one series per distinct value (long format). Not for pie."},
                            "stacked": {"type": "boolean", "description": "Stack the series (bar and area)."}
                        },
                        "required": ["query", "type", "x", "y"]
                    }
                }
            }),
            Some("MongoDB sources only: on SQL sources use render_chart."),
        ),
        (
            Serves::Mongo,
            json!({
                "type": "function",
                "function": {
                    "name": "propose_mongo_write",
                    "description": "Propose one mongosh write: insert, update, replace, delete, bulkWrite, findOneAnd*, index, drop, dropDatabase, rename, createCollection or a runCommand that writes. The user sees it and approves or denies it; only then does it run, and the result says whether it ran and how many documents changed. A single statement is atomic on one document only; multi-document operations are not one transaction.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Exactly one mongosh write statement."}
                        },
                        "required": ["query"]
                    }
                }
            }),
            Some("MongoDB sources only: on SQL sources use propose_write."),
        ),
    ]
}

/// The arguments as an object; `None` when the text is not a JSON object.
pub fn parse_args(raw: &str) -> Option<Map<String, Value>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(Map::new());
    }
    match serde_json::from_str(raw) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// One database of the chat. The model names it by `id`.
#[derive(Debug)]
pub struct Source {
    /// `db1`, `db2`, … in the order of the request.
    pub id: String,
    pub conn: Connection,
    pub database: String,
    /// PG schema for `list_tables` and `describe_table` when the model gives none.
    pub schema: Option<String>,
    /// The chat's write mode for this source is on.
    pub allow_writes: bool,
}

/// What one chat runs against.
pub struct ToolContext<'a> {
    pub state: &'a AppState,
    /// At least one.
    pub sources: &'a [Source],
    pub settings: &'a AiSettings,
}

impl ToolContext<'_> {
    /// The source that `args` names. Without `source`, the only one; with more, an error.
    fn source(&self, args: &Map<String, Value>) -> Result<&Source, AppError> {
        let ids = || {
            self.sources
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match opt_str(args, "source") {
            Some(id) => self.sources.iter().find(|s| s.id == id).ok_or_else(|| {
                AppError::invalid(format!(
                    "Unknown source {}. Use one of: {}",
                    quoted(id),
                    ids()
                ))
            }),
            None if self.sources.len() == 1 => Ok(&self.sources[0]),
            None => Err(AppError::invalid(format!(
                "The tool needs \"source\", one of: {}",
                ids()
            ))),
        }
    }
}

/// The result of one tool call.
pub struct ToolOutput {
    /// For the UI, in order: a `write_preview` or `chart` when there is one, then the `tool_result`.
    /// A proposed write has only its `write_preview` here; `finish_write` makes the rest.
    pub events: Vec<ChatEvent>,
    /// The `tool` message for the model, at most `MAX_CONTENT_BYTES`. Empty for a proposed write.
    pub content: String,
    /// A write that waits for the user's decision.
    pub write: Option<ProposedWrite>,
}

/// A write that `propose_write` accepted. It runs only after the user approves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedWrite {
    pub conn_id: String,
    pub database: String,
    pub sql: String,
}

/// What the user did with a proposed write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Denied,
    /// Nobody decided within `APPROVAL_TIMEOUT`.
    TimedOut,
}

/// UI part of a successful call.
struct Done {
    summary: String,
    columns: Option<Vec<String>>,
    rows: Option<Vec<Vec<Value>>>,
    truncated: Option<bool>,
    /// An event sent before the `tool_result`: a `write_preview` or a `chart`.
    preview: Option<ChatEvent>,
    content: Value,
    write: Option<ProposedWrite>,
}

impl Done {
    fn plain(summary: String, content: Value) -> Self {
        Self {
            summary,
            columns: None,
            rows: None,
            truncated: None,
            preview: None,
            content,
            write: None,
        }
    }
}

pub async fn execute(ctx: &ToolContext<'_>, id: &str, name: &str, raw_args: &str) -> ToolOutput {
    let res = match parse_args(raw_args) {
        Some(args) => run(ctx, id, name, &args).await,
        None => Err(AppError::invalid(
            "The tool arguments are not a JSON object",
        )),
    };
    output(ctx, id, name, res)
}

/// Runs an approved write, or reports a denied one: the `tool_result` and the model's view.
/// `stop` stops the write before it commits.
pub async fn finish_write(
    ctx: &ToolContext<'_>,
    id: &str,
    w: &ProposedWrite,
    decision: Decision,
    stop: &CancellationToken,
) -> ToolOutput {
    let res = match decision {
        Decision::Denied => Err(AppError::new(
            ErrorCode::WriteDenied,
            "The user denied this write. Nothing ran.",
        )),
        Decision::TimedOut => Err(AppError::new(
            ErrorCode::WriteDenied,
            format!(
                "Nobody approved this write within {} minutes. Nothing ran.",
                APPROVAL_TIMEOUT.as_secs() / 60
            ),
        )),
        Decision::Approved => write::run_approved(ctx.state, &w.conn_id, &w.database, &w.sql, stop)
            .await
            .map(|r| {
                Done::plain(
                    format!(
                        "{} · committed · {} ms",
                        count(r.affected as usize, "row changed", "rows changed"),
                        r.elapsed_ms
                    ),
                    json!({"status": "committed", "rowsAffected": r.affected}),
                )
            }),
    };
    // The tool name the model sees: the engine's write tool, so the model's history is honest.
    let name = ctx
        .sources
        .iter()
        .find(|s| s.conn.id == w.conn_id)
        .map(|s| {
            if s.conn.engine == Engine::Mongo {
                "propose_mongo_write"
            } else {
                "propose_write"
            }
        })
        .unwrap_or("propose_write");
    output(ctx, id, name, res)
}

fn output(ctx: &ToolContext<'_>, id: &str, name: &str, res: Result<Done, AppError>) -> ToolOutput {
    match res {
        Ok(d) if d.write.is_some() => ToolOutput {
            events: d.preview.into_iter().collect(),
            content: String::new(),
            write: d.write,
        },
        Ok(d) => {
            let mut events = Vec::with_capacity(2);
            events.extend(d.preview);
            events.push(ChatEvent::ToolResult {
                id: id.to_string(),
                ok: true,
                summary: d.summary,
                columns: d.columns,
                rows: d.rows,
                truncated: d.truncated,
                error: None,
            });
            ToolOutput {
                events,
                content: cap(d.content.to_string()),
                write: None,
            }
        }
        Err(e) => {
            // A database error can quote a row value: reads, and writes too.
            let data = matches!(
                name,
                "run_read_query"
                    | "render_chart"
                    | "propose_write"
                    | "run_mongo_read"
                    | "render_mongo_chart"
                    | "propose_mongo_write"
            );
            let hide = data && ctx.settings.privacy == Privacy::Schema;
            let err = model_error(&e, hide);
            ToolOutput {
                events: vec![ChatEvent::ToolResult {
                    id: id.to_string(),
                    ok: false,
                    summary: e.message.clone(),
                    columns: None,
                    rows: None,
                    truncated: None,
                    error: Some(e),
                }],
                content: cap(json!({ "error": err }).to_string()),
                write: None,
            }
        }
    }
}

/// Error codes whose message is our own fixed text, never the server's, so it cannot quote a
/// row value.
const OWN_MESSAGE_CODES: [ErrorCode; 8] = [
    ErrorCode::GuardRejected,
    ErrorCode::Timeout,
    ErrorCode::Cancelled,
    ErrorCode::ReadOnlyViolation,
    ErrorCode::ConnectionReadOnly,
    ErrorCode::InvalidInput,
    ErrorCode::NotFound,
    ErrorCode::WriteDenied,
];

/// The model's view of a failed call. Database messages are kept so the model can fix its SQL.
/// `hide` (a read in `schema` privacy): a server message can quote a row value
/// (`invalid input syntax for type integer: "alice@example.com"`), so only our own messages
/// pass; the rest become the server's error code. The detail is left out.
fn model_error(e: &AppError, hide: bool) -> Value {
    if !hide {
        let mut err = json!({"code": e.code, "message": e.message});
        if let Some(d) = &e.detail {
            err["detail"] = json!(d);
        }
        return err;
    }
    let message = if OWN_MESSAGE_CODES.contains(&e.code) {
        e.message.clone()
    } else {
        let hidden = "The error text is hidden by the privacy setting.";
        match &e.server_code {
            Some(c) => format!("The database refused the query ({c}). {hidden}"),
            None => format!("The query failed. {hidden}"),
        }
    };
    json!({"code": e.code, "message": message})
}

async fn run(
    ctx: &ToolContext<'_>,
    id: &str,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    match name {
        "list_tables" => list_tables(ctx, args).await,
        "describe_table" => describe_table(ctx, args).await,
        "run_read_query" => run_read_query(ctx, args, false).await,
        "render_chart" => render_chart(ctx, id, args, false).await,
        "propose_write" => propose_write(ctx, id, args),
        "run_mongo_read" => run_read_query(ctx, args, true).await,
        "render_mongo_chart" => render_chart(ctx, id, args, true).await,
        "propose_mongo_write" => propose_mongo_write(ctx, id, args),
        _ => Err(AppError::not_found(format!("Unknown tool {name}"))),
    }
}

fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The error for calling an engine's tool on a source of the other engine; `use_instead` is
/// the tool the model should call.
fn wrong_engine(src: &Source, use_instead: &str) -> AppError {
    let (engine, language) = match src.conn.engine {
        Engine::Mongo => ("MongoDB", "a mongosh query"),
        Engine::Pg => ("PostgreSQL", "SQL"),
        Engine::Mysql => ("MySQL/MariaDB", "SQL"),
        Engine::Sqlite => ("SQLite", "SQL"),
    };
    AppError::invalid(format!(
        "Source {} is {engine}. Use {use_instead} with {language}.",
        src.id
    ))
}

fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, AppError> {
    opt_str(args, key).ok_or_else(|| AppError::invalid(format!("The tool needs \"{key}\"")))
}

/// `n` and the noun, singular for 1.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn schema_of<'a>(src: &'a Source, args: &'a Map<String, Value>) -> Option<&'a str> {
    if src.conn.engine != Engine::Pg {
        return None;
    }
    opt_str(args, "schema").or(src.schema.as_deref())
}

async fn list_tables(ctx: &ToolContext<'_>, args: &Map<String, Value>) -> Result<Done, AppError> {
    let src = ctx.source(args)?;
    let schema = schema_of(src, args);
    let tables = introspect::list_tables(ctx.state, &src.conn.id, &src.database, schema).await?;
    let mut content = json!({ "tables": tables });
    if src.conn.engine == Engine::Pg {
        content["schema"] = json!(schema.unwrap_or("public"));
    }
    Ok(Done::plain(count(tables.len(), "table", "tables"), content))
}

async fn describe_table(
    ctx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let src = ctx.source(args)?;
    let table = req_str(args, "table")?;
    let schema = schema_of(src, args);
    let columns =
        introspect::list_columns(ctx.state, &src.conn.id, &src.database, schema, table).await?;
    Ok(Done::plain(
        count(columns.len(), "column", "columns"),
        json!({ "table": table, "columns": columns }),
    ))
}

/// The source of an engine's read tool, and the text it runs: `sql` for SQL sources, `query`
/// (mongosh) for MongoDB. A source of the other engine gets the other tool's name.
fn read_source<'a>(
    ctx: &'a ToolContext<'_>,
    args: &'a Map<String, Value>,
    mongo: bool,
    other_tool: &str,
) -> Result<(&'a Source, &'a str), AppError> {
    let src = ctx.source(args)?;
    if (src.conn.engine == Engine::Mongo) != mongo {
        return Err(wrong_engine(src, other_tool));
    }
    let text = req_str(args, if mongo { "query" } else { "sql" })?;
    Ok((src, text))
}

/// `run_read_query` (`mongo` false) and `run_mongo_read` (`mongo` true).
async fn run_read_query(
    ctx: &ToolContext<'_>,
    args: &Map<String, Value>,
    mongo: bool,
) -> Result<Done, AppError> {
    let other = if mongo {
        "run_read_query"
    } else {
        "run_mongo_read"
    };
    let (src, text) = read_source(ctx, args, mongo, other)?;
    let r = readonly::run_ai_read(
        ctx.state,
        &src.conn.id,
        Some(&src.database),
        text,
        limits(ctx),
    )
    .await?;
    let n = r.rows.len();
    let more = if r.truncated {
        " (row limit reached)"
    } else {
        ""
    };
    let summary = format!("{}{more} · {} ms", count(n, "row", "rows"), r.elapsed_ms);
    let content = read_content(&r, ctx.settings.privacy);
    let columns: Vec<String> = r.columns.into_iter().map(|c| c.name).collect();
    let ui_truncated = r.truncated || n > UI_ROWS;
    let mut rows = r.rows;
    rows.truncate(UI_ROWS);
    Ok(Done {
        summary,
        columns: Some(columns),
        rows: Some(rows),
        truncated: Some(ui_truncated),
        preview: None,
        content,
        write: None,
    })
}

/// The model's view of a read. `schema` privacy: names and counts only, never a row value.
/// A MongoDB read also has `documents`: the whole documents, not the flattened rows.
fn read_content(r: &AiReadResult, privacy: Privacy) -> Value {
    let columns: Vec<Value> = r
        .columns
        .iter()
        .map(|c| json!({"name": c.name, "type": c.data_type}))
        .collect();
    let mut out = json!({
        "columns": columns,
        "rowCount": r.rows.len(),
        "truncated": r.truncated,
    });
    if privacy == Privacy::Schema {
        out["note"] = json!("Row values are hidden by the privacy setting.");
        return out;
    }
    // Whole rows only, so the JSON stays valid; the cap leaves room for the other fields.
    let budget = MAX_CONTENT_BYTES.saturating_sub(out.to_string().len() + 200);
    if let Some(documents) = &r.documents {
        // MongoDB: the model gets the documents themselves, not the flattened grid.
        let mut used = 0;
        let mut docs = Vec::new();
        for doc in documents {
            let len = serde_json::to_string(doc).map_or(0, |s| s.len()) + 1;
            if used + len > budget {
                break;
            }
            used += len;
            docs.push(doc.clone());
        }
        if docs.len() < documents.len() {
            out["note"] = json!(format!(
                "Only the first {} documents fit in the 32 KiB tool output limit.",
                docs.len()
            ));
        }
        out["documents"] = Value::from(docs);
        return out;
    }
    let mut used = 0;
    let mut rows = Vec::new();
    for row in &r.rows {
        let len = serde_json::to_string(row).map_or(0, |s| s.len()) + 1;
        if used + len > budget {
            break;
        }
        used += len;
        rows.push(row.clone());
    }
    if rows.len() < r.rows.len() {
        out["note"] = json!(format!(
            "Only the first {} rows fit in the 32 KiB tool output limit.",
            rows.len()
        ));
    }
    out["rows"] = Value::from(rows);
    out
}

fn limits(ctx: &ToolContext<'_>) -> ReadLimits {
    ReadLimits {
        max_rows: ctx.settings.max_rows as usize,
        timeout: Duration::from_secs(ctx.settings.timeout_sec),
    }
}

/// What the UI draws. Every column name is a column of the result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub x: String,
    pub y: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    pub stacked: bool,
}

/// The chart arguments. Checked before the query runs; the column names are checked after it.
fn chart_spec(args: &Map<String, Value>) -> Result<ChartSpec, AppError> {
    let kind = req_str(args, "type")?;
    if !CHART_TYPES.contains(&kind) {
        return Err(AppError::invalid(format!(
            "\"type\" must be one of: {}",
            CHART_TYPES.join(", ")
        )));
    }
    let x = req_str(args, "x")?.to_string();
    // Some models send one column as a plain string.
    let y: Vec<String> = match args.get("y") {
        Some(Value::String(s)) => vec![s.trim().to_string()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_string())
            .collect(),
        _ => Vec::new(),
    };
    if y.is_empty() || y.iter().any(String::is_empty) {
        return Err(AppError::invalid(
            "The tool needs \"y\": a list of value column names",
        ));
    }
    if y.len() > MAX_SERIES {
        return Err(AppError::invalid(format!(
            "A chart takes at most {MAX_SERIES} y columns"
        )));
    }
    let series = opt_str(args, "series").map(str::to_string);
    if kind == "pie" && (y.len() != 1 || series.is_some()) {
        return Err(AppError::invalid(
            "A pie chart takes exactly one y column and no series",
        ));
    }
    if series.is_some() && y.len() != 1 {
        return Err(AppError::invalid(
            "With \"series\", give exactly one y column",
        ));
    }
    Ok(ChartSpec {
        kind: kind.to_string(),
        title: opt_str(args, "title").map(str::to_string),
        x,
        y,
        series,
        stacked: args.get("stacked").and_then(Value::as_bool) == Some(true),
    })
}

/// Every column the chart names is in the result. The error lists the result's columns, so the
/// model can fix its arguments; names are schema, never row values.
fn check_columns(spec: &ChartSpec, columns: &[String]) -> Result<(), AppError> {
    let missing: Vec<String> = std::iter::once(&spec.x)
        .chain(&spec.y)
        .chain(&spec.series)
        .filter(|c| !columns.contains(c))
        .map(|c| quoted(c))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let have: Vec<String> = columns.iter().map(|c| quoted(c)).collect();
    Err(AppError::invalid(format!(
        "The result has no column {}. Its columns: {}",
        missing.join(", "),
        have.join(", ")
    )))
}

fn quoted(name: &str) -> String {
    Value::from(name).to_string()
}

/// `render_chart` (`mongo` false) and `render_mongo_chart` (`mongo` true).
async fn render_chart(
    ctx: &ToolContext<'_>,
    id: &str,
    args: &Map<String, Value>,
    mongo: bool,
) -> Result<Done, AppError> {
    let other = if mongo {
        "render_chart"
    } else {
        "render_mongo_chart"
    };
    let (src, text) = read_source(ctx, args, mongo, other)?;
    let spec = chart_spec(args)?;
    let r = readonly::run_ai_read(
        ctx.state,
        &src.conn.id,
        Some(&src.database),
        text,
        limits(ctx),
    )
    .await?;
    let columns: Vec<String> = r.columns.iter().map(|c| c.name.clone()).collect();
    check_columns(&spec, &columns)?;
    let n = r.rows.len();
    let more = if r.truncated {
        " (row limit reached)"
    } else {
        ""
    };
    let mut kind = spec.kind.clone();
    kind[..1].make_ascii_uppercase();
    let summary = format!(
        "{kind} chart · {}{more} · {} ms",
        count(n, "row", "rows"),
        r.elapsed_ms
    );
    let mut content = read_content(&r, ctx.settings.privacy);
    content["chart"] = json!(if n > CHART_ROWS {
        format!("Shown to the user. It draws the first {CHART_ROWS} rows.")
    } else {
        "Shown to the user.".to_string()
    });
    let truncated = r.truncated || n > CHART_ROWS;
    let mut rows = r.rows;
    rows.truncate(CHART_ROWS);
    if mongo {
        // The flattener gives int64 beyond 2^53 and Decimal128 as text; the chart's y values
        // are numbers when they are finite.
        let y: Vec<usize> = spec
            .y
            .iter()
            .filter_map(|name| columns.iter().position(|c| c == name))
            .collect();
        for row in &mut rows {
            for &i in &y {
                if let Some(cell) = row.get_mut(i) {
                    numeric_cell(cell);
                }
            }
        }
    }
    let mut done = Done::plain(summary, content);
    done.preview = Some(ChatEvent::Chart {
        id: id.to_string(),
        spec,
        columns,
        rows,
        truncated,
    });
    Ok(done)
}

/// A text cell that is a plain decimal number becomes a JSON number when it is finite;
/// anything else stays as it is.
fn numeric_cell(v: &mut Value) {
    if let Value::String(s) = v
        && looks_numeric(s)
        && let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64)
    {
        *v = Value::Number(n);
    }
}

/// `^-?(\d+(\.\d+)?|\.\d+)([eE][+-]?\d+)?$` without a regex dependency.
fn looks_numeric(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i = 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

fn propose_mongo_write(
    ctx: &ToolContext<'_>,
    id: &str,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let src = ctx.source(args)?;
    if src.conn.engine != Engine::Mongo {
        return Err(wrong_engine(src, "propose_write"));
    }
    let text = req_str(args, "query")?;
    writes_allowed(src)?;
    // Parse here so the preview never shows text that does not parse; the write parses again
    // when it runs.
    let s = mongo::ai::one_statement(text)?;
    if matches!(s.command, mongo::command::Command::Use(_)) {
        return Err(AppError::invalid(
            "\"use\" is not a write: the statement runs on the source's database",
        ));
    }
    if mongo_guard::kind(&s.command) == mongo_guard::Kind::Read {
        return Err(AppError::invalid("This is a read. Use run_mongo_read."));
    }
    let info = mongo_guard::classify_write(text, Some(&src.database));
    let mut done = Done::plain(String::new(), Value::Null);
    done.preview = Some(ChatEvent::WritePreview {
        id: id.to_string(),
        sql: text.to_string(),
        info,
    });
    done.write = Some(ProposedWrite {
        conn_id: src.conn.id.clone(),
        database: src.database.clone(),
        sql: text.to_string(),
    });
    Ok(done)
}

fn propose_write(
    ctx: &ToolContext<'_>,
    id: &str,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let src = ctx.source(args)?;
    if src.conn.engine == Engine::Mongo {
        return Err(wrong_engine(src, "propose_mongo_write"));
    }
    let sql = req_str(args, "sql")?;
    writes_allowed(src)?;
    let info = guard::classify_write(sql, src.conn.engine, None);
    let mut done = Done::plain(String::new(), Value::Null);
    done.preview = Some(ChatEvent::WritePreview {
        id: id.to_string(),
        sql: sql.to_string(),
        info,
    });
    done.write = Some(ProposedWrite {
        conn_id: src.conn.id.clone(),
        database: src.database.clone(),
        sql: sql.to_string(),
    });
    Ok(done)
}

/// The assistant may propose writes on `src`: the connection allows writes and the chat's
/// write mode is on for it.
fn writes_allowed(src: &Source) -> Result<(), AppError> {
    if !conn_ro::writes_allowed(&src.conn) {
        return Err(AppError::new(
            ErrorCode::WriteBlocked,
            format!(
                "Connection \"{}\" is read-only, so the assistant cannot propose writes",
                src.conn.name
            ),
        ));
    }
    if !src.allow_writes {
        return Err(AppError::new(
            ErrorCode::WriteBlocked,
            format!(
                "Write mode is off on \"{}\", so the assistant cannot propose writes",
                src.conn.name
            ),
        ));
    }
    Ok(())
}

/// Cuts `s` to `MAX_CONTENT_BYTES` on a char boundary and adds a marker.
fn cap(mut s: String) -> String {
    if s.len() <= MAX_CONTENT_BYTES {
        return s;
    }
    let mut end = MAX_CONTENT_BYTES - CUT_MARKER.len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str(CUT_MARKER);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::query::ColumnMeta;

    fn read(rows: Vec<Vec<Value>>) -> AiReadResult {
        AiReadResult {
            columns: vec![ColumnMeta {
                name: "email".into(),
                data_type: "TEXT".into(),
                mixed: false,
            }],
            rows,
            truncated: false,
            elapsed_ms: 3,
            documents: None,
        }
    }

    #[test]
    fn schema_privacy_never_has_values() {
        let r = read(vec![vec![json!("alice@example.com")]]);
        let v = read_content(&r, Privacy::Schema);
        assert!(!v.to_string().contains("alice"), "{v}");
        assert_eq!(v["rowCount"], 1);
        assert_eq!(v["columns"][0]["name"], "email");
        let v = read_content(&r, Privacy::Results);
        assert_eq!(v["rows"][0][0], "alice@example.com");
    }

    #[test]
    fn results_are_cut_by_whole_rows() {
        let rows = (0..2000)
            .map(|i| vec![json!(format!("value-{i:05}-padding"))])
            .collect();
        let v = read_content(&read(rows), Privacy::Results);
        let s = v.to_string();
        assert!(s.len() <= MAX_CONTENT_BYTES, "{}", s.len());
        let shown = v["rows"].as_array().unwrap().len();
        assert!(shown > 100 && shown < 2000, "{shown}");
        assert_eq!(v["rowCount"], 2000);
        assert!(
            v["note"]
                .as_str()
                .unwrap()
                .contains(&format!("first {shown} rows"))
        );
    }

    #[test]
    fn cap_marks_the_cut() {
        let s = "é".repeat(MAX_CONTENT_BYTES);
        let c = cap(s);
        assert!(c.len() <= MAX_CONTENT_BYTES);
        assert!(c.ends_with(CUT_MARKER));
        assert_eq!(cap("short".into()), "short");
    }

    #[test]
    fn args_parse() {
        assert_eq!(parse_args("").unwrap().len(), 0);
        assert_eq!(
            parse_args(r#"{"sql": "SELECT 1"}"#).unwrap()["sql"],
            "SELECT 1"
        );
        assert!(parse_args("[1]").is_none());
        assert!(parse_args("{bad").is_none());
    }

    fn names(defs: &Value) -> Vec<&str> {
        defs.as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect()
    }

    fn tool<'a>(defs: &'a Value, name: &str) -> &'a Value {
        defs.as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .unwrap()
    }

    #[test]
    fn sql_only_chats_get_the_sql_tools_unchanged() {
        let defs = definitions(&[source("db1", "a")]);
        assert_eq!(
            names(&defs),
            [
                "list_tables",
                "describe_table",
                "run_read_query",
                "render_chart",
                "propose_write"
            ]
        );
        // No MongoDB wording reaches a SQL-only chat.
        assert!(!defs.to_string().contains("Mongo"), "{defs}");
        assert!(!defs.to_string().contains("mongosh"), "{defs}");
        assert_eq!(
            tool(&defs, "list_tables")["function"]["description"],
            "List the tables and views of one database."
        );
    }

    #[test]
    fn mongo_only_chats_get_no_sql_tools() {
        let defs = definitions(&[mongo_source("db1", "m")]);
        assert_eq!(
            names(&defs),
            [
                "list_tables",
                "describe_table",
                "run_mongo_read",
                "render_mongo_chart",
                "propose_mongo_write"
            ]
        );
        assert_eq!(
            tool(&defs, "list_tables")["function"]["description"],
            "List the tables and views (MongoDB: collections and views) of one database."
        );
        // The hints that name the other engine's tools are for mixed chats only.
        assert!(!defs.to_string().contains("run_read_query"), "{defs}");
        let read = &tool(&defs, "run_mongo_read")["function"]["parameters"];
        assert_eq!(read["required"], json!(["query"]));
    }

    #[test]
    fn mixed_chats_split_the_source_enum_per_engine() {
        let defs = definitions(&[source("db1", "pg"), mongo_source("db2", "m")]);
        assert_eq!(defs.as_array().unwrap().len(), 8);
        let ids = |name: &str| {
            tool(&defs, name)["function"]["parameters"]["properties"]["source"]["enum"].clone()
        };
        for name in ["list_tables", "describe_table"] {
            assert_eq!(ids(name), json!(["db1", "db2"]), "{name}");
        }
        for name in ["run_read_query", "render_chart", "propose_write"] {
            assert_eq!(ids(name), json!(["db1"]), "{name}");
        }
        for name in [
            "run_mongo_read",
            "render_mongo_chart",
            "propose_mongo_write",
        ] {
            assert_eq!(ids(name), json!(["db2"]), "{name}");
        }
        // Several sources: every tool must name one.
        for d in defs.as_array().unwrap() {
            let required = d["function"]["parameters"]["required"].as_array().unwrap();
            assert!(required.contains(&json!("source")), "{d}");
        }
        assert!(
            tool(&defs, "run_read_query")["function"]["description"]
                .as_str()
                .unwrap()
                .ends_with("SQL sources only: on MongoDB use run_mongo_read.")
        );
    }

    /// One source: `source` is optional, so a model that never names it still works. More
    /// sources: every tool must name one of them.
    #[test]
    fn source_is_required_only_with_several_sources() {
        for d in definitions(&[source("db1", "a")]).as_array().unwrap() {
            let p = &d["function"]["parameters"];
            assert_eq!(p["properties"]["source"]["enum"], json!(["db1"]));
            let required = p["required"].as_array().cloned().unwrap_or_default();
            assert!(!required.contains(&json!("source")), "{d}");
        }
        let two = definitions(&[source("db1", "a"), source("db2", "b")]);
        for d in two.as_array().unwrap() {
            let p = &d["function"]["parameters"];
            assert_eq!(p["properties"]["source"]["enum"], json!(["db1", "db2"]));
            assert!(
                p["required"].as_array().unwrap().contains(&json!("source")),
                "{d}"
            );
        }
        let describe = &two[1]["function"]["parameters"]["required"];
        assert_eq!(describe, &json!(["table", "source"]));
    }

    fn source(id: &str, name: &str) -> Source {
        use crate::core::connections::{Env, SslMode};
        Source {
            id: id.into(),
            conn: Connection {
                id: format!("c-{id}"),
                name: name.into(),
                engine: Engine::Pg,
                version: String::new(),
                env: Env::Local,
                host: "h".into(),
                port: 5432,
                database: "shop".into(),
                user: "u".into(),
                ssl: SslMode::Prefer,
                file: String::new(),
                ai_write: false,
                read_only: false,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            database: "shop".into(),
            schema: None,
            allow_writes: false,
        }
    }

    fn mongo_source(id: &str, name: &str) -> Source {
        let mut s = source(id, name);
        s.conn.engine = Engine::Mongo;
        s.conn.port = 27017;
        s
    }

    fn error_of(out: &ToolOutput) -> &AppError {
        match &out.events[..] {
            [ChatEvent::ToolResult { error: Some(e), .. }] => e,
            other => panic!("{other:?}"),
        }
    }

    /// A tool of the other engine is refused before anything runs, with the tool to use.
    #[tokio::test]
    async fn wrong_engine_names_the_right_tool() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let sources = [source("db1", "pg"), mongo_source("db2", "m")];
        let ctx = ToolContext {
            state: &state,
            sources: &sources,
            settings: &settings,
        };
        let chart = r#""type": "bar", "x": "a", "y": ["b"]"#;
        for (tool, args, want) in [
            (
                "run_read_query",
                r#"{"source": "db2", "sql": "SELECT 1"}"#.to_string(),
                "Source db2 is MongoDB. Use run_mongo_read with a mongosh query.",
            ),
            (
                "render_chart",
                format!(r#"{{"source": "db2", "sql": "SELECT 1", {chart}}}"#),
                "Source db2 is MongoDB. Use render_mongo_chart with a mongosh query.",
            ),
            (
                "propose_write",
                r#"{"source": "db2", "sql": "DELETE FROM t"}"#.to_string(),
                "Source db2 is MongoDB. Use propose_mongo_write with a mongosh query.",
            ),
            (
                "run_mongo_read",
                r#"{"source": "db1", "query": "db.a.find()"}"#.to_string(),
                "Source db1 is PostgreSQL. Use run_read_query with SQL.",
            ),
            (
                "render_mongo_chart",
                format!(r#"{{"source": "db1", "query": "db.a.find()", {chart}}}"#),
                "Source db1 is PostgreSQL. Use render_chart with SQL.",
            ),
            (
                "propose_mongo_write",
                r#"{"source": "db1", "query": "db.a.deleteMany({})"}"#.to_string(),
                "Source db1 is PostgreSQL. Use propose_write with SQL.",
            ),
        ] {
            let out = execute(&ctx, "t", tool, &args).await;
            let e = error_of(&out);
            assert_eq!(e.code, ErrorCode::InvalidInput, "{tool}");
            assert_eq!(e.message, want, "{tool}");
        }
    }

    #[tokio::test]
    async fn propose_mongo_write_checks_and_previews() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let mut writable = mongo_source("db2", "staging");
        writable.allow_writes = true;
        let sources = [mongo_source("db1", "prod"), writable];
        let ctx = ToolContext {
            state: &state,
            sources: &sources,
            settings: &settings,
        };
        let call =
            |source: &str, query: &str| json!({"source": source, "query": query}).to_string();

        // Write mode off: the same refusal as propose_write.
        let out = execute(
            &ctx,
            "w",
            "propose_mongo_write",
            &call("db1", "db.a.deleteMany({})"),
        )
        .await;
        let e = error_of(&out);
        assert_eq!(e.code, ErrorCode::WriteBlocked);
        assert!(e.message.contains("\"prod\""), "{}", e.message);

        for (query, want) in [
            ("db.a.find()", "This is a read. Use run_mongo_read."),
            ("use admin", "\"use\" is not a write"),
            (
                "db.a.deleteOne({})\ndb.a.deleteOne({})",
                "Exactly one statement",
            ),
            ("db.a.deleteOne({", "Line 1, column"),
        ] {
            let out = execute(&ctx, "w", "propose_mongo_write", &call("db2", query)).await;
            let e = error_of(&out);
            assert_eq!(e.code, ErrorCode::InvalidInput, "{query}");
            assert!(e.message.starts_with(want), "{query}: {}", e.message);
        }

        // Nothing runs: only the preview, with the Mongo classification.
        let query = "db.orders.deleteMany({})";
        let out = execute(&ctx, "w", "propose_mongo_write", &call("db2", query)).await;
        match &out.events[..] {
            [ChatEvent::WritePreview { sql, info, .. }] => {
                assert_eq!(sql, query);
                assert_eq!(info, &mongo_guard::classify_write(query, Some("shop")));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            out.write,
            Some(ProposedWrite {
                conn_id: "c-db2".into(),
                database: "shop".into(),
                sql: query.into(),
            })
        );
    }

    fn mongo_read() -> AiReadResult {
        AiReadResult {
            columns: vec![
                ColumnMeta {
                    name: "_id".into(),
                    data_type: "objectId".into(),
                    mixed: false,
                },
                ColumnMeta {
                    name: "customer.email".into(),
                    data_type: "string | null".into(),
                    mixed: true,
                },
            ],
            rows: vec![vec![json!("65a1"), json!("alice@example.com")]],
            truncated: false,
            elapsed_ms: 2,
            documents: Some(vec![json!({
                "_id": {"$oid": "65a1"},
                "customer": {"email": "alice@example.com"},
            })]),
        }
    }

    #[test]
    fn mongo_read_content_follows_the_privacy_setting() {
        let r = mongo_read();
        let v = read_content(&r, Privacy::Schema);
        assert!(!v.to_string().contains("alice"), "{v}");
        assert!(
            v.get("documents").is_none() && v.get("rows").is_none(),
            "{v}"
        );
        assert_eq!(
            v["columns"][1],
            json!({"name": "customer.email", "type": "string | null"})
        );
        assert_eq!(v["rowCount"], 1);
        assert_eq!(v["note"], "Row values are hidden by the privacy setting.");

        // Full privacy: the whole documents, not the flattened rows.
        let v = read_content(&r, Privacy::Results);
        assert_eq!(v["documents"][0]["customer"]["email"], "alice@example.com");
        assert!(v.get("rows").is_none(), "{v}");

        // Cut by whole documents inside the budget.
        let mut big = mongo_read();
        let docs: Vec<Value> = (0..2000)
            .map(|i| json!({"v": format!("value-{i:05}-padding")}))
            .collect();
        big.rows = vec![vec![]; docs.len()];
        big.documents = Some(docs);
        let v = read_content(&big, Privacy::Results);
        assert!(v.to_string().len() <= MAX_CONTENT_BYTES);
        let shown = v["documents"].as_array().unwrap().len();
        assert!(shown > 100 && shown < 2000, "{shown}");
        assert!(
            v["note"]
                .as_str()
                .unwrap()
                .contains(&format!("first {shown} documents"))
        );
    }

    #[test]
    fn numeric_text_cells_become_numbers() {
        for (cell, want) in [
            (json!("9007199254740993"), json!(9007199254740993.0)),
            (json!("12.50"), json!(12.5)),
            (json!("-3e2"), json!(-300.0)),
            (json!(7), json!(7)),
            (json!("NaN"), json!("NaN")),
            (json!("Infinity"), json!("Infinity")),
            (json!("1e400"), json!("1e400")),
            (json!("abc"), json!("abc")),
            (json!(null), json!(null)),
        ] {
            let mut c = cell.clone();
            numeric_cell(&mut c);
            assert_eq!(c, want, "{cell}");
        }
    }

    /// A Mongo server error keeps only its code in `schema` privacy.
    #[test]
    fn schema_privacy_keeps_the_mongo_error_code() {
        let mut e = AppError::new(
            ErrorCode::Database,
            "E11000 duplicate key error dup key: { email: \"alice@example.com\" }",
        );
        e.server_code = Some("MongoDB error 11000".into());
        let v = model_error(&e, true);
        assert!(!v.to_string().contains("alice"), "{v}");
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .starts_with("The database refused the query (MongoDB error 11000)."),
            "{v}"
        );
    }

    #[test]
    fn tools_run_on_the_named_source() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let args = |v: Value| chart_args(v);

        let one = [source("db1", "a")];
        let ctx = ToolContext {
            state: &state,
            sources: &one,
            settings: &settings,
        };
        assert_eq!(ctx.source(&args(json!({}))).unwrap().id, "db1");

        let two = [source("db1", "a"), source("db2", "b")];
        let ctx = ToolContext {
            state: &state,
            sources: &two,
            settings: &settings,
        };
        assert_eq!(
            ctx.source(&args(json!({"source": "db2"})))
                .unwrap()
                .conn
                .name,
            "b"
        );
        // A guess must not land on the first database.
        let e = ctx.source(&args(json!({}))).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert_eq!(e.message, "The tool needs \"source\", one of: db1, db2");
        let e = ctx.source(&args(json!({"source": "db3"}))).unwrap_err();
        assert_eq!(e.message, "Unknown source \"db3\". Use one of: db1, db2");
    }

    #[tokio::test]
    async fn write_mode_is_per_source() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let mut writable = source("db2", "staging");
        writable.allow_writes = true;
        let sources = [source("db1", "prod"), writable];
        let ctx = ToolContext {
            state: &state,
            sources: &sources,
            settings: &settings,
        };
        let sql = r#"{"sql": "DELETE FROM t", "source": "db1"}"#;
        let out = execute(&ctx, "w1", "propose_write", sql).await;
        assert!(matches!(
            &out.events[..],
            [ChatEvent::ToolResult { ok: false, error: Some(e), .. }]
                if e.code == ErrorCode::WriteBlocked && e.message.contains("\"prod\"")
        ));
        let sql = r#"{"sql": "DELETE FROM t", "source": "db2"}"#;
        let out = execute(&ctx, "w2", "propose_write", sql).await;
        // Nothing runs yet: only the preview, and the write waits for the user.
        assert!(matches!(&out.events[..], [ChatEvent::WritePreview { .. }]));
        assert_eq!(
            out.write,
            Some(ProposedWrite {
                conn_id: "c-db2".into(),
                database: "shop".into(),
                sql: "DELETE FROM t".into(),
            })
        );
    }

    #[test]
    fn schema_privacy_hides_server_error_text() {
        let mut server = AppError::new(
            ErrorCode::Database,
            "invalid input syntax for type integer: \"alice@example.com\"",
        )
        .with_detail("error returned from database: alice@example.com");
        server.server_code = Some("SQLSTATE 22P02".into());
        let v = model_error(&server, true);
        assert!(!v.to_string().contains("alice"), "{v}");
        assert_eq!(v["code"], "database");
        assert!(
            v["message"].as_str().unwrap().contains("(SQLSTATE 22P02)"),
            "{v}"
        );
        server.server_code = None;
        let v = model_error(&server, true);
        assert!(!v.to_string().contains("alice"), "{v}");

        // Our own messages pass, without the detail.
        let guard = AppError::new(ErrorCode::GuardRejected, "Blocked: DELETE is not a read")
            .with_detail("guard detail");
        assert_eq!(
            model_error(&guard, true),
            json!({"code": "guard_rejected", "message": "Blocked: DELETE is not a read"})
        );

        // `results` privacy keeps everything so the model can fix its SQL.
        let v = model_error(&server, false);
        assert!(v["message"].as_str().unwrap().contains("alice"), "{v}");
        assert!(v["detail"].as_str().unwrap().contains("alice"), "{v}");
    }

    fn chart_args(v: Value) -> Map<String, Value> {
        let Value::Object(m) = v else { unreachable!() };
        m
    }

    #[test]
    fn chart_spec_takes_valid_arguments() {
        let s = chart_spec(&chart_args(json!({
            "sql": "SELECT 1", "type": "bar", "x": "month", "y": ["revenue", "orders"],
            "title": "Sales", "stacked": true
        })))
        .unwrap();
        assert_eq!(s.y, ["revenue", "orders"]);
        assert!(s.stacked);
        // A model that sends one column as a string still gets its chart.
        let s = chart_spec(&chart_args(
            json!({"type": "line", "x": "day", "y": "total"}),
        ))
        .unwrap();
        assert_eq!(s.y, ["total"]);
        assert!(!s.stacked);
        // The UI reads `type`, not the Rust field name.
        assert_eq!(serde_json::to_value(&s).unwrap()["type"], "line");
    }

    #[test]
    fn chart_spec_refuses_charts_the_ui_cannot_draw() {
        for (args, want) in [
            (json!({"type": "radar", "x": "a", "y": ["b"]}), "\"type\""),
            (json!({"type": "bar", "x": "a", "y": []}), "\"y\""),
            (json!({"type": "bar", "x": "a", "y": [""]}), "\"y\""),
            (json!({"type": "pie", "x": "a", "y": ["b", "c"]}), "pie"),
            (
                json!({"type": "pie", "x": "a", "y": ["b"], "series": "c"}),
                "pie",
            ),
            (
                json!({"type": "line", "x": "a", "y": ["b", "c"], "series": "d"}),
                "series",
            ),
            (
                json!({"type": "bar", "x": "a", "y": ["1","2","3","4","5","6","7","8","9"]}),
                "at most 8",
            ),
        ] {
            let e = chart_spec(&chart_args(args.clone())).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{args}");
            assert!(e.message.contains(want), "{args}: {}", e.message);
        }
    }

    #[test]
    fn missing_chart_column_names_the_result_columns() {
        let spec = chart_spec(&chart_args(
            json!({"type": "bar", "x": "month", "y": ["revenue"], "series": "region"}),
        ))
        .unwrap();
        let cols = [
            "month".to_string(),
            "total".to_string(),
            "region".to_string(),
        ];
        let e = check_columns(&spec, &cols).unwrap_err();
        // Own message, so it reaches the model under `schema` privacy and the model can fix it.
        assert!(OWN_MESSAGE_CODES.contains(&e.code));
        assert_eq!(
            e.message,
            "The result has no column \"revenue\". Its columns: \"month\", \"total\", \"region\""
        );
        let cols = [
            "month".to_string(),
            "revenue".to_string(),
            "region".to_string(),
        ];
        assert!(check_columns(&spec, &cols).is_ok());
    }
}
