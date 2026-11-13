//! Lazy schema browsing: databases → schemas (PG only) → tables and views → columns, plus a
//! capped snapshot for editor autocomplete and the index/constraint/foreign key properties of
//! one table.

mod mysql;
mod pg;
mod sqlite;

use crate::core::connections::Connection;
use crate::core::db::pool::DbPool;
use crate::core::engine::SqlEngine;
use crate::core::error::AppError;
use crate::core::result::{
    ColumnInfo, ConstraintInfo, ConstraintKind, ForeignKeyInfo, IndexInfo, SNAPSHOT_MAX_COLUMNS,
    SNAPSHOT_MAX_TABLES, SchemaSnapshot, SnapRow, SnapTable, TableInfo, TableKind, TableProperties,
    fold_runs, group_snapshot, not_found_table,
};
use crate::core::db::ctx::Ctx;

/// Databases the user can open. SQLite has exactly one: `main`.
pub async fn list_databases(
    ctx: &Ctx<'_>,
    conn: &Connection,
) -> Result<Vec<String>, AppError> {
    // PG and MySQL list databases from the connection's own database.
    match ctx.pool_for(conn, None)? {
        DbPool::Pg(p) => pg::databases(&p).await,
        DbPool::MySql(p) => mysql::databases(&p).await,
        DbPool::Sqlite(_) => Ok(vec!["main".into()]),
    }
}

/// PG schemas of `database`. Empty for MySQL and SQLite, which have no schema level.
pub async fn list_schemas(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
) -> Result<Vec<String>, AppError> {
    if engine != SqlEngine::Pg {
        return Ok(vec![]);
    }
    match ctx.pool_for(conn, Some(database))? {
        DbPool::Pg(p) => pg::schemas(&p).await,
        _ => Ok(vec![]),
    }
}

/// Tables and views. `schema` is required on PG (default `public`) and ignored elsewhere.
pub async fn list_tables(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    schema: Option<&str>,
) -> Result<Vec<TableInfo>, AppError> {
    match ctx.pool_for(conn, Some(database))? {
        DbPool::Pg(p) => pg::tables(&p, schema.unwrap_or("public")).await,
        DbPool::MySql(p) => mysql::tables(&p, database).await,
        DbPool::Sqlite(p) => sqlite::tables(&p).await,
    }
}

pub async fn list_columns(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<Vec<ColumnInfo>, AppError> {
    let cols = match ctx.pool_for(conn, Some(database))? {
        DbPool::Pg(p) => pg::columns(&p, schema.unwrap_or("public"), table).await?,
        DbPool::MySql(p) => mysql::columns(&p, database, table).await?,
        DbPool::Sqlite(p) => sqlite::columns(&p, table).await?,
    };
    if cols.is_empty() {
        return Err(not_found_table(table));
    }
    Ok(cols)
}

/// Whether `table` has SQLite's implicit rowid. Other engines have no such key; the grid needs
/// a declared key there.
pub async fn has_rowid(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    table: &str,
) -> Result<bool, AppError> {
    match ctx.pool_for(conn, Some(database))? {
        DbPool::Sqlite(p) => sqlite::has_rowid(&p, table).await,
        _ => Ok(false),
    }
}

pub async fn schema_snapshot(
    ctx: &Ctx<'_>,
    conn: &Connection,
    engine: SqlEngine,
    database: &str,
) -> Result<SchemaSnapshot, AppError> {
    let (tables, rows) = match ctx.pool_for(conn, Some(database))? {
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
    let db = if engine == SqlEngine::Sqlite {
        "main".to_string()
    } else {
        database.to_string()
    };
    Ok(group_snapshot(db, tables, rows))
}

/// Indexes, constraints, declared foreign keys and incoming foreign keys of one table. `schema`
/// is PG only (default `public`). A view yields whatever the engine reports, usually nothing.
pub async fn table_properties(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<TableProperties, AppError> {
    let props = match ctx.pool_for(conn, Some(database))? {
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
    fn fold_runs_groups_consecutive_keys() {
        let rows = vec![("a", 1), ("a", 2), ("b", 3), ("a", 4)];
        let out = fold_runs(rows, |r| r.0, |r| (r.0, vec![r.1]), |t, r| t.1.push(r.1));
        assert_eq!(out, vec![("a", vec![1, 2]), ("b", vec![3]), ("a", vec![4])]);
        let empty: Vec<(&str, i32)> = vec![];
        assert!(fold_runs(empty, |r| r.0, |r| r.1, |_, _| ()).is_empty());
    }
}
