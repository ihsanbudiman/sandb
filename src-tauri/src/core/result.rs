//! The wire types that both engines return, and the few rules they share. Nothing here imports
//! `db` or `mongo`: each engine builds these values. The JSON shape is the frontend contract.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::connections::Connection;
use crate::core::error::{AppError, ErrorCode};

/// Rows per result set unless the caller asks for more.
pub const DEFAULT_LIMIT: usize = 200;
/// Upper bound for "Load more", so one result set cannot exhaust memory.
pub const MAX_LIMIT: usize = 100_000;

/// Rows per result set. Without a caller limit, a text whose own row limit is above the default
/// (`LIMIT 1000`, `.limit(1000)`) gets those rows on the first page.
pub fn limit(requested: Option<usize>, own: impl FnOnce() -> Option<u64>) -> usize {
    requested
        .unwrap_or_else(|| {
            own().map_or(DEFAULT_LIMIT, |n| {
                usize::try_from(n).unwrap_or(MAX_LIMIT).max(DEFAULT_LIMIT)
            })
        })
        .clamp(1, MAX_LIMIT)
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSort {
    pub column: String,
    pub column_index: usize,
    pub ascending: bool,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub conn_id: String,
    /// The connection's own database when absent.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub database: Option<String>,
    pub sql: String,
    /// Chosen by the caller; `cancel_query` uses it.
    pub query_id: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub limit: Option<usize>,
    /// The editor tab the run belongs to. On a production connection the run then executes
    /// inside that tab's own session and open transaction (manual commit).
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub tab_id: Option<String>,
    /// Server-side ordering for one read result. The original SQL remains the result's source.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub sort: Option<RunSort>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub data_type: String,
    /// MongoDB only: the column's cells hold several BSON types.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mixed: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementResult {
    pub sql: String,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    /// More rows exist than `limit`. For a read they were not fetched; for a write that returns
    /// rows (RETURNING) the statement ran to the end and only the first `limit` rows are kept.
    pub truncated: bool,
    /// The row limit the statement ran with; "Load more" runs it again with a larger one.
    pub limit: usize,
    /// Rows changed, for statements that return no columns.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub affected: Option<u64>,
    /// A plain read (`facts::is_plain_read_sql`): "Load more" and export may run it again.
    pub is_read: bool,
    /// The lone plain read of the run is a query (`SELECT`, `WITH`, `VALUES`), so a header click
    /// may add ORDER BY to it. EXPLAIN and SHOW are reads that cannot be sorted.
    pub sortable: bool,
    pub elapsed_ms: u64,
    /// MongoDB only: each document of the page as relaxed Extended JSON, for the JSON view and the
    /// full value of a cell that the grid shows as a summary (`[3 items]`) or cut text.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub documents: Option<Vec<Value>>,
    /// MongoDB only: the BSON type name of each cell (`rows` shape), `"missing"` when the document
    /// has no such field.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub cell_types: Option<Vec<Vec<&'static str>>>,
    /// MongoDB only: the database the statement ran in. A `use` earlier in the same run makes it
    /// differ from the tab's database, and reruns and grid edits must go to this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub database: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRun {
    pub statements: Vec<StatementResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(test, ts(optional = nullable))]
    pub notices: Vec<String>,
    pub elapsed_ms: u64,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryProgress {
    pub done: usize,
    pub total: usize,
}

pub const DEFAULT_MAX_ROWS: usize = 500;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// The outer safety net fires this long after the database's own limit should have.
pub const GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    pub max_rows: usize,
    pub timeout: Duration,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl ReadLimits {
    /// The limits to send: PostgreSQL and MySQL read 0 as "no limit", and so does MongoDB's
    /// `maxTimeMS`, so a zero becomes the smallest real limit instead of turning the cap off.
    /// `db::readonly` and `mongo::ai` call this once, so no AI read hands the server an unset limit.
    pub fn sanitized(self) -> Self {
        Self {
            max_rows: self.max_rows.max(1),
            timeout: self.timeout.max(Duration::from_millis(1)),
        }
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiReadResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    /// More rows exist than `max_rows`; they were not fetched.
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// MongoDB only: each document of the page as relaxed Extended JSON, one per row.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub documents: Option<Vec<Value>>,
}

pub fn timeout_error(limit: Duration) -> AppError {
    let shown = if limit < Duration::from_secs(1) {
        format!("{} ms", limit.as_millis())
    } else {
        format!("{} s", limit.as_secs_f64())
    };
    AppError::new(
        ErrorCode::Timeout,
        format!("The AI query did not finish within {shown} and was stopped"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteResult {
    /// Rows that all statements changed together.
    pub affected: u64,
    pub elapsed_ms: u64,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub conn_id: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub database: Option<String>,
    pub sql: String,
    /// Production editor tab: the export reads inside the tab's open transaction.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub tab_id: Option<String>,
}

/// A result as INSERT statements into `table` (in `schema`, the PG schema or MySQL database,
/// when set), for loading the rows into the same table in another database.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlExportRequest {
    #[serde(flatten)]
    pub query: ExportRequest,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub schema: Option<String>,
    pub table: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub options: SqlOptions,
}

/// A MongoDB read as Extended JSON, one document after the other.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentExportRequest {
    #[serde(flatten)]
    pub query: ExportRequest,
    pub format: DocumentFormat,
    pub ejson: EjsonFlavor,
}

/// How the documents are separated.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFormat {
    /// One JSON array of documents.
    Json,
    /// One document per line (newline-delimited JSON).
    Ndjson,
}

/// How BSON values appear in JSON.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EjsonFlavor {
    /// Numbers as JSON numbers where the type survives, everything else as `{"$type": ...}`.
    Relaxed,
    /// Every value keeps its type.
    Canonical,
}

/// What a row already in the target does to its INSERT.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InsertMode {
    /// Plain INSERT: the statement fails.
    #[default]
    Insert,
    /// The row is skipped: PG `ON CONFLICT DO NOTHING`, MySQL `INSERT IGNORE`, SQLite
    /// `INSERT OR IGNORE`.
    Skip,
    /// The row is updated: `ON CONFLICT (key) DO UPDATE`, MySQL `ON DUPLICATE KEY UPDATE`.
    Upsert,
}

/// Column indexes are positions in the result, so repeated names stay apart.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SqlOptions {
    /// Result columns to write, in this order; `None` writes every column.
    #[cfg_attr(test, ts(optional = nullable))]
    pub columns: Option<Vec<usize>>,
    #[cfg_attr(test, ts(optional = nullable))]
    pub mode: InsertMode,
    /// Conflict key of an upsert (result columns). MySQL finds the key itself and only
    /// leaves these columns out of the update.
    #[cfg_attr(test, ts(optional = nullable))]
    pub key: Vec<usize>,
    #[cfg_attr(test, ts(optional = nullable))]
    pub rows_per_statement: usize,
    /// `BEGIN;` … `COMMIT;` around the statements, so a failed row loads nothing.
    #[cfg_attr(test, ts(optional = nullable))]
    pub transaction: bool,
    /// The `-- sandb SQL export` comment block at the top.
    #[cfg_attr(test, ts(optional = nullable))]
    pub header: bool,
}

impl Default for SqlOptions {
    fn default() -> Self {
        Self {
            columns: None,
            mode: InsertMode::Insert,
            key: Vec::new(),
            rows_per_statement: 100,
            transaction: false,
            header: true,
        }
    }
}

pub const MAX_ROWS_PER_STATEMENT: usize = 10_000;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// The user closed the save dialog; nothing was written.
    pub cancelled: bool,
    pub rows: u64,
    pub path: String,
}

/// One table as a `.sql` file (or a MongoDB collection as a mongosh script).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableExportRequest {
    pub conn_id: String,
    pub database: String,
    /// PG schema (default `public`); ignored elsewhere.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub schema: Option<String>,
    pub table: String,
    /// Also write INSERT statements for every row (tables only; a view exports its DDL alone).
    pub data: bool,
}

/// Columns in one autocomplete snapshot; larger schemas are cut and flagged `truncated`.
pub const SNAPSHOT_MAX_COLUMNS: usize = 2000;
/// Tables in one snapshot. Separate from the column cap so that wide tables early in the
/// order cannot hide later table names from autocomplete.
pub const SNAPSHOT_MAX_TABLES: usize = 10_000;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TableKind {
    Table,
    View,
    /// PG materialized view: it stores rows, but is not a table.
    Matview,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
    /// Server statistics, not an exact count; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub row_estimate: Option<i64>,
    /// SQLite only: the table has an implicit `rowid` (not `WITHOUT ROWID`, not a view).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub has_rowid: Option<bool>,
    /// PG only: the parent table when this table is a partition.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub partition_of: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub default: Option<String>,
    pub is_pk: bool,
    /// 1-based position in the primary key.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub pk_ordinal: Option<u32>,
    /// MongoDB only: the share of sampled documents that have this field, 0.0 to 1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub frequency: Option<f64>,
    /// MongoDB only: the number of documents in the sample the fields come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub sample_size: Option<u32>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotColumn {
    pub name: String,
    pub data_type: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTable {
    /// PG schema; absent on MySQL and SQLite (the database is the namespace).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub schema: Option<String>,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<SnapshotColumn>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSnapshot {
    pub database: String,
    pub tables: Vec<SnapshotTable>,
    pub truncated: bool,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    /// Column names in index order; expressions appear as their SQL text.
    pub columns: Vec<String>,
    pub unique: bool,
    /// The index backs the primary key.
    pub primary: bool,
    /// Access method (btree, hash, gin, … on PG; BTREE, FULLTEXT, … on MySQL); absent on SQLite.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub method: Option<String>,
    /// Full `CREATE INDEX` text when the engine gives it (PG, SQLite).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub definition: Option<String>,
    /// MongoDB only: the parts of the index that have no SQL counterpart.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mongo: Option<MongoIndexInfo>,
}

/// A MongoDB index as `listIndexes` describes it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoIndexInfo {
    /// The key document as compact relaxed Extended JSON, e.g. `{"status":1,"createdAt":-1}`.
    pub keys: String,
    pub sparse: bool,
    pub hidden: bool,
    /// `expireAfterSeconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ttl_seconds: Option<i64>,
    /// `partialFilterExpression`, as compact relaxed Extended JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub partial_filter: Option<String>,
    /// The remaining options (collation, weights, default_language, ...), as compact relaxed
    /// Extended JSON; absent when the index has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub options: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConstraintKind {
    Primary,
    Unique,
    Check,
    Exclude,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintInfo {
    pub name: String,
    pub kind: ConstraintKind,
    pub columns: Vec<String>,
    /// Check expression or PG `pg_get_constraintdef` text; absent when not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub definition: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: String,
    /// Referencing side. `schema` is PG only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub schema: Option<String>,
    pub table: String,
    pub columns: Vec<String>,
    /// Referenced side. `ref_schema` is PG only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL` or `SET DEFAULT`.
    pub on_update: String,
    pub on_delete: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableProperties {
    pub indexes: Vec<IndexInfo>,
    pub constraints: Vec<ConstraintInfo>,
    /// Foreign keys this table declares.
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Foreign keys of other tables (or self-references) that point at this table.
    pub references: Vec<ForeignKeyInfo>,
    /// MongoDB only: everything the SQL fields have no room for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mongo: Option<MongoCollectionInfo>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MongoCollectionKind {
    #[default]
    Collection,
    View,
    Timeseries,
}

/// A MongoDB collection as `listCollections` and `$collStats` describe it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoCollectionInfo {
    pub kind: MongoCollectionKind,
    /// The whole validator document, as pretty (2-space) relaxed Extended JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub validator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub validation_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub validation_action: Option<String>,
    /// The other `listCollections` options (capped, size, max, timeseries, collation, viewOn,
    /// pipeline, ...) as pretty relaxed Extended JSON; `{}` when there are none.
    pub options: String,
    /// Absent for views, and when the user may not read the stats.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub stats: Option<MongoCollectionStats>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoCollectionStats {
    pub count: i64,
    pub size: i64,
    pub avg_obj_size: f64,
    pub storage_size: i64,
    pub total_index_size: i64,
    pub nindexes: i64,
}

/// One table or view, ordered as the engine lists them, at most `SNAPSHOT_MAX_TABLES + 1`.
pub struct SnapTable {
    pub schema: Option<String>,
    pub table: String,
    pub kind: TableKind,
}

/// One column, ordered by table, at most `SNAPSHOT_MAX_COLUMNS + 1`.
pub struct SnapRow {
    pub schema: Option<String>,
    pub table: String,
    pub column: String,
    pub data_type: String,
}

/// Every table (up to `SNAPSHOT_MAX_TABLES`), with columns filled in up to `SNAPSHOT_MAX_COLUMNS`.
pub fn group_snapshot(
    database: String,
    tables: Vec<SnapTable>,
    rows: Vec<SnapRow>,
) -> SchemaSnapshot {
    let truncated = tables.len() > SNAPSHOT_MAX_TABLES || rows.len() > SNAPSHOT_MAX_COLUMNS;
    let mut tables: Vec<SnapshotTable> = tables
        .into_iter()
        .take(SNAPSHOT_MAX_TABLES)
        .map(|t| SnapshotTable {
            schema: t.schema,
            name: t.table,
            kind: t.kind,
            columns: vec![],
        })
        .collect();
    let at: HashMap<(Option<String>, String), usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| ((t.schema.clone(), t.name.clone()), i))
        .collect();
    for r in rows.into_iter().take(SNAPSHOT_MAX_COLUMNS) {
        if let Some(&i) = at.get(&(r.schema, r.table)) {
            tables[i].columns.push(SnapshotColumn {
                name: r.column,
                data_type: r.data_type,
            });
        }
    }
    SchemaSnapshot {
        database,
        tables,
        truncated,
    }
}

/// Folds rows that arrive sorted by key: a row whose key equals the previous row's extends that
/// item with `push`; any other row starts a new item with `start`.
pub fn fold_runs<R, K: PartialEq, T>(
    rows: impl IntoIterator<Item = R>,
    key: impl Fn(&R) -> K,
    start: impl Fn(R) -> T,
    push: impl Fn(&mut T, R),
) -> Vec<T> {
    let mut out: Vec<(K, T)> = Vec::new();
    for row in rows {
        let k = key(&row);
        match out.last_mut() {
            Some((last, item)) if *last == k => push(item, row),
            _ => out.push((k, start(row))),
        }
    }
    out.into_iter().map(|(_, item)| item).collect()
}

pub fn not_found_table(table: &str) -> AppError {
    AppError::not_found(format!("Table {table} not found"))
}

pub type CellValue = Value;

/// One `{type, value}` value of the wire: a BSON value as the grid sends it.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TypedValue {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional))]
    pub value: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditableRequest {
    pub conn_id: String,
    pub database: String,
    pub sql: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditableResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub schema: Option<String>,
    pub pk: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    /// MongoDB only: whether the server has transactions (replica set or sharded cluster).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub transactions: Option<bool>,
}

impl EditableResult {
    pub fn not_editable(reason: impl Into<String>) -> Self {
        Self {
            table: None,
            schema: None,
            pk: Vec::new(),
            reason: Some(reason.into()),
            transactions: None,
        }
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEditsRequest {
    pub conn_id: String,
    pub database: String,
    pub sql: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub tab_id: Option<String>,
    /// The id `cancel_query` uses to stop this save: the save runs in the editor's query
    /// registry, so the dialog's Cancel reaches it.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub query_id: Option<String>,
    pub operations: Vec<EditOperation>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOperation {
    pub key: String,
    pub kind: EditKind,
    pub pk: std::collections::BTreeMap<String, CellValue>,
    pub set: std::collections::BTreeMap<String, CellValue>,
    /// MongoDB update: path → typed value of the cell as it was loaded, for the conflict check.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub old: std::collections::BTreeMap<String, TypedValue>,
    /// MongoDB insert: the whole new document as mongosh text.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub document: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditKind {
    Update,
    Insert,
    Delete,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEditsResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub failed: Option<EditFailure>,
    /// MongoDB standalone only: the operations before `failed.index` stay saved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFailure {
    pub index: usize,
    pub message: String,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellValueRequest {
    pub conn_id: String,
    pub database: String,
    pub sql: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub tab_id: Option<String>,
    pub pk: std::collections::BTreeMap<String, CellValue>,
    pub column: String,
}

/// The AI read guard's verdict (`guard::check_ai_read`) as one value for the UI and the bridge.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardCheck {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub kind: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteStatement {
    /// `INSERT`, `UPDATE`, `DROP TABLE`, `SELECT`, …
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub table: Option<String>,
    /// MongoDB only: the database the statement runs on, from the tab's database and following
    /// `use`, so the confirmation names it even when `table` is absent (`db.dropDatabase()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub database: Option<String>,
    /// MongoDB only: the statement only reads. The editor asks for a confirmation before a run on
    /// a Production connection when any statement has `read: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub read: Option<bool>,
    /// Whether the statement changes what the catalog lists (schema, collections, indexes), so a
    /// run has to drop the cached schema. Per statement, so the editor can ask before a run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub structure: Option<bool>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    UpdateWithoutWhere,
    DeleteWithoutWhere,
    Drop,
    Truncate,
    AlterDrop,
    MultipleStatements,
    Unparsable,
    ExecutableComment,
    /// MongoDB: `renameCollection` cannot be undone.
    Rename,
    /// MongoDB: the guard cannot say what the statement does.
    UnknownCommand,
    /// MySQL: a statement that commits implicitly (DDL, LOCK TABLES, SET autocommit, CALL, …),
    /// so the approved write does not run as one transaction.
    ImplicitCommit,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteWarning {
    pub code: WarningCode,
    pub message: String,
    /// 0-based statement the warning is about; absent for the whole text.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub statement: Option<usize>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteInfo {
    pub statements: Vec<WriteStatement>,
    pub warnings: Vec<WriteWarning>,
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
    /// The chat's write mode for this source is on. The connection's saved `ai_write` and
    /// `may_write()` are required too; [`Source::write_mode`] checks all three.
    pub allow_writes: bool,
}

impl Source {
    /// `None` when the assistant may propose writes on this source; otherwise the first rule
    /// that forbids it. The prompt and the write tool both read this one gate.
    pub fn write_mode(&self) -> Option<WriteOff> {
        if !self.conn.may_write() {
            Some(WriteOff::ReadOnly)
        } else if !self.conn.ai_write {
            Some(WriteOff::AiWritesOff)
        } else if !self.allow_writes {
            Some(WriteOff::ModeOff)
        } else {
            None
        }
    }
}

/// Why the assistant may not propose writes on a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOff {
    /// The connection allows no writes at all.
    ReadOnly,
    /// The connection's saved setting forbids the assistant's writes.
    AiWritesOff,
    /// The chat's write mode is off for this source.
    ModeOff,
}

impl WriteOff {
    /// The reason as the system prompt states it for a chat with one source.
    pub fn reason(self) -> &'static str {
        match self {
            Self::ReadOnly => "This connection is read-only",
            Self::AiWritesOff => "AI writes are off on this connection",
            Self::ModeOff => "Write mode is off",
        }
    }

    /// The reason inside the prompt's per-source `Writes: not allowed, …` line.
    pub fn clause(self) -> &'static str {
        match self {
            Self::ReadOnly => "the connection is read-only",
            Self::AiWritesOff => "AI writes are off on this connection",
            Self::ModeOff => "write mode is off",
        }
    }

    /// The refusal of a proposed write, naming the connection.
    pub fn blocked(self, conn: &str) -> AppError {
        let message = match self {
            Self::ReadOnly => {
                format!("Connection \"{conn}\" is read-only, so the assistant cannot propose writes")
            }
            Self::AiWritesOff => format!(
                "AI writes are off on \"{conn}\", so the assistant cannot propose writes"
            ),
            Self::ModeOff => format!(
                "Write mode is off on \"{conn}\", so the assistant cannot propose writes"
            ),
        };
        AppError::new(ErrorCode::WriteBlocked, message)
    }
}

/// The engines of one chat's sources, worked out once: the prompt and the tool definitions both
/// read it, and neither counts engines of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engines {
    pub sql: bool,
    pub mongo: bool,
}

impl Engines {
    pub fn of(sources: &[Source]) -> Self {
        Self {
            sql: sources.iter().any(|s| s.conn.engine.sql().is_some()),
            mongo: sources.iter().any(|s| s.conn.engine.sql().is_none()),
        }
    }

    /// A chat with both kinds of source: each tool names the engine it serves.
    pub fn is_mixed(self) -> bool {
        self.sql && self.mongo
    }
}

/// What the UI draws. Every column name is a column of the result.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    pub x: String,
    pub y: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub series: Option<String>,
    pub stacked: bool,
}
