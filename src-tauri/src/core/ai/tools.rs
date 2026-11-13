//! The tools the model can call. One table lists every tool: the definitions sent to the model,
//! the dispatcher, the privacy rule, the write result and the UI kinds all come from it, so a
//! new tool is one row and no caller can disagree about a name.
//!
//! Reads go through the AI read path (guard, read-only session, limits); one read executor builds
//! the summary and the model's view for both `run_read_query` and `render_chart`, and a chart is
//! that read plus a preview. `propose_write` shows the statement to the user and runs it only
//! after the user approves it.

use std::pin::Pin;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::core::ai::approval::APPROVAL_TIMEOUT;
use crate::core::ai::event::{ChatEvent, ToolKind, ToolResultData};
use crate::core::ai::settings::{AiSettings, Privacy};
use crate::core::engine::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::ops;
use crate::core::result::{AiReadResult, ChartSpec, Engines, Source};
use crate::core::state::AppState;

/// Largest tool output sent to the model; longer output is cut with a marker.
pub const MAX_CONTENT_BYTES: usize = 32 * 1024;
/// `MAX_CONTENT_BYTES` in KiB, for the texts the model reads.
const MAX_CONTENT_KIB: usize = MAX_CONTENT_BYTES / 1024;
/// Rows of a query result sent to the UI.
pub const UI_ROWS: usize = 50;
/// Rows a chart keeps. Charts are saved with the chat, so this stays small.
pub const CHART_ROWS: usize = 1000;
pub const CHART_TYPES: [&str; 5] = ["bar", "line", "area", "pie", "scatter"];
/// Most value columns of one chart.
const MAX_SERIES: usize = 8;

/// Which sources a tool serves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Serves {
    All,
    Sql,
    Mongo,
}

impl Serves {
    fn matches(self, engine: Engine) -> bool {
        match self {
            Self::All => true,
            Self::Sql => engine != Engine::Mongo,
            Self::Mongo => engine == Engine::Mongo,
        }
    }
}

/// A tool's definition without `source`: the sentence the model reads and the parameters it
/// fills. `mongo`: the chat has a MongoDB source, which changes the wording of the two schema
/// tools.
type Build = fn(&'static Tool, bool) -> (&'static str, Value);

/// A tool's handler. The table passes its row, so one function serves the SQL and the MongoDB
/// tool of the same kind.
type Handler = for<'a, 'b> fn(
    &'static Tool,
    &'a ToolContext<'b>,
    &'a str,
    &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>>;

/// One tool. Everything that differs between tools is a field here.
struct Tool {
    name: &'static str,
    serves: Serves,
    kind: ToolKind,
    build: Build,
    /// The argument that holds the statement (`sql`, `query`); `None` for the schema tools.
    text: Option<&'static str>,
    /// The tool of the other engine that does the same thing.
    other: Option<&'static str>,
    /// A server error message can quote a row value, so `schema` privacy hides its text.
    data: bool,
    handler: Handler,
}

impl Tool {
    /// The other engine's twin, for a source this tool does not serve.
    fn other(&self) -> &'static str {
        self.other
            .expect("every engine-specific tool names its other engine's tool")
    }

    /// The statement argument's key; a table invariant.
    fn text_key(&self) -> &'static str {
        self.text
            .expect("every statement tool names its text argument")
    }
}

/// Every tool, in the order the model sees the definitions.
static TOOLS: [Tool; 8] = [
    Tool {
        name: "list_tables",
        serves: Serves::All,
        kind: ToolKind::List,
        build: build_list,
        text: None,
        other: None,
        data: false,
        handler: handle_list,
    },
    Tool {
        name: "describe_table",
        serves: Serves::All,
        kind: ToolKind::Describe,
        build: build_describe,
        text: None,
        other: None,
        data: false,
        handler: handle_describe,
    },
    Tool {
        name: "run_read_query",
        serves: Serves::Sql,
        kind: ToolKind::Read,
        build: build_read,
        text: Some("sql"),
        other: Some("run_mongo_read"),
        data: true,
        handler: handle_read,
    },
    Tool {
        name: "render_chart",
        serves: Serves::Sql,
        kind: ToolKind::Chart,
        build: build_chart,
        text: Some("sql"),
        other: Some("render_mongo_chart"),
        data: true,
        handler: handle_chart,
    },
    Tool {
        name: "propose_write",
        serves: Serves::Sql,
        kind: ToolKind::Write,
        build: build_write,
        text: Some("sql"),
        other: Some("propose_mongo_write"),
        data: true,
        handler: handle_write,
    },
    Tool {
        name: "run_mongo_read",
        serves: Serves::Mongo,
        kind: ToolKind::Read,
        build: build_read,
        text: Some("query"),
        other: Some("run_read_query"),
        data: true,
        handler: handle_read,
    },
    Tool {
        name: "render_mongo_chart",
        serves: Serves::Mongo,
        kind: ToolKind::Chart,
        build: build_chart,
        text: Some("query"),
        other: Some("render_chart"),
        data: true,
        handler: handle_chart,
    },
    Tool {
        name: "propose_mongo_write",
        serves: Serves::Mongo,
        kind: ToolKind::Write,
        build: build_write,
        text: Some("query"),
        other: Some("propose_write"),
        data: true,
        handler: handle_write,
    },
];

fn tool_named(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The kind of a tool by name; `Other` for a name this build does not have, so the call still
/// streams (the UI can show it) and then fails as `not_found`.
pub fn kind(name: &str) -> ToolKind {
    tool_named(name).map_or(ToolKind::Other, |t| t.kind)
}

fn tool_of(kind: ToolKind, engine: Engine) -> &'static Tool {
    TOOLS
        .iter()
        .find(|t| t.kind == kind && t.serves.matches(engine))
        .expect("every engine has the tool of each kind")
}

/// The tools as data: their names and kinds, for the table test.
#[cfg(test)]
pub fn catalog() -> impl Iterator<Item = (&'static str, ToolKind)> {
    TOOLS.iter().map(|t| (t.name, t.kind))
}

/// OpenAI function tool definitions for the engines of `sources`. The schema tools serve every
/// source; the SQL read/chart/write tools exist when a SQL source does, the Mongo ones when a
/// Mongo source does. Every tool takes `source`, an enum of the ids it serves; it is required
/// when the chat has more than one source.
pub fn definitions(sources: &[Source], engines: Engines) -> Value {
    let ids = |serves: Serves| -> Vec<&str> {
        sources
            .iter()
            .filter(|s| serves.matches(s.conn.engine))
            .map(|s| s.id.as_str())
            .collect()
    };
    let mut defs = Vec::new();
    for t in &TOOLS {
        let ids = ids(t.serves);
        if ids.is_empty() {
            continue;
        }
        let (description, mut params) = (t.build)(t, engines.mongo);
        // A mixed SQL + MongoDB chat says which twin to use on the other engine.
        let description = match t.serves {
            Serves::Sql if engines.is_mixed() => {
                format!("{description} SQL sources only: on MongoDB use {}.", t.other())
            }
            Serves::Mongo if engines.is_mixed() => {
                format!("{description} MongoDB sources only: on SQL sources use {}.", t.other())
            }
            _ => description.to_string(),
        };
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
        defs.push(json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": description,
                "parameters": params,
            }
        }));
    }
    Value::Array(defs)
}

fn build_list(_: &Tool, mongo: bool) -> (&'static str, Value) {
    let description = if mongo {
        "List the tables and views (MongoDB: collections and views) of one database."
    } else {
        "List the tables and views of one database."
    };
    let schema = if mongo {
        "PostgreSQL schema. Default: the current schema. Ignored on MySQL, SQLite and MongoDB."
    } else {
        "PostgreSQL schema. Default: the current schema. Ignored on MySQL and SQLite."
    };
    (
        description,
        json!({
            "type": "object",
            "properties": {"schema": {"type": "string", "description": schema}}
        }),
    )
}

fn build_describe(_: &Tool, mongo: bool) -> (&'static str, Value) {
    let (description, table, schema) = if mongo {
        (
            "List the columns of one table or view: name, type, nullable, default, primary key. On MongoDB, the fields from a sample, with their types and the share of documents that have them.",
            "Table or collection name without schema.",
            "PostgreSQL schema. Default: the current schema. Ignored on MongoDB.",
        )
    } else {
        (
            "List the columns of one table or view: name, type, nullable, default, primary key.",
            "Table name without schema.",
            "PostgreSQL schema. Default: the current schema.",
        )
    };
    (
        description,
        json!({
            "type": "object",
            "properties": {
                "table": {"type": "string", "description": table},
                "schema": {"type": "string", "description": schema}
            },
            "required": ["table"]
        }),
    )
}

/// The chart arguments, after the statement: they are the same for both engines, but MongoDB
/// names result fields where SQL names columns.
fn chart_params(mongo: bool) -> Value {
    let (x, y, series) = if mongo {
        (
            "Result field for the x axis (pie: the slice labels).",
            "Result fields with the numeric values, one series each (pie: exactly one).",
            "Optional result field that splits one y field into one series per distinct value (long format). Not for pie.",
        )
    } else {
        (
            "Result column for the x axis (pie: the slice labels).",
            "Result columns with the numeric values, one series each (pie: exactly one).",
            "Optional result column that splits one y column into one series per distinct value (long format). Not for pie.",
        )
    };
    json!({
        "type": {"type": "string", "enum": CHART_TYPES, "description": "bar, line, area, pie or scatter."},
        "title": {"type": "string", "description": "Short chart title."},
        "x": {"type": "string", "description": x},
        "y": {"type": "array", "items": {"type": "string"}, "description": y},
        "series": {"type": "string", "description": series},
        "stacked": {"type": "boolean", "description": "Stack the series (bar and area)."}
    })
}

fn build_read(t: &Tool, _mongo: bool) -> (&'static str, Value) {
    let key = t.text_key();
    let (description, statement) = if t.serves == Serves::Mongo {
        (
            "Run one read-only mongosh statement and get the result. Exactly one statement: db.<collection>.find(...), aggregate([...]), countDocuments, distinct, findOne, estimatedDocumentCount, or a read runCommand. Writes are refused.",
            "Exactly one read-only mongosh statement, for example db.orders.find({status: \"paid\"}, {total: 1}).sort({_id: -1}).limit(20) or db.orders.aggregate([...]).",
        )
    } else {
        (
            "Run one read-only SQL statement (SELECT, SHOW, DESCRIBE, EXPLAIN, PRAGMA) and get the result. Writes are refused.",
            "Exactly one read-only statement.",
        )
    };
    (
        description,
        json!({
            "type": "object",
            "properties": {key: {"type": "string", "description": statement}},
            "required": [key]
        }),
    )
}

fn build_chart(t: &Tool, _mongo: bool) -> (&'static str, Value) {
    let key = t.text_key();
    let mongo = t.serves == Serves::Mongo;
    let description = if mongo {
        "Run one read-only mongosh statement and show its result to the user as a chart in the chat. This is the only way to show a chart on MongoDB: call it every time the user asks for a chart, graph or plot. The chart is also in the PDF export of the reply. Shape the data in the pipeline: one document per point, sorted, already aggregated; $project flat fields. x, y and series name result fields (flattened paths such as _id or month)."
    } else {
        "Run one read-only SQL statement and show its result to the user as a chart in the chat. This is the only way to show a chart: call it every time the user asks for a chart, graph or plot. The chart is also in the PDF export of the reply. Shape the data in SQL: one row per point, sorted, already aggregated."
    };
    let statement = if mongo {
        "Exactly one read-only mongosh statement whose result has one document per point."
    } else {
        "Exactly one read-only statement."
    };
    let mut properties = match chart_params(mongo) {
        Value::Object(map) => map,
        _ => unreachable!(),
    };
    properties.insert(
        key.to_string(),
        json!({"type": "string", "description": statement}),
    );
    (
        description,
        json!({
            "type": "object",
            "properties": properties,
            "required": [key, "type", "x", "y"]
        }),
    )
}

fn build_write(t: &Tool, _mongo: bool) -> (&'static str, Value) {
    let key = t.text_key();
    let (description, statement) = if t.serves == Serves::Mongo {
        (
            "Propose one mongosh write: insert, update, replace, delete, bulkWrite, findOneAnd*, index, drop, dropDatabase, rename, createCollection or a runCommand that writes. The user sees it and approves or denies it; only then does it run, and the result says whether it ran and how many documents changed. A single statement is atomic on one document only; multi-document operations are not one transaction.",
            "Exactly one mongosh write statement.",
        )
    } else {
        (
            "Propose SQL that changes data or schema. The user sees it and approves or denies it. The approved statements run in one transaction, except on MySQL: a statement that commits implicitly (DDL, LOCK TABLES, SET autocommit, CALL, GRANT) commits the earlier ones as it runs, and the preview warns about that. The result says whether it ran and how many rows changed.",
            "The SQL to propose.",
        )
    };
    (
        description,
        json!({
            "type": "object",
            "properties": {key: {"type": "string", "description": statement}},
            "required": [key]
        }),
    )
}

fn handle_list<'a, 'b>(
    _: &'static Tool,
    ctx: &'a ToolContext<'b>,
    _id: &'a str,
    args: &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>> {
    Box::pin(list_tables(ctx, args))
}

fn handle_describe<'a, 'b>(
    _: &'static Tool,
    ctx: &'a ToolContext<'b>,
    _id: &'a str,
    args: &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>> {
    Box::pin(describe_table(ctx, args))
}

fn handle_read<'a, 'b>(
    t: &'static Tool,
    ctx: &'a ToolContext<'b>,
    _id: &'a str,
    args: &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>> {
    Box::pin(read_tool(t, ctx, args))
}

fn handle_chart<'a, 'b>(
    t: &'static Tool,
    ctx: &'a ToolContext<'b>,
    id: &'a str,
    args: &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>> {
    Box::pin(chart_tool(t, ctx, id, args))
}

fn handle_write<'a, 'b>(
    t: &'static Tool,
    ctx: &'a ToolContext<'b>,
    id: &'a str,
    args: &'a Map<String, Value>,
) -> Pin<Box<dyn Future<Output = Result<Done, AppError>> + Send + 'a>> {
    Box::pin(write_tool(t, ctx, id, args))
}

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
    /// The source's engine; it decides which write tool the model's history names.
    pub engine: Engine,
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
    /// The numbers the summary states, for the UI.
    result: Option<ToolResultData>,
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
            result: None,
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
    let Some(t) = tool_named(name) else {
        return failed(
            id,
            AppError::not_found(format!("Unknown tool {name}")),
            false,
        );
    };
    let res = match parse_args(raw_args) {
        Some(args) => (t.handler)(t, ctx, id, &args).await,
        None => Err(AppError::invalid(
            "The tool arguments are not a JSON object",
        )),
    };
    output(t, ctx, id, res)
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
        Decision::Approved => {
            ops::run_approved_write(ctx.state, &w.conn_id, &w.database, &w.sql, stop)
                .await
                .map(|r| {
                    let mut done = Done::plain(
                        format!(
                            "{} · committed · {} ms",
                            count(r.affected as usize, "row changed", "rows changed"),
                            r.elapsed_ms
                        ),
                        json!({"status": "committed", "rowsAffected": r.affected}),
                    );
                    done.result = Some(ToolResultData {
                        rows: Some(r.affected),
                        ms: Some(r.elapsed_ms),
                        committed: Some(true),
                    });
                    done
                })
        }
    };
    // The tool name the model sees: the engine's write tool, so the model's history is honest.
    output(tool_of(ToolKind::Write, w.engine), ctx, id, res)
}

fn output(t: &Tool, ctx: &ToolContext<'_>, id: &str, res: Result<Done, AppError>) -> ToolOutput {
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
                result: d.result,
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
            // A database error can quote a row value: reads and writes both touch data.
            let hide = t.data && ctx.settings.privacy == Privacy::Schema;
            failed(id, e, hide)
        }
    }
}

/// The `tool_result` of a call that produced no UI part, and the model's view.
fn failed(id: &str, e: AppError, hide: bool) -> ToolOutput {
    let err = model_error(&e, hide);
    ToolOutput {
        events: vec![ChatEvent::ToolResult {
            id: id.to_string(),
            ok: false,
            summary: e.message.clone(),
            result: None,
            columns: None,
            rows: None,
            truncated: None,
            error: Some(e),
        }],
        content: cap(json!({ "error": err }).to_string()),
        write: None,
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
            Some(c) => format!("The database refused the query ({}). {hidden}", c.label()),
            None => format!("The query failed. {hidden}"),
        }
    };
    json!({"code": e.code, "message": message})
}

/// The source and statement of a tool that runs one statement. A source of the other engine is
/// refused with that engine's tool name.
fn statement<'a>(
    ctx: &'a ToolContext<'_>,
    t: &'static Tool,
    args: &'a Map<String, Value>,
) -> Result<(&'a Source, &'a str), AppError> {
    let src = ctx.source(args)?;
    if !t.serves.matches(src.conn.engine) {
        return Err(wrong_engine(src, t.other()));
    }
    Ok((src, req_str(args, t.text_key())?))
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
    let language = if src.conn.engine == Engine::Mongo {
        "a mongosh query"
    } else {
        "SQL"
    };
    AppError::invalid(format!(
        "Source {} is {}. Use {use_instead} with {language}.",
        src.id,
        src.conn.engine.label()
    ))
}

fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, AppError> {
    opt_str(args, key).ok_or_else(|| AppError::invalid(format!("The tool needs \"{key}\"")))
}

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
    let tables = ops::list_tables(ctx.state, &src.conn.id, &src.database, schema).await?;
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
        ops::list_columns(ctx.state, &src.conn.id, &src.database, schema, table).await?;
    Ok(Done::plain(
        count(columns.len(), "column", "columns"),
        json!({ "table": table, "columns": columns }),
    ))
}

/// One AI read, in the two views the tools need: the rows for the UI and the content for the
/// model. `run_read_query` and `render_chart` share it; a chart adds its preview.
struct Read {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    /// Rows the read returned, before the UI's cap.
    n: usize,
    /// More rows exist than the read fetched.
    truncated: bool,
    elapsed_ms: u64,
    content: Value,
}

/// The one read executor: the AI read path with the saved limits, then the typed summary and
/// the model's view, both built from the result.
async fn ai_read(ctx: &ToolContext<'_>, src: &Source, text: &str) -> Result<Read, AppError> {
    let r = ops::run_ai_read(
        ctx.state,
        &src.conn.id,
        Some(&src.database),
        text,
        ctx.settings.read_limits(),
    )
    .await?;
    let content = read_content(&r, ctx.settings.privacy);
    Ok(Read {
        columns: r.columns.into_iter().map(|c| c.name).collect(),
        n: r.rows.len(),
        truncated: r.truncated,
        elapsed_ms: r.elapsed_ms,
        rows: r.rows,
        content,
    })
}

impl Read {
    /// `5 rows · 42 ms`, with the read's own cut noted; `prefix` names the tool's result.
    fn summary(&self, prefix: &str) -> String {
        let more = if self.truncated {
            " (row limit reached)"
        } else {
            ""
        };
        format!(
            "{prefix}{}{more} · {} ms",
            count(self.n, "row", "rows"),
            self.elapsed_ms
        )
    }

    fn data(&self) -> ToolResultData {
        ToolResultData {
            rows: Some(self.n as u64),
            ms: Some(self.elapsed_ms),
            committed: None,
        }
    }

    /// The UI rows, at most `cap`, and whether more exist than the UI holds.
    fn ui_rows(&mut self, cap: usize) -> (Vec<Vec<Value>>, bool) {
        let more = self.truncated || self.n > cap;
        self.rows.truncate(cap);
        (std::mem::take(&mut self.rows), more)
    }
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
                "Only the first {} documents fit in the {MAX_CONTENT_KIB} KiB tool output limit.",
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
            "Only the first {} rows fit in the {MAX_CONTENT_KIB} KiB tool output limit.",
            rows.len()
        ));
    }
    out["rows"] = Value::from(rows);
    out
}

async fn read_tool(
    t: &'static Tool,
    ctx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let (src, text) = statement(ctx, t, args)?;
    let mut r = ai_read(ctx, src, text).await?;
    let summary = r.summary("");
    let data = r.data();
    let columns = std::mem::take(&mut r.columns);
    let (rows, truncated) = r.ui_rows(UI_ROWS);
    let mut done = Done::plain(summary, r.content);
    done.result = Some(data);
    done.columns = Some(columns);
    done.rows = Some(rows);
    done.truncated = Some(truncated);
    Ok(done)
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

async fn chart_tool(
    t: &'static Tool,
    ctx: &ToolContext<'_>,
    id: &str,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let (src, text) = statement(ctx, t, args)?;
    let spec = chart_spec(args)?;
    let mut r = ai_read(ctx, src, text).await?;
    check_columns(&spec, &r.columns)?;
    let mut kind = spec.kind.clone();
    kind[..1].make_ascii_uppercase();
    let summary = r.summary(&format!("{kind} chart · "));
    let data = r.data();
    let mut content = std::mem::take(&mut r.content);
    content["chart"] = json!(if r.n > CHART_ROWS {
        format!("Shown to the user. It draws the first {CHART_ROWS} rows.")
    } else {
        "Shown to the user.".to_string()
    });
    let columns = std::mem::take(&mut r.columns);
    let (mut rows, truncated) = r.ui_rows(CHART_ROWS);
    if src.conn.engine.numbers_as_text() {
        // This engine's rows hold big integers and decimals as text; the chart's y values are
        // numbers when they are finite.
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
    done.result = Some(data);
    done.preview = Some(ChatEvent::Chart {
        id: id.to_string(),
        spec,
        columns,
        rows,
        truncated,
    });
    Ok(done)
}

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

async fn write_tool(
    t: &'static Tool,
    ctx: &ToolContext<'_>,
    id: &str,
    args: &Map<String, Value>,
) -> Result<Done, AppError> {
    let (src, text) = statement(ctx, t, args)?;
    writes_allowed(src)?;
    // Parsed here so the preview never shows text that does not parse; the write parses again
    // when it runs.
    let info = ops::write_preview(text, src.conn.engine, &src.database)?;
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
        engine: src.conn.engine,
    });
    Ok(done)
}

/// The assistant may propose writes on `src`: the connection allows AI writes, is not read-only,
/// and the chat's write mode is on for it. The client sends the chat's mode; the stored flag is
/// checked too, so a client cannot enable writes the connection never allowed.
fn writes_allowed(src: &Source) -> Result<(), AppError> {
    match src.write_mode() {
        Some(off) => Err(off.blocked(&src.conn.name)),
        None => Ok(()),
    }
}

/// The marker `cap` appends instead of the cut text.
fn cut_marker() -> String {
    format!("\n[cut: the tool output was longer than {MAX_CONTENT_KIB} KiB]")
}

/// Cuts `s` to `MAX_CONTENT_BYTES` on a char boundary and adds the marker.
fn cap(mut s: String) -> String {
    if s.len() <= MAX_CONTENT_BYTES {
        return s;
    }
    let marker = cut_marker();
    let mut end = MAX_CONTENT_BYTES - marker.len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str(&marker);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::Connection;
    use crate::core::error::ServerCode;
    use crate::core::result::ColumnMeta;

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
        assert!(c.ends_with(&cut_marker()));
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

    /// The table is the one list: the definitions, the catalog and the dispatch agree, and each
    /// kind has one tool per engine it serves.
    #[test]
    fn the_table_is_the_one_list() {
        let sources = [source("db1", "a"), mongo_source("db2", "m")];
        let defs = definitions(&sources, Engines::of(&sources));
        let defined = names(&defs);
        let catalogued: Vec<&str> = catalog().map(|(name, _)| name).collect();
        assert_eq!(defined, catalogued);
        for (name, k) in catalog() {
            assert_eq!(kind(name), k, "{name}");
            assert!(tool_of(k, Engine::Pg).serves != Serves::Mongo, "{name}");
        }
        for engine in [Engine::Pg, Engine::Mysql, Engine::Sqlite, Engine::Mongo] {
            let write = tool_of(ToolKind::Write, engine);
            assert!(write.serves.matches(engine), "{engine:?}");
            assert_eq!(write.kind, ToolKind::Write);
        }
        assert_eq!(kind("nope"), ToolKind::Other);
        // Every engine-specific tool names its other engine's twin.
        for t in &TOOLS {
            if t.serves == Serves::All {
                assert!(t.other.is_none() && t.text.is_none(), "{}", t.name);
            } else {
                assert!(t.other.is_some() && t.text.is_some(), "{}", t.name);
            }
        }
        assert_eq!(defined.len(), TOOLS.len());
    }

    #[test]
    fn sql_only_chats_get_the_sql_tools_unchanged() {
        let sources = [source("db1", "a")];
        let defs = definitions(&sources, Engines::of(&sources));
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
        let sources = [mongo_source("db1", "m")];
        let defs = definitions(&sources, Engines::of(&sources));
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
        let sources = [source("db1", "pg"), mongo_source("db2", "m")];
        let defs = definitions(&sources, Engines::of(&sources));
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
        let one = [source("db1", "a")];
        for d in definitions(&one, Engines::of(&one)).as_array().unwrap() {
            let p = &d["function"]["parameters"];
            assert_eq!(p["properties"]["source"]["enum"], json!(["db1"]));
            let required = p["required"].as_array().cloned().unwrap_or_default();
            assert!(!required.contains(&json!("source")), "{d}");
        }
        let two = [source("db1", "a"), source("db2", "b")];
        let defs = definitions(&two, Engines::of(&two));
        for d in defs.as_array().unwrap() {
            let p = &d["function"]["parameters"];
            assert_eq!(p["properties"]["source"]["enum"], json!(["db1", "db2"]));
            assert!(
                p["required"].as_array().unwrap().contains(&json!("source")),
                "{d}"
            );
        }
        let describe = defs
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["function"]["name"] == "describe_table")
            .expect("describe_table");
        assert_eq!(
            &describe["function"]["parameters"]["required"],
            &json!(["table", "source"])
        );
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
                ai_write: true,
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

    /// A tool of the other engine is refused before anything runs, and the refusal names the
    /// tool that does work on that source.
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
        for (tool, args, source, other_engine) in [
            (
                "run_read_query",
                r#"{"source": "db2", "sql": "SELECT 1"}"#.to_string(),
                "db2",
                Engine::Mongo,
            ),
            (
                "render_chart",
                format!(r#"{{"source": "db2", "sql": "SELECT 1", {chart}}}"#),
                "db2",
                Engine::Mongo,
            ),
            (
                "propose_write",
                r#"{"source": "db2", "sql": "DELETE FROM t"}"#.to_string(),
                "db2",
                Engine::Mongo,
            ),
            (
                "run_mongo_read",
                r#"{"source": "db1", "query": "db.a.find()"}"#.to_string(),
                "db1",
                Engine::Pg,
            ),
            (
                "render_mongo_chart",
                format!(r#"{{"source": "db1", "query": "db.a.find()", {chart}}}"#),
                "db1",
                Engine::Pg,
            ),
            (
                "propose_mongo_write",
                r#"{"source": "db1", "query": "db.a.deleteMany({})"}"#.to_string(),
                "db1",
                Engine::Pg,
            ),
        ] {
            let out = execute(&ctx, "t", tool, &args).await;
            let e = error_of(&out);
            assert_eq!(e.code, ErrorCode::InvalidInput, "{tool}");
            // The refusal names the tool that serves that engine, from the one tool table.
            let want = tool_of(kind(tool), other_engine).name;
            assert!(e.message.contains(want), "{tool}: {}", e.message);
            assert!(e.message.contains(source), "{tool}: {}", e.message);
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
                assert_eq!(
                    info,
                    &crate::core::mongo::guard::classify_write(query, Some("shop"))
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            out.write,
            Some(ProposedWrite {
                conn_id: "c-db2".into(),
                database: "shop".into(),
                sql: query.into(),
                engine: Engine::Mongo,
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

    /// A Mongo server error keeps only its code in `schema` privacy, and the label comes from
    /// the typed code the Mongo error path builds.
    #[test]
    fn schema_privacy_keeps_the_mongo_error_code() {
        let e = crate::core::mongo::conn::server_error(
            11000,
            "E11000 duplicate key error dup key: { email: \"alice@example.com\" }",
        );
        assert_eq!(e.server_code, Some(ServerCode::new(Engine::Mongo, "11000")));
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
        assert!(
            e.message.contains("db1") && e.message.contains("db2"),
            "{}",
            e.message
        );
        let e = ctx.source(&args(json!({"source": "db3"}))).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        for want in ["db1", "db2", "db3"] {
            assert!(e.message.contains(want), "{want}: {}", e.message);
        }
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
                engine: Engine::Pg,
            })
        );
    }

    /// A write nobody decides within `APPROVAL_TIMEOUT` counts as denied and never runs.
    #[tokio::test]
    async fn a_write_nobody_answers_times_out_as_denied() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let sources = [source("db1", "a")];
        let ctx = ToolContext {
            state: &state,
            sources: &sources,
            settings: &settings,
        };
        let write = ProposedWrite {
            conn_id: "c-db1".into(),
            database: "shop".into(),
            sql: "DELETE FROM t".into(),
            engine: Engine::Pg,
        };
        let out = finish_write(
            &ctx,
            "w",
            &write,
            Decision::TimedOut,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(error_of(&out).code, ErrorCode::WriteDenied);
        assert!(out.write.is_none());
    }

    /// The stored connection setting must allow AI writes even when the client sends its own
    /// per-chat flag.
    #[tokio::test]
    async fn the_stored_ai_write_flag_is_required_too() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(
            dir.path(),
            Box::new(crate::core::secrets::MemoryStore::new()),
        )
        .unwrap();
        let settings = AiSettings::default();
        let mut chat_allows = source("db1", "prod");
        chat_allows.allow_writes = true;
        chat_allows.conn.ai_write = false;
        let sources = [chat_allows];
        let ctx = ToolContext {
            state: &state,
            sources: &sources,
            settings: &settings,
        };
        let out = execute(&ctx, "w", "propose_write", r#"{"sql": "DELETE FROM t"}"#).await;
        let e = error_of(&out);
        assert_eq!(e.code, ErrorCode::WriteBlocked);
        assert!(e.message.contains("\"prod\""), "{}", e.message);
        assert!(out.write.is_none());
    }

    #[test]
    fn schema_privacy_hides_server_error_text() {
        let mut server = AppError::new(
            ErrorCode::Database,
            "invalid input syntax for type integer: \"alice@example.com\"",
        )
        .with_detail("error returned from database: alice@example.com");
        server.server_code = Some(ServerCode::new(Engine::Pg, "22P02"));
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
        // Own message, so it reaches the model under `schema` privacy and the model can fix it:
        // it names the unknown column and the columns the result has.
        assert!(OWN_MESSAGE_CODES.contains(&e.code));
        for want in ["\"revenue\"", "\"month\"", "\"total\"", "\"region\""] {
            assert!(e.message.contains(want), "{want}: {}", e.message);
        }
        let cols = [
            "month".to_string(),
            "revenue".to_string(),
            "region".to_string(),
        ];
        assert!(check_columns(&spec, &cols).is_ok());
    }
}
