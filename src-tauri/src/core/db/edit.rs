//! DBeaver-style grid editing for one simple table SELECT.
//!
//! Editability is derived again when saving: the original SQL, current primary-key metadata and
//! connection write permission are all checked before any statement runs. Every value is bound;
//! identifiers from parsed SQL and schema metadata are quoted, never interpolated as values.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sqlparser::ast::{
    Expr, GroupByExpr, Ident, ObjectName, ObjectNamePart, Query, SelectFlavor, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, Statement, TableFactor, WildcardAdditionalOptions,
};
use sqlparser::parser::Parser;
use sqlx::mysql::MySqlArguments;
use sqlx::postgres::PgArguments;
use sqlx::sqlite::SqliteArguments;
use sqlx::{MySql, Postgres, Sqlite};
use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, Env, target_database};
use crate::core::db::conn::Conn;
use crate::core::db::ctx::Ctx;
use crate::core::db::introspect;
use crate::core::db::session::{self, Settle};
use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{
    ApplyEditsRequest, ApplyEditsResult, CellValue, CellValueRequest, ColumnInfo, EditFailure,
    EditKind, EditOperation, EditableRequest, EditableResult,
};
use crate::core::sql::split;

/// The names SQLite gives its implicit rowid; any of them addresses the row.
const ROWID_ALIASES: [&str; 3] = ["rowid", "_rowid_", "oid"];

#[derive(Debug)]
enum ParsedQuery {
    NotEditable(String),
    Simple(SimpleQuery),
}

#[derive(Debug)]
struct SimpleQuery {
    engine: SqlEngine,
    qualifier: Option<Ident>,
    table: Ident,
    projection: Projection,
}

#[derive(Debug, Default)]
struct Projection {
    all: bool,
    columns: Vec<ProjectedColumn>,
}

#[derive(Debug)]
struct ProjectedColumn {
    output_name: String,
}

#[derive(Debug)]
struct Target {
    result: EditableResult,
    /// Database used for MySQL's qualified table and metadata lookup.
    database: String,
    /// Qualified, quoted table name. All parts came from parsed identifiers or known defaults.
    relation: String,
    columns: BTreeMap<String, String>,
}

enum Resolution {
    NotEditable(String),
    Editable(Target),
}

fn plain_wildcard(o: &WildcardAdditionalOptions) -> bool {
    o.opt_ilike.is_none()
        && o.opt_exclude.is_none()
        && o.opt_except.is_none()
        && o.opt_replace.is_none()
        && o.opt_rename.is_none()
        && o.opt_alias.is_none()
}

fn ident_name(engine: SqlEngine, ident: &Ident) -> String {
    // PostgreSQL keeps a quoted name exactly; every other case folds (MySQL and SQLite match
    // names case-insensitively here).
    if engine == SqlEngine::Pg && ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_lowercase()
    }
}

fn output_name(engine: SqlEngine, ident: &Ident) -> String {
    match engine {
        SqlEngine::Pg => ident_name(engine, ident),
        // These engines report the projection's original spelling, which must match metadata
        // exactly before it can be used as an edit key.
        SqlEngine::Mysql | SqlEngine::Sqlite => ident.value.clone(),
    }
}

fn object_idents(name: &ObjectName) -> Option<Vec<Ident>> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(i) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

fn projected_column(
    engine: SqlEngine,
    expr: &Expr,
    alias: Option<&Ident>,
    table: &Ident,
    table_alias: Option<&Ident>,
) -> Option<ProjectedColumn> {
    let (name, qualifier) = match expr {
        Expr::Identifier(i) => (i, None),
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => (&parts[1], Some(&parts[0])),
        _ => return None,
    };
    if let Some(q) = qualifier {
        let q = ident_name(engine, q);
        if q != ident_name(engine, table) && table_alias.is_none_or(|a| q != ident_name(engine, a))
        {
            return None;
        }
    }
    if alias.is_some_and(|alias| alias.value != name.value) {
        return None;
    }
    Some(ProjectedColumn {
        output_name: output_name(engine, name),
    })
}

/// Parses exactly the SELECT shape whose rows can be traced back to one base-table key.
fn parse_simple(engine: SqlEngine, sql: &str) -> Result<ParsedQuery, AppError> {
    let dialect = split::dialect(engine);
    let parsed = Parser::parse_sql(&*dialect, sql)
        .map_err(|e| AppError::invalid("Invalid SQL").with_detail(e.to_string()))?;
    let [Statement::Query(query)] = parsed.as_slice() else {
        return Ok(ParsedQuery::NotEditable(
            "Grid editing requires one SELECT query".into(),
        ));
    };
    match simple_query(engine, query) {
        Some(q) => Ok(ParsedQuery::Simple(q)),
        None => Ok(ParsedQuery::NotEditable(
            "Only a simple single-table SELECT with plain columns or * is editable".into(),
        )),
    }
}

fn simple_query(engine: SqlEngine, query: &Query) -> Option<SimpleQuery> {
    if query.with.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.format_clause.is_some()
        || query.settings.is_some()
        || !query.pipe_operators.is_empty()
    {
        return None;
    }
    let SetExpr::Select(select) = &*query.body else {
        return None;
    };
    if select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.connect_by.is_empty()
        || !matches!(&select.group_by, GroupByExpr::Expressions(v, _) if v.is_empty())
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || select.flavor != SelectFlavor::Standard
        || select.from.len() != 1
        || !select.from[0].joins.is_empty()
    {
        return None;
    }
    let TableFactor::Table {
        name,
        alias,
        args,
        with_hints,
        version,
        with_ordinality,
        partitions,
        json_path,
        sample,
        index_hints,
    } = &select.from[0].relation
    else {
        return None;
    };
    if args.is_some()
        || !with_hints.is_empty()
        || version.is_some()
        || *with_ordinality
        || !partitions.is_empty()
        || json_path.is_some()
        || sample.is_some()
        || !index_hints.is_empty()
        || alias
            .as_ref()
            .is_some_and(|a| !a.columns.is_empty() || a.at.is_some())
    {
        return None;
    }
    let mut parts = object_idents(name)?;
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let table = parts.pop()?;
    let qualifier = parts.pop();
    let table_alias = alias.as_ref().map(|a| &a.name);
    let mut projection = Projection::default();
    let mut output_names = BTreeSet::new();
    for item in &select.projection {
        match item {
            SelectItem::Wildcard(o) if plain_wildcard(o) => {
                if projection.all {
                    return None;
                }
                projection.all = true;
            }
            SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), o)
                if plain_wildcard(o) =>
            {
                let ids = object_idents(n)?;
                let q = ids.last()?;
                let q = ident_name(engine, q);
                if q != ident_name(engine, &table)
                    && table_alias.is_none_or(|a| q != ident_name(engine, a))
                {
                    return None;
                }
                if projection.all {
                    return None;
                }
                projection.all = true;
            }
            SelectItem::UnnamedExpr(expr) => {
                let col = projected_column(engine, expr, None, &table, table_alias)?;
                if !output_names.insert(col.output_name.clone()) {
                    return None;
                }
                projection.columns.push(col);
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                let col = projected_column(engine, expr, Some(alias), &table, table_alias)?;
                if !output_names.insert(col.output_name.clone()) {
                    return None;
                }
                projection.columns.push(col);
            }
            _ => return None,
        }
    }
    if !projection.all && projection.columns.is_empty() {
        return None;
    }
    Some(SimpleQuery {
        engine,
        qualifier,
        table,
        projection,
    })
}

fn table_names(q: &SimpleQuery, requested_database: &str) -> (Option<String>, String, String) {
    match q.engine {
        SqlEngine::Pg => {
            let table = ident_name(q.engine, &q.table);
            let schema = q
                .qualifier
                .as_ref()
                .map(|s| ident_name(q.engine, s))
                .unwrap_or_else(|| "public".into());
            let relation = format!("{}.{}", q.engine.quote(&schema), q.engine.quote(&table));
            (Some(schema), table, relation)
        }
        SqlEngine::Mysql => {
            let table = q.table.value.clone();
            let database = q
                .qualifier
                .as_ref()
                .map(|s| s.value.clone())
                .unwrap_or_else(|| requested_database.to_string());
            let relation = if q.qualifier.is_some() {
                format!("{}.{}", q.engine.quote(&database), q.engine.quote(&table))
            } else {
                q.engine.quote(&table)
            };
            (None, table, relation)
        }
        SqlEngine::Sqlite => {
            let table = q.table.value.clone();
            let schema = q
                .qualifier
                .as_ref()
                .map(|s| s.value.clone())
                .unwrap_or_else(|| "main".into());
            let relation = format!("{}.{}", q.engine.quote(&schema), q.engine.quote(&table));
            (None, table, relation)
        }
    }
}

async fn resolve(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
    sql: &str,
) -> Result<Resolution, AppError> {
    let simple = match parse_simple(engine, sql)? {
        ParsedQuery::NotEditable(reason) => return Ok(Resolution::NotEditable(reason)),
        ParsedQuery::Simple(q) => q,
    };
    let (schema, table, relation) = table_names(&simple, database);
    if engine == SqlEngine::Sqlite && schema_is_not_main(&simple) {
        return Ok(Resolution::NotEditable(
            "Only tables in SQLite's main database are editable".into(),
        ));
    }
    let metadata_database = if engine == SqlEngine::Mysql {
        simple
            .qualifier
            .as_ref()
            .map(|s| s.value.clone())
            .unwrap_or_else(|| database.to_string())
    } else {
        database.to_string()
    };
    let columns = introspect::list_columns(
        ctx,
        conn,
        &metadata_database,
        schema.as_deref(),
        &table,
    )
    .await?;
    let declared: BTreeSet<String> = columns.iter().map(|c| c.name.to_lowercase()).collect();
    for projected in &simple.projection.columns {
        let actual = columns.iter().find(|c| c.name == projected.output_name);
        if actual.is_some() && simple.projection.all {
            return Ok(Resolution::NotEditable(format!(
                "Output column {} is duplicated by the wildcard",
                projected.output_name
            )));
        }
        let implicit_rowid = engine == SqlEngine::Sqlite
            && ROWID_ALIASES.contains(&projected.output_name.as_str())
            && !declared.contains(&projected.output_name.to_lowercase());
        if actual.is_none() && !implicit_rowid {
            return Ok(Resolution::NotEditable(format!(
                "Output column {} does not exactly match a table column",
                projected.output_name
            )));
        }
    }
    let mut pk: Vec<&ColumnInfo> = columns.iter().filter(|c| c.is_pk).collect();
    pk.sort_by_key(|c| c.pk_ordinal);
    let mut pk: Vec<String> = pk.into_iter().map(|c| c.name.clone()).collect();
    if pk.is_empty() {
        if engine == SqlEngine::Sqlite
            && introspect::has_rowid(ctx, conn, database, &table).await?
            && let Some(rowid) = ROWID_ALIASES.into_iter().find(|alias| {
                !declared.contains(*alias)
                    && simple
                        .projection
                        .columns
                        .iter()
                        .any(|c| c.output_name == *alias)
            })
        {
            pk.push(rowid.into());
        }
        if pk.is_empty() {
            return Ok(Resolution::NotEditable(
                "The table has no primary key included in the result".into(),
            ));
        }
    }
    let missing: Vec<&str> = pk
        .iter()
        .map(String::as_str)
        .filter(|name| {
            !simple.projection.all
                && !simple
                    .projection
                    .columns
                    .iter()
                    .any(|c| c.output_name == *name)
        })
        .collect();
    if !missing.is_empty() {
        return Ok(Resolution::NotEditable(format!(
            "The result does not include primary-key column(s): {}",
            missing.join(", ")
        )));
    }
    let result = EditableResult {
        table: Some(table),
        schema,
        pk,
        reason: None,
        transactions: None,
    };
    let mut column_types: BTreeMap<String, String> =
        columns.into_iter().map(|c| (c.name, c.data_type)).collect();
    for name in &result.pk {
        if engine == SqlEngine::Sqlite && !column_types.contains_key(name) {
            column_types.insert(name.clone(), "INTEGER".into());
        }
    }
    Ok(Resolution::Editable(Target {
        result,
        database: metadata_database,
        relation,
        columns: column_types,
    }))
}

fn schema_is_not_main(q: &SimpleQuery) -> bool {
    q.qualifier
        .as_ref()
        .is_some_and(|s| !s.value.eq_ignore_ascii_case("main"))
}

/// Describes how the result can be edited. A valid but unsupported query is a normal result with
/// `reason`, not a command error.
pub async fn editable_result(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    request: EditableRequest,
) -> Result<EditableResult, AppError> {
    let database = target_database(conn, Some(&request.database));
    match resolve(ctx, conn, engine, &database, &request.sql).await? {
        Resolution::NotEditable(reason) => Ok(EditableResult::not_editable(reason)),
        Resolution::Editable(_) if !conn.may_write() => {
            Ok(EditableResult::not_editable("The connection is read-only"))
        }
        Resolution::Editable(target) => Ok(target.result),
    }
}

struct BoundValue<'a> {
    value: &'a CellValue,
    data_type: &'a str,
}

/// The grid shows geometry as plain WKT, which has SRID 0. A value without an SRID gets the
/// column's SRID: the typmod's (`geometry(Point,4326)`), else on update the row's old one.
/// A value with its own SRID (EWKT) is left as it is.
fn pg_keep_srid(p: &str, column: &str, data_type: &str, update: bool) -> String {
    // `geometry`, or `extensions.geometry` when PostGIS is not on the search path.
    let base = crate::core::engine::pg_base_type(data_type);
    let Some(schema) = base.strip_suffix("geometry") else {
        return p.into();
    };
    if !(schema.is_empty() || schema.ends_with('.')) {
        return p.into();
    }
    let typmod_srid = data_type
        .split_once('(')
        .and_then(|(_, args)| args.trim_end_matches(')').split(',').nth(1))
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|&s| s != 0);
    let srid = match typmod_srid {
        Some(s) => s.to_string(),
        None if update => {
            format!("COALESCE({schema}st_srid({}), 0)", SqlEngine::Pg.quote(column))
        }
        None => return p.into(),
    };
    format!("CASE WHEN {schema}st_srid({p}) = 0 THEN {schema}st_setsrid({p}, {srid}) ELSE {p} END")
}

/// The expression that binds the `index`-th value. PostgreSQL adds its type cast (and the
/// PostGIS SRID rule); the other engines have neither, so they get the bare placeholder.
fn value_sql(
    engine: SqlEngine,
    index: usize,
    column: &str,
    data_type: &str,
    update: bool,
) -> String {
    let p = engine.placeholder(index, data_type);
    if engine == SqlEngine::Pg {
        pg_keep_srid(&p, column, data_type, update)
    } else {
        p
    }
}

fn operation_sql<'a>(
    target: &'a Target,
    engine: SqlEngine,
    op: &'a EditOperation,
) -> Result<(String, Vec<BoundValue<'a>>), String> {
    if !matches!(op.kind, EditKind::Insert) {
        let expected: BTreeSet<&str> = target.result.pk.iter().map(String::as_str).collect();
        let actual: BTreeSet<&str> = op.pk.keys().map(String::as_str).collect();
        if actual != expected {
            return Err(
                "The operation must contain exactly the original primary-key values".into(),
            );
        }
    }
    for name in op.set.keys() {
        if !target.columns.contains_key(name) {
            return Err(format!("Unknown table column {name}"));
        }
    }
    let mut binds = Vec::new();
    let mut next = 1usize;
    let mut push = |name: &'a str, value: &'a CellValue| {
        let data_type = target.columns.get(name).expect("validated column");
        let index = next;
        next += 1;
        binds.push(BoundValue { value, data_type });
        index
    };
    let predicate = |name: &str, index: usize| {
        format!(
            "{} = {}",
            engine.quote(name),
            engine.placeholder(index, target.columns[name].as_str())
        )
    };
    let sql = match op.kind {
        EditKind::Update => {
            if op.set.is_empty() {
                return Err("An update needs at least one changed column".into());
            }
            let assignments = op
                .set
                .iter()
                .map(|(name, value)| {
                    let i = push(name, value);
                    let p = value_sql(
                        engine,
                        i,
                        name,
                        target.columns[name.as_str()].as_str(),
                        true,
                    );
                    format!("{} = {p}", engine.quote(name))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let predicates = op
                .pk
                .iter()
                .map(|(name, value)| {
                    let i = push(name, value);
                    predicate(name, i)
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            format!(
                "UPDATE {} SET {assignments} WHERE {predicates}",
                target.relation
            )
        }
        EditKind::Delete => {
            let predicates = op
                .pk
                .iter()
                .map(|(name, value)| {
                    let i = push(name, value);
                    predicate(name, i)
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("DELETE FROM {} WHERE {predicates}", target.relation)
        }
        EditKind::Insert if op.set.is_empty() => engine.empty_insert(&target.relation),
        EditKind::Insert => {
            let names = op
                .set
                .keys()
                .map(|name| engine.quote(name))
                .collect::<Vec<_>>()
                .join(", ");
            let values = op
                .set
                .iter()
                .map(|(name, value)| {
                    let i = push(name, value);
                    value_sql(
                        engine,
                        i,
                        name,
                        target.columns[name.as_str()].as_str(),
                        false,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {} ({names}) VALUES ({values})",
                target.relation
            )
        }
    };
    Ok((sql, binds))
}

fn binary(value: &Value) -> Result<Option<Vec<u8>>, String> {
    let Value::Object(o) = value else {
        return Ok(None);
    };
    if !o.contains_key("$bytes") {
        return Ok(None);
    }
    let Some(encoded) = o.get("$bytes").and_then(Value::as_str) else {
        return Err("Invalid binary cell value".into());
    };
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid binary cell value".to_string())?;
    if o.get("len").and_then(Value::as_u64) != Some(bytes.len() as u64) {
        return Err("A truncated binary preview cannot be saved".into());
    }
    Ok(Some(bytes))
}

fn first_text<R: sqlx::Row>(row: Option<R>) -> Result<Option<Option<String>>, AppError>
where
    for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    usize: sqlx::ColumnIndex<R>,
{
    row.map(|r| r.try_get::<Option<String>, _>(0))
        .transpose()
        .map_err(|e| AppError::internal("Could not read a value").with_detail(e.to_string()))
}

type PgQuery<'q> = sqlx::query::Query<'q, Postgres, PgArguments>;
type MySqlQuery<'q> = sqlx::query::Query<'q, MySql, MySqlArguments>;
type SqliteQuery<'q> = sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>>;

fn pg_array_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn pg_array_element(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Bool(v) => v.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => pg_array_string(s),
        Value::Array(_) => pg_array_text(value)?.expect("array value"),
        Value::Object(o) if o.contains_key("$bytes") => {
            let bytes = binary(value)?.expect("binary object");
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut encoded = String::with_capacity(2 + bytes.len() * 2);
            encoded.push_str("\\x");
            for byte in bytes {
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
            // The array parser consumes one backslash layer, leaving bytea's \x hex input.
            pg_array_string(&encoded)
        }
        // Other JSON objects can be elements of JSON/JSONB arrays; their compact text is one
        // quoted array element rather than PostgreSQL array syntax.
        Value::Object(_) => pg_array_string(&value.to_string()),
    })
}

fn pg_array_text(value: &Value) -> Result<Option<String>, String> {
    let Value::Array(items) = value else {
        return Ok(None);
    };
    Ok(Some(format!(
        "{{{}}}",
        items
            .iter()
            .map(pg_array_element)
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    )))
}

/// One JSON cell as the value each engine binds. The classification is the same everywhere;
/// only the final bind differs, because the drivers accept different argument types.
enum Bound<'a> {
    Null,
    Bool(bool),
    Int(i64),
    /// A `u64` above `i64::MAX`; only MySQL binds it as an integer.
    UInt(u64),
    Float(f64),
    Text(Cow<'a, str>),
    Bytes(Vec<u8>),
    /// A PostgreSQL array literal.
    Array(String),
}

/// The one JSON → bound-value rule. The engines agree on everything except a `u64` above
/// `i64::MAX`: MySQL has a real unsigned integer argument, while PG and SQLite bind its decimal
/// text (SQLite integers are `i64`, and PG's text keeps the value for numeric/numeric-like
/// targets). `keep_arrays` is PostgreSQL's array-literal step.
fn bound<'a>(engine: SqlEngine, b: &BoundValue<'a>) -> Result<Bound<'a>, String> {
    Ok(match b.value {
        Value::Null => Bound::Null,
        Value::Bool(v) => Bound::Bool(*v),
        Value::Number(n) if n.is_i64() => Bound::Int(n.as_i64().unwrap_or_default()),
        Value::Number(n) if n.is_u64() => {
            let v = n.as_u64().unwrap_or_default();
            if v <= i64::MAX as u64 {
                Bound::Int(v as i64)
            } else if engine == SqlEngine::Mysql {
                Bound::UInt(v)
            } else {
                Bound::Text(Cow::Owned(v.to_string()))
            }
        }
        Value::Number(n) => Bound::Float(n.as_f64().unwrap_or_default()),
        Value::String(s) => Bound::Text(Cow::Borrowed(s)),
        other => {
            if let Some(bytes) = binary(other)? {
                Bound::Bytes(bytes)
            } else if engine == SqlEngine::Pg && b.data_type.ends_with("[]") {
                match pg_array_text(other)? {
                    Some(array) => Bound::Array(array),
                    None => Bound::Text(Cow::Owned(other.to_string())),
                }
            } else {
                Bound::Text(Cow::Owned(other.to_string()))
            }
        }
    })
}

fn bind_pg<'q>(q: PgQuery<'q>, b: Bound<'q>) -> PgQuery<'q> {
    match b {
        Bound::Null => q.bind(Option::<String>::None),
        Bound::Bool(v) => q.bind(v),
        Bound::Int(v) => q.bind(v),
        // Unreachable: `bound` never hands PostgreSQL an unsigned integer.
        Bound::UInt(v) => q.bind(v as i64),
        Bound::Float(v) => q.bind(v),
        Bound::Text(Cow::Borrowed(s)) => q.bind(s),
        Bound::Text(Cow::Owned(s)) => q.bind(s),
        Bound::Bytes(b) => q.bind(b),
        Bound::Array(a) => q.bind(a),
    }
}

fn bind_mysql<'q>(q: MySqlQuery<'q>, b: Bound<'q>) -> MySqlQuery<'q> {
    match b {
        Bound::Null => q.bind(Option::<String>::None),
        Bound::Bool(v) => q.bind(v),
        Bound::Int(v) => q.bind(v),
        Bound::UInt(v) => q.bind(v),
        Bound::Float(v) => q.bind(v),
        Bound::Text(Cow::Borrowed(s)) => q.bind(s),
        Bound::Text(Cow::Owned(s)) => q.bind(s),
        Bound::Bytes(b) => q.bind(b),
        // Unreachable: array literals are PostgreSQL's.
        Bound::Array(a) => q.bind(a),
    }
}

fn bind_sqlite<'q>(q: SqliteQuery<'q>, b: Bound<'q>) -> SqliteQuery<'q> {
    match b {
        Bound::Null => q.bind(Option::<String>::None),
        Bound::Bool(v) => q.bind(v),
        Bound::Int(v) => q.bind(v),
        // Unreachable: `bound` never hands SQLite an unsigned integer.
        Bound::UInt(v) => q.bind(v as i64),
        Bound::Float(v) => q.bind(v),
        Bound::Text(Cow::Borrowed(s)) => q.bind(s),
        Bound::Text(Cow::Owned(s)) => q.bind(s),
        Bound::Bytes(b) => q.bind(b),
        // Unreachable: array literals are PostgreSQL's.
        Bound::Array(a) => q.bind(a),
    }
}

/// Runs one batch statement with its binds; `Err` is the server's message for the failure.
async fn execute<'s, 'v: 's>(
    c: &mut Conn,
    sql: &'s str,
    binds: &'s [BoundValue<'v>],
) -> Result<u64, String> {
    match c {
        Conn::Pg(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_pg(q, bound(SqlEngine::Pg, b)?);
            }
            q.execute(o.connection())
                .await
                .map(|r| r.rows_affected())
                .map_err(|e| AppError::from(e).message)
        }
        Conn::MySql(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_mysql(q, bound(SqlEngine::Mysql, b)?);
            }
            q.execute(o.connection())
                .await
                .map(|r| r.rows_affected())
                .map_err(|e| AppError::from(e).message)
        }
        Conn::Sqlite(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_sqlite(q, bound(SqlEngine::Sqlite, b)?);
            }
            q.execute(o.connection())
                .await
                .map(|r| r.rows_affected())
                .map_err(|e| AppError::from(e).message)
        }
    }
}

/// First column of the first row as text; `None` when no row matches.
async fn fetch_text<'s, 'v: 's>(
    c: &mut Conn,
    sql: &'s str,
    binds: &'s [BoundValue<'v>],
) -> Result<Option<Option<String>>, AppError> {
    match c {
        Conn::Pg(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_pg(q, bound(SqlEngine::Pg, b).map_err(AppError::invalid)?);
            }
            first_text(q.fetch_optional(o.connection()).await?)
        }
        Conn::MySql(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_mysql(q, bound(SqlEngine::Mysql, b).map_err(AppError::invalid)?);
            }
            first_text(q.fetch_optional(o.connection()).await?)
        }
        Conn::Sqlite(o) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_sqlite(q, bound(SqlEngine::Sqlite, b).map_err(AppError::invalid)?);
            }
            first_text(q.fetch_optional(o.connection()).await?)
        }
    }
}

/// How long a cancelled save waits for the reply of `COMMIT`, so it can say whether the change
/// applied.
const COMMIT_REPLY_WAIT: Duration = Duration::from_secs(2);

/// The savepoint name: one constant, so `SAVEPOINT` / `ROLLBACK TO` / `RELEASE` cannot drift.
const SAVEPOINT: &str = "sandb_grid_edit";

/// `action` is empty (create), `ROLLBACK TO` or `RELEASE`.
fn savepoint_sql(action: &str) -> String {
    if action.is_empty() {
        format!("SAVEPOINT {SAVEPOINT}")
    } else {
        format!("{action} SAVEPOINT {SAVEPOINT}")
    }
}

/// Wakes when the save's `query_id` is cancelled; never without one.
async fn until_cancelled(cancelled: Option<&CancellationToken>) {
    match cancelled {
        Some(c) => c.cancelled().await,
        None => core::future::pending::<()>().await,
    }
}

fn is_cancelled(cancelled: Option<&CancellationToken>) -> bool {
    cancelled.is_some_and(CancellationToken::is_cancelled)
}

fn save_cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Save cancelled")
}

/// The local save stopped, but the commit may still have reached the server.
fn save_maybe_applied() -> AppError {
    save_cancelled().with_detail("The save may have applied before the cancel. Reload to check.")
}

/// Rolls the transaction back, best effort: the server may be the reason for the cancel.
async fn rollback(c: &mut Conn) {
    let _ = tokio::time::timeout(COMMIT_REPLY_WAIT, c.end_tx_unbounded(false)).await;
}

/// `Err` is the cancel: the caller rolls back and reports it.
async fn run_operations(
    c: &mut Conn,
    target: &Target,
    engine: SqlEngine,
    operations: &[EditOperation],
    cancelled: Option<&CancellationToken>,
) -> Result<Option<EditFailure>, AppError> {
    for (index, op) in operations.iter().enumerate() {
        if is_cancelled(cancelled) {
            return Err(save_cancelled());
        }
        let result = match operation_sql(target, engine, op) {
            Ok((sql, binds)) => execute(c, &sql, &binds).await,
            Err(message) => return Ok(Some(EditFailure { index, message })),
        };
        match result {
            Ok(0) if matches!(op.kind, EditKind::Update | EditKind::Delete) => {
                return Ok(Some(EditFailure {
                    index,
                    message: "The row no longer exists or its primary key changed".into(),
                }));
            }
            Ok(_) => {}
            // A statement the server killed fails here; the flag says it was the cancel.
            Err(_) if is_cancelled(cancelled) => return Err(save_cancelled()),
            Err(message) => return Ok(Some(EditFailure { index, message })),
        }
    }
    if is_cancelled(cancelled) {
        return Err(save_cancelled());
    }
    Ok(None)
}

/// Runs the batch in its own transaction and rolls it back on a failure or cancel.
/// `Ok(None)` leaves a completed batch for the caller to COMMIT, so the caller detaches the
/// registry entry first and no cancel kill can land on the COMMIT.
async fn run_autocommit(
    c: &mut Conn,
    target: &Target,
    engine: SqlEngine,
    operations: &[EditOperation],
    cancelled: Option<&CancellationToken>,
) -> Result<Option<ApplyEditsResult>, AppError> {
    if is_cancelled(cancelled) {
        return Err(save_cancelled());
    }
    c.begin().await?;
    match run_operations(c, target, engine, operations, cancelled).await {
        Err(e) => {
            rollback(c).await;
            Err(e)
        }
        Ok(Some(failed)) => {
            c.end_tx(false).await?;
            Ok(Some(ApplyEditsResult {
                failed: Some(failed),
                partial: false,
            }))
        }
        Ok(None) => Ok(None),
    }
}

/// The COMMIT, raced with the cancel like the MongoDB save: one already sent gets a moment for
/// its reply; without one the change may have applied, so it is reported that way.
async fn commit(
    c: &mut Conn,
    cancelled: Option<&CancellationToken>,
) -> Result<ApplyEditsResult, AppError> {
    if is_cancelled(cancelled) {
        rollback(c).await;
        return Err(save_cancelled());
    }
    let committed = {
        let commit = c.end_tx_unbounded(true);
        tokio::pin!(commit);
        let sent = tokio::select! {
            r = &mut commit => Some(r),
            () = until_cancelled(cancelled) => None,
        };
        match sent {
            Some(r) => Some(r),
            // The commit is in flight: it may apply, and a rollback cannot take it back.
            None => tokio::time::timeout(COMMIT_REPLY_WAIT, &mut commit)
                .await
                .ok(),
        }
    };
    let Some(committed) = committed else {
        return Err(save_maybe_applied());
    };
    match committed {
        Ok(()) => Ok(ApplyEditsResult {
            failed: None,
            partial: false,
        }),
        // A COMMIT that failed while the flag was set may still have committed: MySQL's KILL
        // QUERY repeats, and a deferred constraint can fail after the flag. Never claim a clean
        // cancel for it.
        Err(_) if is_cancelled(cancelled) => Err(save_maybe_applied()),
        Err(e) => {
            rollback(c).await;
            Err(e)
        }
    }
}

/// One retry for the savepoint rollback: a cancel kill delivered just as the batch stopped is
/// queued on the connection and aborts the transaction it lands in; ROLLBACK TO SAVEPOINT is the
/// one statement that works then, and the retry runs clean.
async fn rollback_to_savepoint(c: &mut Conn) -> Result<(), AppError> {
    let sql = savepoint_sql("ROLLBACK TO");
    match c.control(&sql).await {
        Err(_) => c.control(&sql).await,
        ok => ok,
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_in_session(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
    tab_id: &str,
    save: Option<(&str, &CancellationToken)>,
    target: &Target,
    operations: &[EditOperation],
) -> Result<ApplyEditsResult, AppError> {
    let cancelled = save.map(|(_, cancelled)| cancelled);
    let id = save.map(|(id, _)| id);
    let mut s = session::for_tab(ctx, conn, database, tab_id).await?;
    let had_tx = s.tx_open;
    let r = async {
        session::begin_if_needed(&mut s).await?;
        if is_cancelled(cancelled) {
            return Err(save_cancelled());
        }
        s.conn.control(&savepoint_sql("")).await?;
        // Attached only while the batch statements run: a cancel that arrives earlier sets the
        // flag and sends no kill, and one that arrives later leaves no kill queued for the
        // user's next run in this tab.
        let attached = s.conn.attach(ctx.queries, id).await.unwrap_or_default();
        let result = run_operations(&mut s.conn, target, engine, operations, cancelled).await;
        drop(attached);
        match result {
            // The cancel: undo the batch only, the user's transaction stays open. A kill sent
            // just as the batch stopped can abort the transaction; the rollback retry repairs
            // that, and the caller below reports the case where it could not.
            Err(e) => {
                let _ = rollback_to_savepoint(&mut s.conn).await;
                let _ = s.conn.control(&savepoint_sql("RELEASE")).await;
                Err(e)
            }
            Ok(Some(failed)) => {
                rollback_to_savepoint(&mut s.conn).await?;
                s.conn.control(&savepoint_sql("RELEASE")).await?;
                Ok(ApplyEditsResult {
                    failed: Some(failed),
                    partial: false,
                })
            }
            Ok(None) => {
                s.conn.control(&savepoint_sql("RELEASE")).await?;
                s.dirty = true;
                Ok(ApplyEditsResult {
                    failed: None,
                    partial: false,
                })
            }
        }
    }
    .await;
    session::refresh(&mut s).await;
    // A cancel whose kill landed on the transaction can leave it aborted (or, when it was
    // empty, gone); say so instead of only "Save cancelled".
    let r = match r {
        Err(e) if e.code == ErrorCode::Cancelled => {
            if s.aborted {
                Err(e.with_detail("The tab's transaction was aborted. Roll it back to continue."))
            } else if had_tx && !s.tx_open {
                Err(e.with_detail("The tab's transaction was rolled back."))
            } else {
                Err(e)
            }
        }
        r => r,
    };
    session::finish(ctx, tab_id, s, false).await;
    r
}

/// Revalidates the source query and applies the whole batch atomically. Production tab sessions
/// use a savepoint inside their manual-commit transaction; only standalone saves COMMIT.
pub async fn apply_edits(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    request: ApplyEditsRequest,
) -> Result<ApplyEditsResult, AppError> {
    if !conn.may_write() {
        return Err(crate::core::connections::read_only_refusal(conn, None));
    }
    let database = target_database(conn, Some(&request.database));
    let target = match resolve(ctx, conn, engine, &database, &request.sql).await? {
        Resolution::NotEditable(reason) => return Err(AppError::invalid(reason)),
        Resolution::Editable(target) => target,
    };
    if request.operations.is_empty() {
        return Ok(ApplyEditsResult {
            failed: None,
            partial: false,
        });
    }
    // The dialog's id puts the save in the editor's registry, so `cancel_query` reaches it with
    // the one mechanism the app has: the token stops the save, and the kill stops its statement.
    let id = request.query_id.as_deref().filter(|id| !id.is_empty());
    let run = match id {
        Some(id) => Some(ctx.queries.register(
            id,
            &conn.id,
            &database,
            conn.engine,
            request.operations.len() + 2,
        )?),
        None => None,
    };
    let cancelled = run.as_ref().map(|r| r.cancelled.clone());
    let flag = cancelled.as_ref();
    if is_cancelled(flag) {
        return Err(save_cancelled());
    }
    let result = if conn.env == Env::Production
        && let Some(tab_id) = request.tab_id.as_deref()
    {
        apply_in_session(
            ctx,
            conn,
            engine,
            &database,
            tab_id,
            id.zip(flag),
            &target,
            &request.operations,
        )
        .await
    } else {
        let pool = ctx.pool_for(conn, Some(&target.database))?;
        let mut c = Conn::acquire(&pool).await?;
        let attached = c.attach(ctx.queries, id).await.unwrap_or_default();
        let done = run_autocommit(&mut c, &target, engine, &request.operations, flag).await;
        drop(attached);
        let r = match done {
            Ok(Some(done)) => Ok(done),
            Ok(None) => commit(&mut c, flag).await,
            Err(e) => Err(e),
        };
        // A cancelled save's statement may still be in flight: close that connection rather
        // than hand a half-open one back to the pool.
        let keep = !r.as_ref().is_err_and(|e| e.code == ErrorCode::Cancelled);
        if keep {
            c.settle(true).await;
        } else {
            c.close().await;
        }
        r
    };
    drop(run);
    result
}

/// The full text of one cell that the grid shows cut at the preview cap, read by primary key so
/// that it can be edited. PostGIS geometry comes back as WKT. A tab with an open transaction
/// reads through it, so the value includes the tab's unsaved changes.
pub async fn cell_value(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    request: CellValueRequest,
) -> Result<Option<String>, AppError> {
    let database = target_database(conn, Some(&request.database));
    let target = match resolve(ctx, conn, engine, &database, &request.sql).await? {
        Resolution::NotEditable(reason) => return Err(AppError::invalid(reason)),
        Resolution::Editable(target) => target,
    };
    let Some(data_type) = target.columns.get(&request.column) else {
        return Err(AppError::invalid(format!(
            "Unknown table column {}",
            request.column
        )));
    };
    let expected: BTreeSet<&str> = target.result.pk.iter().map(String::as_str).collect();
    if request
        .pk
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err(AppError::invalid(
            "The request must contain exactly the primary-key values",
        ));
    }
    let mut binds = Vec::new();
    let predicates = request
        .pk
        .iter()
        .enumerate()
        .map(|(i, (name, value))| {
            let data_type = &target.columns[name.as_str()];
            binds.push(BoundValue { value, data_type });
            format!(
                "{} = {}",
                engine.quote(name),
                engine.placeholder(i + 1, data_type)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let text = engine.text_cast_type();
    let sql = format!(
        "SELECT CAST({} AS {text}) FROM {} WHERE {predicates} LIMIT 1",
        engine.quote(&request.column),
        target.relation
    );
    let read = session::with_tab(
        ctx,
        conn,
        &database,
        request.tab_id.as_deref(),
        async |s| {
            (
                fetch_text(&mut s.conn, &sql, &binds).await,
                Settle::Keep,
            )
        },
    )
    .await?;
    let row = match read {
        Some(r) => r?,
        None => {
            let pool = ctx.pool_for(conn, Some(&target.database))?;
            let mut c = Conn::acquire(&pool).await?;
            let r = fetch_text(&mut c, &sql, &binds).await;
            c.settle(true).await;
            r?
        }
    };
    let Some(value) = row else {
        return Err(AppError::not_found(
            "The row no longer exists or its primary key changed",
        ));
    };
    let base = crate::core::engine::pg_base_type(data_type);
    let spatial =
        engine == SqlEngine::Pg && (base.ends_with("geometry") || base.ends_with("geography"));
    Ok(value.map(|v| match spatial {
        true => crate::core::db::wkb::hex_ewkb_to_wkt(&v, None).unwrap_or(v),
        false => v,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{ConnectionInput, SslMode};
    use crate::core::db::query;
    use crate::core::engine::Engine;
    use crate::core::result::{QueryRun, RunRequest, RunSort};
    use crate::core::state::AppState;

    fn conn_of(state: &AppState, id: &str) -> Connection {
        state.connection(id).unwrap()
    }

    // The tests run on one SQLite connection, so these wrappers route for them.
    async fn editable_result(
        state: &AppState,
        request: EditableRequest,
    ) -> Result<EditableResult, AppError> {
        let conn = conn_of(state, &request.conn_id);
        super::editable_result(&state.db(), &conn, SqlEngine::Sqlite, request).await
    }

    async fn apply_edits(
        state: &AppState,
        request: ApplyEditsRequest,
    ) -> Result<ApplyEditsResult, AppError> {
        let conn = conn_of(state, &request.conn_id);
        super::apply_edits(&state.db(), &conn, SqlEngine::Sqlite, request).await
    }

    async fn run_sql(state: &AppState, req: RunRequest) -> Result<QueryRun, AppError> {
        let conn = conn_of(state, &req.conn_id);
        query::run(&state.db(), &conn, SqlEngine::Sqlite, req).await
    }
    use crate::core::secrets::MemoryStore;
    use sqlx::Connection as _;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

    async fn sqlite_state(env: Env) -> (tempfile::TempDir, AppState, String) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        let file = dir.path().join("grid.db");
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::Executor::execute(
            &mut c,
            sqlx::raw_sql(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER); \
                 INSERT INTO users VALUES (1, 'one', 1); \
                 CREATE TABLE logs (message TEXT); \
                 INSERT INTO logs VALUES ('first'); \
                 CREATE TABLE docs (id INTEGER PRIMARY KEY, payload JSON)",
            ),
        )
        .await
        .unwrap();
        c.close().await.unwrap();
        let conn = state.create_connection(
            ConnectionInput {
                name: "sqlite".into(),
                engine: Engine::Sqlite,
                env,
                host: String::new(),
                port: 0,
                database: "main".into(),
                user: String::new(),
                ssl: SslMode::Disable,
                file: file.to_string_lossy().into_owned(),
                ai_write: false,
                read_only: false,
                version: None,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
            },
            None,
        )
        .unwrap();
        (dir, state, conn.id)
    }

    fn op(kind: EditKind, pk: &[(&str, i64)], set: &[(&str, Value)]) -> EditOperation {
        EditOperation {
            key: uuid::Uuid::new_v4().to_string(),
            kind,
            pk: pk
                .iter()
                .map(|(k, v)| (k.to_string(), Value::from(*v)))
                .collect(),
            set: set
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            old: BTreeMap::new(),
            document: None,
        }
    }

    async fn save(
        state: &AppState,
        conn_id: &str,
        operations: Vec<EditOperation>,
        tab_id: Option<&str>,
    ) -> ApplyEditsResult {
        apply_edits(
            state,
            ApplyEditsRequest {
                conn_id: conn_id.into(),
                database: "main".into(),
                sql: "SELECT id, name, age FROM users".into(),
                tab_id: tab_id.map(str::to_string),
                query_id: None,
                operations,
            },
        )
        .await
        .unwrap()
    }

    async fn user_rows(state: &AppState, conn_id: &str) -> Vec<Vec<Value>> {
        run_sql(
            state,
            RunRequest {
                conn_id: conn_id.into(),
                database: Some("main".into()),
                sql: "SELECT id, name, age FROM users ORDER BY id".into(),
                query_id: uuid::Uuid::new_v4().to_string(),
                limit: None,
                tab_id: None,
                sort: None,
            },
        )
        .await
        .unwrap()
        .statements
        .remove(0)
        .rows
    }

    fn simple(engine: SqlEngine, sql: &str) -> Option<SimpleQuery> {
        match parse_simple(engine, sql).unwrap() {
            ParsedQuery::Simple(q) => Some(q),
            ParsedQuery::NotEditable(_) => None,
        }
    }

    #[test]
    fn accepts_plain_single_table_selects() {
        for sql in [
            "SELECT * FROM users",
            "SELECT id, name FROM users WHERE active = true ORDER BY name LIMIT 20",
            "SELECT u.id, u.name AS name FROM public.users AS u",
        ] {
            assert!(simple(SqlEngine::Pg, sql).is_some(), "{sql}");
        }
    }

    #[test]
    fn refuses_ambiguous_or_computed_selects() {
        for sql in [
            "SELECT a.id FROM a JOIN b ON b.id = a.id",
            "WITH x AS (SELECT * FROM a) SELECT * FROM x",
            "SELECT id FROM a UNION SELECT id FROM b",
            "SELECT DISTINCT id FROM a",
            "SELECT id, count(*) FROM a GROUP BY id",
            "SELECT lower(name) FROM a",
            "SELECT id AS other FROM a",
            "SELECT id AS ID FROM a",
            "SELECT id, id FROM a",
            "SELECT *, name AS id FROM a",
            "SELECT *, * FROM a",
            "SELECT * FROM (SELECT * FROM a) x",
            "UPDATE a SET name = 'x'",
        ] {
            assert!(simple(SqlEngine::Pg, sql).is_none(), "{sql}");
        }
    }

    #[test]
    fn mysql_table_names_preserve_case() {
        let query = simple(SqlEngine::Mysql, "SELECT id FROM Sales.CustomerOrder").unwrap();
        let (schema, table, relation) = table_names(&query, "other");
        assert_eq!(schema, None);
        assert_eq!(table, "CustomerOrder");
        assert_eq!(relation, "`Sales`.`CustomerOrder`");
    }

    #[test]
    fn quotes_identifiers_without_inlining_values() {
        let target = Target {
            result: EditableResult {
                table: Some("user".into()),
                schema: Some("public".into()),
                pk: vec!["id".into()],
                reason: None,
                transactions: None,
            },
            database: "db".into(),
            relation: "\"public\".\"user\"".into(),
            columns: BTreeMap::from([
                ("id".into(), "bigint".into()),
                ("name".into(), "character varying(255)".into()),
            ]),
        };
        let op = EditOperation {
            key: "0".into(),
            kind: EditKind::Update,
            pk: BTreeMap::from([("id".into(), Value::String("42".into()))]),
            set: BTreeMap::from([(
                "name".into(),
                Value::String("x'; DROP TABLE user; --".into()),
            )]),
            old: BTreeMap::new(),
            document: None,
        };
        let (sql, binds) = operation_sql(&target, SqlEngine::Pg, &op).unwrap();
        assert_eq!(
            sql,
            "UPDATE \"public\".\"user\" SET \"name\" = CAST($1 AS character varying) WHERE \"id\" = CAST($2 AS bigint)"
        );
        assert!(!sql.contains("DROP TABLE"));
        assert_eq!(binds.len(), 2);
        assert_eq!(
            crate::core::engine::pg_base_type("timestamp(3) with time zone"),
            "timestamp with time zone"
        );
        assert_eq!(crate::core::engine::pg_base_type("character(2)[]"), "bpchar[]");
    }

    #[test]
    fn pg_char_and_bit_sql_uses_unbounded_comparable_types() {
        let update = |pk_type: &str, set_type: &str| {
            let target = Target {
                result: EditableResult {
                    table: Some("t".into()),
                    schema: Some("public".into()),
                    pk: vec!["id".into()],
                    reason: None,
                    transactions: None,
                },
                database: "db".into(),
                relation: "\"public\".\"t\"".into(),
                columns: BTreeMap::from([
                    ("id".into(), pk_type.into()),
                    ("value".into(), set_type.into()),
                ]),
            };
            let op = EditOperation {
                key: "0".into(),
                kind: EditKind::Update,
                pk: BTreeMap::from([("id".into(), Value::from("key"))]),
                set: BTreeMap::from([("value".into(), Value::from("new"))]),
                old: BTreeMap::new(),
                document: None,
            };
            operation_sql(&target, SqlEngine::Pg, &op).unwrap().0
        };
        assert_eq!(
            update("character(8)", "character(4)"),
            "UPDATE \"public\".\"t\" SET \"value\" = CAST($1 AS bpchar) WHERE \"id\" = CAST($2 AS bpchar)"
        );
        assert_eq!(
            update("bit(8)", "bit(4)"),
            "UPDATE \"public\".\"t\" SET \"value\" = CAST($1 AS bit varying) WHERE \"id\" = CAST($2 AS bit varying)"
        );
    }

    #[test]
    fn pg_array_text_handles_null_nested_empty_and_escaped_strings() {
        assert_eq!(
            pg_array_text(&serde_json::json!([])).unwrap().unwrap(),
            "{}"
        );
        assert_eq!(
            pg_array_text(&serde_json::json!([1, null, true]))
                .unwrap()
                .unwrap(),
            "{1,NULL,true}"
        );
        assert_eq!(
            pg_array_text(&serde_json::json!([[1, 2], [], ["x"]]))
                .unwrap()
                .unwrap(),
            r#"{{1,2},{},{"x"}}"#
        );
        assert_eq!(
            pg_array_text(&serde_json::json!(["a\"b", "c\\d", "{x,y}", "NULL"]))
                .unwrap()
                .unwrap(),
            r#"{"a\"b","c\\d","{x,y}","NULL"}"#
        );
    }

    #[test]
    fn pg_bytea_arrays_use_hex_and_reject_truncated_previews() {
        assert_eq!(
            pg_array_text(&serde_json::json!([
                {"$bytes": "AA==", "len": 1},
                {"$bytes": "ACr/", "len": 3}
            ]))
            .unwrap()
            .unwrap(),
            r#"{"\\x00","\\x002aff"}"#
        );
        let error = pg_array_text(&serde_json::json!([
            {"$bytes": "AA==", "len": 2}
        ]))
        .unwrap_err();
        assert_eq!(error, "A truncated binary preview cannot be saved");
    }

    #[test]
    fn empty_insert_uses_database_defaults() {
        let target = Target {
            result: EditableResult {
                table: Some("t".into()),
                schema: None,
                pk: vec!["id".into()],
                reason: None,
                transactions: None,
            },
            database: "main".into(),
            relation: "\"main\".\"t\"".into(),
            columns: BTreeMap::from([("id".into(), "INTEGER".into())]),
        };
        let op = EditOperation {
            key: "new".into(),
            kind: EditKind::Insert,
            pk: BTreeMap::new(),
            set: BTreeMap::new(),
            old: BTreeMap::new(),
            document: None,
        };
        let (sql, binds) = operation_sql(&target, SqlEngine::Sqlite, &op).unwrap();
        assert_eq!(sql, "INSERT INTO \"main\".\"t\" DEFAULT VALUES");
        assert!(binds.is_empty());
    }

    #[tokio::test]
    async fn sqlite_metadata_requires_keys_and_supports_safe_rowid() {
        let (_dir, state, conn_id) = sqlite_state(Env::Local).await;
        let describe = |sql: &str| EditableRequest {
            conn_id: conn_id.clone(),
            database: "main".into(),
            sql: sql.into(),
        };
        let editable = editable_result(&state, describe("SELECT id, name FROM users"))
            .await
            .unwrap();
        assert_eq!(editable.table.as_deref(), Some("users"));
        assert_eq!(editable.pk, ["id"]);
        assert!(editable.reason.is_none());
        assert!(
            editable_result(&state, describe("SELECT id AS id, name FROM users"))
                .await
                .unwrap()
                .reason
                .is_none()
        );
        for sql in [
            "SELECT ID, name FROM users",
            "SELECT id, id FROM users",
            "SELECT *, id FROM users",
            "SELECT *, name AS id FROM users",
        ] {
            assert!(
                editable_result(&state, describe(sql))
                    .await
                    .unwrap()
                    .reason
                    .is_some(),
                "{sql}"
            );
        }
        assert!(
            editable_result(&state, describe("SELECT name FROM users"))
                .await
                .unwrap()
                .reason
                .is_some()
        );
        let rowid = editable_result(&state, describe("SELECT rowid, message FROM logs"))
            .await
            .unwrap();
        assert_eq!(rowid.pk, ["rowid"]);
        assert!(
            editable_result(&state, describe("SELECT * FROM logs"))
                .await
                .unwrap()
                .reason
                .is_some()
        );
        assert!(
            editable_result(&state, describe("SELECT id + 1 AS id FROM users"))
                .await
                .unwrap()
                .reason
                .is_some()
        );
    }

    #[tokio::test]
    async fn sqlite_batches_commit_or_roll_back_atomically() {
        let (_dir, state, conn_id) = sqlite_state(Env::Local).await;
        let failed = save(
            &state,
            &conn_id,
            vec![
                op(
                    EditKind::Update,
                    &[("id", 1)],
                    &[("name", Value::from("changed"))],
                ),
                op(
                    EditKind::Insert,
                    &[],
                    &[("id", Value::from(2)), ("name", Value::from("two"))],
                ),
                op(EditKind::Delete, &[("id", 999)], &[]),
            ],
            None,
        )
        .await;
        assert_eq!(failed.failed.map(|f| f.index), Some(2));
        assert_eq!(user_rows(&state, &conn_id).await[0][1], Value::from("one"));
        assert_eq!(user_rows(&state, &conn_id).await.len(), 1);

        let saved = save(
            &state,
            &conn_id,
            vec![
                op(
                    EditKind::Update,
                    &[("id", 1)],
                    &[("name", Value::from("changed")), ("age", Value::Null)],
                ),
                op(
                    EditKind::Insert,
                    &[],
                    &[("id", Value::from(2)), ("name", Value::from("two"))],
                ),
            ],
            None,
        )
        .await;
        assert!(saved.failed.is_none());
        let rows = user_rows(&state, &conn_id).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![Value::from(1), Value::from("changed"), Value::Null]
        );
        let deleted = save(
            &state,
            &conn_id,
            vec![op(EditKind::Delete, &[("id", 2)], &[])],
            None,
        )
        .await;
        assert!(deleted.failed.is_none());
        assert_eq!(user_rows(&state, &conn_id).await.len(), 1);
    }

    #[tokio::test]
    async fn sqlite_json_objects_and_arrays_bind_as_compact_text() {
        let (_dir, state, conn_id) = sqlite_state(Env::Local).await;
        let saved = apply_edits(
            &state,
            ApplyEditsRequest {
                conn_id: conn_id.clone(),
                database: "main".into(),
                sql: "SELECT id, payload FROM docs".into(),
                tab_id: None,
                query_id: None,
                operations: vec![
                    op(
                        EditKind::Insert,
                        &[],
                        &[
                            ("id", Value::from(1)),
                            ("payload", serde_json::json!({"b": [2, 1]})),
                        ],
                    ),
                    op(
                        EditKind::Insert,
                        &[],
                        &[
                            ("id", Value::from(2)),
                            ("payload", serde_json::json!(["a", 1])),
                        ],
                    ),
                ],
            },
        )
        .await
        .unwrap();
        assert!(saved.failed.is_none());
        let run = run_sql(
            &state,
            RunRequest {
                conn_id,
                database: Some("main".into()),
                sql: "SELECT payload FROM docs ORDER BY id".into(),
                query_id: "json".into(),
                limit: None,
                tab_id: None,
                sort: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(run.statements[0].rows[0][0], Value::from(r#"{"b":[2,1]}"#));
        assert_eq!(run.statements[0].rows[1][0], Value::from(r#"["a",1]"#));
    }

    #[tokio::test]
    async fn sqlite_sort_orders_before_limit_and_keeps_source_sql() {
        let (_dir, state, conn_id) = sqlite_state(Env::Local).await;
        save(
            &state,
            &conn_id,
            vec![op(
                EditKind::Insert,
                &[],
                &[("id", Value::from(2)), ("name", Value::from("two"))],
            )],
            None,
        )
        .await;
        let source = "SELECT id FROM users ORDER BY id ASC";
        let run = run_sql(
            &state,
            RunRequest {
                conn_id,
                database: Some("main".into()),
                sql: source.into(),
                query_id: "sorted".into(),
                limit: Some(1),
                tab_id: None,
                sort: Some(RunSort {
                    column: "id".into(),
                    column_index: 0,
                    ascending: false,
                }),
            },
        )
        .await
        .unwrap();
        assert_eq!(run.statements[0].rows[0][0], Value::from(2));
        assert_eq!(run.statements[0].sql, source);
    }

    #[tokio::test]
    async fn production_savepoint_preserves_manual_rollback() {
        let (_dir, state, conn_id) = sqlite_state(Env::Production).await;
        let saved = save(
            &state,
            &conn_id,
            vec![op(
                EditKind::Update,
                &[("id", 1)],
                &[("name", Value::from("uncommitted"))],
            )],
            Some("tab"),
        )
        .await;
        assert!(saved.failed.is_none());
        let status = session::status(&state, "tab").await.unwrap().unwrap();
        assert!(status.tx_open && status.pending);
        session::end_tx_for(&state, "tab", false).await.unwrap();
        assert_eq!(user_rows(&state, &conn_id).await[0][1], Value::from("one"));
    }

    #[test]
    fn apply_edits_wire_accepts_camel_case_and_omits_an_empty_failure() {
        let request: ApplyEditsRequest = serde_json::from_value(serde_json::json!({
            "connId": "c",
            "database": "main",
            "sql": "SELECT * FROM t",
            "tabId": "tab",
            "operations": [{
                "key": "0", "kind": "update", "pk": {"id": 1}, "set": {"name": "n"}
            }]
        }))
        .unwrap();
        assert_eq!(request.tab_id.as_deref(), Some("tab"));
        // A clean save sends nothing the UI has to read.
        assert_eq!(
            serde_json::to_value(ApplyEditsResult {
                failed: None,
                partial: false
            })
            .unwrap(),
            serde_json::json!({})
        );
    }
}
