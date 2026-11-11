//! Lazy schema browsing: databases → schemas (PG only) → tables and views → columns, plus a
//! capped snapshot for editor autocomplete and the index/constraint/foreign key properties of
//! one table.

mod mysql;
mod pg;
mod sqlite;

use std::collections::HashMap;

use serde::Serialize;

use crate::core::connections::{self, Engine};
use crate::core::db::pool::DbPool;
use crate::core::error::AppError;
use crate::core::mongo;
use crate::core::state::AppState;

/// Columns in one autocomplete snapshot; larger schemas are cut and flagged `truncated`.
pub const SNAPSHOT_MAX_COLUMNS: usize = 2000;
/// Tables in one snapshot. Separate from the column cap so that wide tables early in the
/// order cannot hide later table names from autocomplete.
pub const SNAPSHOT_MAX_TABLES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TableKind {
    Table,
    View,
    /// PG materialized view: it stores rows, but is not a table.
    Matview,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
    /// Server statistics, not an exact count; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_estimate: Option<i64>,
    /// SQLite only: the table has an implicit `rowid` (not `WITHOUT ROWID`, not a view).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_rowid: Option<bool>,
    /// PG only: the parent table when this table is a partition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_of: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub is_pk: bool,
    /// 1-based position in the primary key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pk_ordinal: Option<u32>,
    /// MongoDB only: the share of sampled documents that have this field, 0.0 to 1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<f64>,
    /// MongoDB only: the number of documents in the sample the fields come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTable {
    /// PG schema; absent on MySQL and SQLite (the database is the namespace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<SnapshotColumn>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSnapshot {
    pub database: String,
    pub tables: Vec<SnapshotTable>,
    pub truncated: bool,
}

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
    pub method: Option<String>,
    /// Full `CREATE INDEX` text when the engine gives it (PG, SQLite).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// MongoDB only: the parts of the index that have no SQL counterpart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mongo: Option<MongoIndexInfo>,
}

/// A MongoDB index as `listIndexes` describes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoIndexInfo {
    /// The key document as compact relaxed Extended JSON, e.g. `{"status":1,"createdAt":-1}`.
    pub keys: String,
    pub sparse: bool,
    pub hidden: bool,
    /// `expireAfterSeconds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<i64>,
    /// `partialFilterExpression`, as compact relaxed Extended JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_filter: Option<String>,
    /// The remaining options (collation, weights, default_language, ...), as compact relaxed
    /// Extended JSON; absent when the index has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConstraintKind {
    Primary,
    Unique,
    Check,
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintInfo {
    pub name: String,
    pub kind: ConstraintKind,
    pub columns: Vec<String>,
    /// Check expression or PG `pg_get_constraintdef` text; absent when not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: String,
    /// Referencing side. `schema` is PG only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub table: String,
    pub columns: Vec<String>,
    /// Referenced side. `ref_schema` is PG only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL` or `SET DEFAULT`.
    pub on_update: String,
    pub on_delete: String,
}

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
    pub mongo: Option<MongoCollectionInfo>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MongoCollectionKind {
    #[default]
    Collection,
    View,
    Timeseries,
}

/// A MongoDB collection as `listCollections` and `$collStats` describe it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoCollectionInfo {
    pub kind: MongoCollectionKind,
    /// The whole validator document, as pretty (2-space) relaxed Extended JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_action: Option<String>,
    /// The other `listCollections` options (capped, size, max, timeseries, collation, viewOn,
    /// pipeline, ...) as pretty relaxed Extended JSON; `{}` when there are none.
    pub options: String,
    /// Absent for views, and when the user may not read the stats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<MongoCollectionStats>,
}

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
pub(crate) struct SnapTable {
    pub schema: Option<String>,
    pub table: String,
    pub kind: TableKind,
}

/// One column, ordered by table, at most `SNAPSHOT_MAX_COLUMNS + 1`.
pub(crate) struct SnapRow {
    pub schema: Option<String>,
    pub table: String,
    pub column: String,
    pub data_type: String,
}

/// Every table (up to `SNAPSHOT_MAX_TABLES`), with columns filled in up to `SNAPSHOT_MAX_COLUMNS`.
pub(crate) fn group_snapshot(
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
pub(crate) fn fold_runs<R, K: PartialEq, T>(
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

pub(crate) fn not_found_table(table: &str) -> AppError {
    AppError::not_found(format!("Table {table} not found"))
}

fn engine_of(state: &AppState, conn_id: &str) -> Result<Engine, AppError> {
    Ok(connections::get(state, conn_id)?.engine)
}

/// Databases the user can open. SQLite has exactly one: `main`.
pub async fn list_databases(state: &AppState, conn_id: &str) -> Result<Vec<String>, AppError> {
    let conn = connections::get(state, conn_id)?;
    if conn.engine == Engine::Mongo {
        return mongo::introspect::databases(
            &mongo::client::client(state, conn_id).await?,
            &conn.database,
        )
        .await;
    }
    // PG and MySQL list databases from the connection's own database.
    match connections::pool(state, conn_id)? {
        DbPool::Pg(p) => pg::databases(&p).await,
        DbPool::MySql(p) => mysql::databases(&p).await,
        DbPool::Sqlite(_) => Ok(vec!["main".into()]),
    }
}

/// PG schemas of `database`. Empty for MySQL and SQLite, which have no schema level.
pub async fn list_schemas(
    state: &AppState,
    conn_id: &str,
    database: &str,
) -> Result<Vec<String>, AppError> {
    if engine_of(state, conn_id)? != Engine::Pg {
        // MongoDB has no schema level and, like MySQL and SQLite, never opens a pool here.
        return Ok(vec![]);
    }
    match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => pg::schemas(&p).await,
        _ => Ok(vec![]),
    }
}

/// Tables and views. `schema` is required on PG (default `public`) and ignored elsewhere.
pub async fn list_tables(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
) -> Result<Vec<TableInfo>, AppError> {
    if engine_of(state, conn_id)? == Engine::Mongo {
        return mongo::introspect::tables(&mongo::client::client(state, conn_id).await?, database)
            .await;
    }
    match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => pg::tables(&p, schema.unwrap_or("public")).await,
        DbPool::MySql(p) => mysql::tables(&p, database).await,
        DbPool::Sqlite(p) => sqlite::tables(&p).await,
    }
}

pub async fn list_columns(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<Vec<ColumnInfo>, AppError> {
    if engine_of(state, conn_id)? == Engine::Mongo {
        // An empty collection yields an empty list; only an unknown name is `not_found`.
        return mongo::introspect::columns(
            &mongo::client::client(state, conn_id).await?,
            database,
            table,
        )
        .await;
    }
    let cols = match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => pg::columns(&p, schema.unwrap_or("public"), table).await?,
        DbPool::MySql(p) => mysql::columns(&p, database, table).await?,
        DbPool::Sqlite(p) => sqlite::columns(&p, table).await?,
    };
    if cols.is_empty() {
        return Err(not_found_table(table));
    }
    Ok(cols)
}

pub async fn schema_snapshot(
    state: &AppState,
    conn_id: &str,
    database: &str,
) -> Result<SchemaSnapshot, AppError> {
    if engine_of(state, conn_id)? == Engine::Mongo {
        return mongo::introspect::snapshot(
            &mongo::client::client(state, conn_id).await?,
            database,
        )
        .await;
    }
    let (tables, rows) = match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => (pg::snapshot_tables(&p).await?, pg::snapshot(&p).await?),
        DbPool::MySql(p) => (
            mysql::snapshot_tables(&p, database).await?,
            mysql::snapshot(&p, database).await?,
        ),
        DbPool::Sqlite(p) => (
            sqlite::snapshot_tables(&p).await?,
            sqlite::snapshot(&p).await?,
        ),
    };
    let db = if engine_of(state, conn_id)? == Engine::Sqlite {
        "main".to_string()
    } else {
        database.to_string()
    };
    Ok(group_snapshot(db, tables, rows))
}

/// Indexes, constraints, declared foreign keys and incoming foreign keys of one table. `schema`
/// is PG only (default `public`). A view yields whatever the engine reports, usually nothing.
pub async fn table_properties(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<TableProperties, AppError> {
    if engine_of(state, conn_id)? == Engine::Mongo {
        return mongo::introspect::properties(
            &mongo::client::client(state, conn_id).await?,
            database,
            table,
        )
        .await;
    }
    let props = match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => pg::properties(&p, schema.unwrap_or("public"), table).await?,
        DbPool::MySql(p) => mysql::properties(&p, database, table).await?,
        DbPool::Sqlite(p) => sqlite::properties(&p, table).await?,
    };
    props.ok_or_else(|| not_found_table(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tbl(schema: Option<&str>, table: &str) -> SnapTable {
        SnapTable {
            schema: schema.map(Into::into),
            table: table.into(),
            kind: TableKind::Table,
        }
    }

    fn row(schema: Option<&str>, table: &str, col: &str) -> SnapRow {
        SnapRow {
            schema: schema.map(Into::into),
            table: table.into(),
            column: col.into(),
            data_type: "int".into(),
        }
    }

    #[test]
    fn snapshot_groups_by_schema_and_table() {
        let s = group_snapshot(
            "shop".into(),
            vec![
                tbl(Some("analytics"), "events"),
                tbl(Some("public"), "events"),
            ],
            vec![
                row(Some("analytics"), "events", "id"),
                row(Some("analytics"), "events", "name"),
                row(Some("public"), "events", "id"),
            ],
        );
        assert_eq!(s.tables.len(), 2);
        assert_eq!(s.tables[0].columns.len(), 2);
        assert_eq!(s.tables[1].columns.len(), 1);
        assert!(!s.truncated);
    }

    // Many partition tables can use up the column cap early; autocomplete must still offer
    // the names of the tables that come after it.
    #[test]
    fn column_cap_keeps_every_table_name() {
        let tables: Vec<SnapTable> = (0..300).map(|i| tbl(None, &format!("t{i:03}"))).collect();
        let rows = (0..SNAPSHOT_MAX_COLUMNS + 1)
            .map(|i| row(None, &format!("t{:03}", i / 10), "c"))
            .collect();
        let s = group_snapshot("db".into(), tables, rows);
        assert!(s.truncated);
        assert_eq!(s.tables.len(), 300);
        let n: usize = s.tables.iter().map(|t| t.columns.len()).sum();
        assert_eq!(n, SNAPSHOT_MAX_COLUMNS);
        assert_eq!(s.tables[299].name, "t299");
        assert!(s.tables[299].columns.is_empty());
    }

    #[test]
    fn table_cap_is_flagged() {
        let tables = (0..SNAPSHOT_MAX_TABLES + 1)
            .map(|i| tbl(None, &format!("t{i}")))
            .collect();
        let s = group_snapshot("db".into(), tables, vec![]);
        assert!(s.truncated);
        assert_eq!(s.tables.len(), SNAPSHOT_MAX_TABLES);
    }

    #[test]
    fn json_shapes() {
        let t = TableInfo {
            name: "t".into(),
            kind: TableKind::View,
            row_estimate: None,
            has_rowid: None,
            partition_of: None,
        };
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({"name": "t", "kind": "view"})
        );
        let c = ColumnInfo {
            name: "id".into(),
            data_type: "integer".into(),
            nullable: false,
            default: None,
            is_pk: true,
            pk_ordinal: Some(1),
            frequency: None,
            sample_size: None,
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({"name": "id", "dataType": "integer", "nullable": false, "isPk": true, "pkOrdinal": 1})
        );
    }

    #[test]
    fn fold_runs_groups_consecutive_keys() {
        let rows = vec![("a", 1), ("a", 2), ("b", 3), ("a", 4)];
        let out = fold_runs(rows, |r| r.0, |r| (r.0, vec![r.1]), |t, r| t.1.push(r.1));
        assert_eq!(out, vec![("a", vec![1, 2]), ("b", vec![3]), ("a", vec![4])]);
        let empty: Vec<(&str, i32)> = vec![];
        assert!(fold_runs(empty, |r| r.0, |r| r.1, |_, _| ()).is_empty());
    }

    #[test]
    fn properties_json_shape() {
        let p = TableProperties {
            indexes: vec![IndexInfo {
                name: "t_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                method: None,
                definition: None,
                mongo: None,
            }],
            constraints: vec![ConstraintInfo {
                name: "t_pkey".into(),
                kind: ConstraintKind::Primary,
                columns: vec!["id".into()],
                definition: None,
            }],
            foreign_keys: vec![ForeignKeyInfo {
                name: "t_u_fkey".into(),
                schema: None,
                table: "t".into(),
                columns: vec!["u_id".into()],
                ref_schema: Some("public".into()),
                ref_table: "u".into(),
                ref_columns: vec!["id".into()],
                on_update: "NO ACTION".into(),
                on_delete: "CASCADE".into(),
            }],
            references: vec![],
            mongo: None,
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "indexes": [{"name": "t_pkey", "columns": ["id"], "unique": true, "primary": true}],
                "constraints": [{"name": "t_pkey", "kind": "primary", "columns": ["id"]}],
                "foreignKeys": [{
                    "name": "t_u_fkey", "table": "t", "columns": ["u_id"], "refSchema": "public",
                    "refTable": "u", "refColumns": ["id"], "onUpdate": "NO ACTION", "onDelete": "CASCADE"
                }],
                "references": []
            })
        );
    }
}
