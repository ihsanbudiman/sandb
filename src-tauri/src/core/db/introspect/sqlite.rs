//! SQLite: `sqlite_master` plus the table-valued pragmas (`pragma_table_info`,
//! `pragma_index_list`, …). One database, `main`.

use sqlx::SqlitePool;

use super::{
    ColumnInfo, ConstraintInfo, ConstraintKind, ForeignKeyInfo, IndexInfo, SNAPSHOT_MAX_COLUMNS,
    SNAPSHOT_MAX_TABLES, SnapRow, SnapTable, TableInfo, TableKind, TableProperties, fold_runs,
};
use crate::core::error::AppError;

fn kind(t: &str) -> TableKind {
    if t == "view" {
        TableKind::View
    } else {
        TableKind::Table
    }
}

/// `CREATE TABLE … ) WITHOUT ROWID` has no implicit rowid.
pub(crate) fn without_rowid(create_sql: &str) -> bool {
    let compact: String = create_sql
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    compact.trim_end_matches(';').ends_with("WITHOUTROWID")
}

pub async fn tables(p: &SqlitePool) -> Result<Vec<TableInfo>, AppError> {
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT name, type, sql FROM sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
    )
    .fetch_all(p)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, t, sql)| {
            let k = kind(&t);
            TableInfo {
                name,
                kind: k,
                row_estimate: None,
                has_rowid: Some(
                    k == TableKind::Table && !without_rowid(sql.as_deref().unwrap_or("")),
                ),
                partition_of: None,
            }
        })
        .collect())
}

/// Whether `table` has SQLite's implicit rowid (`WITHOUT ROWID` tables have none).
pub async fn has_rowid(p: &SqlitePool, table: &str) -> Result<bool, AppError> {
    let sql: Option<Option<String>> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ? COLLATE NOCASE",
    )
    .bind(table)
    .fetch_optional(p)
    .await?;
    Ok(sql.is_some_and(|sql| !without_rowid(sql.as_deref().unwrap_or(""))))
}

pub async fn columns(p: &SqlitePool, table: &str) -> Result<Vec<ColumnInfo>, AppError> {
    let rows: Vec<(String, String, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT name, CAST(type AS TEXT), \"notnull\", CAST(dflt_value AS TEXT), pk \
         FROM pragma_table_info(?) ORDER BY cid",
    )
    .bind(table)
    .fetch_all(p)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, data_type, not_null, default, pk)| ColumnInfo {
            name,
            data_type,
            nullable: not_null == 0,
            default,
            is_pk: pk > 0,
            pk_ordinal: (pk > 0).then_some(pk as u32),
            frequency: None,
            sample_size: None,
        })
        .collect())
}

pub async fn snapshot_tables(p: &SqlitePool) -> Result<Vec<SnapTable>, AppError> {
    let sql = format!(
        "SELECT name, type FROM sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY name LIMIT {}",
        SNAPSHOT_MAX_TABLES + 1
    );
    let rows: Vec<(String, String)> = sqlx::query_as(&sql).fetch_all(p).await?;
    Ok(rows
        .into_iter()
        .map(|(table, t)| SnapTable {
            schema: None,
            table,
            kind: kind(&t),
        })
        .collect())
}

pub async fn snapshot(p: &SqlitePool) -> Result<Vec<SnapRow>, AppError> {
    let sql = format!(
        "SELECT m.name, c.name, CAST(c.type AS TEXT) \
         FROM sqlite_master m JOIN pragma_table_info(m.name) c \
         WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY m.name, c.cid LIMIT {}",
        SNAPSHOT_MAX_COLUMNS + 1
    );
    let rows: Vec<(String, String, String)> = sqlx::query_as(&sql).fetch_all(p).await?;
    Ok(rows
        .into_iter()
        .map(|(table, column, data_type)| SnapRow {
            schema: None,
            table,
            column,
            data_type,
        })
        .collect())
}

/// The terms of a `CREATE INDEX … ON t (term, …)` column list, as written (with any `COLLATE`
/// or `ASC`/`DESC`). `None` when the list cannot be found. Nothing in that statement may have
/// an unquoted `(` before the list, so the first one opens it.
fn index_terms(sql: &str) -> Option<Vec<String>> {
    let mut terms = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut chars = sql.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\'' | '"' | '`' | '[' => {
                let close = if c == '[' { ']' } else { c };
                // A doubled quote inside is two adjacent quoted runs, which scans the same.
                chars.by_ref().find(|&(_, d)| d == close)?;
            }
            '(' => {
                depth += 1;
                if depth == 1 {
                    start = i + 1;
                }
            }
            ')' if depth == 0 => return None,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    terms.push(sql[start..i].trim().to_string());
                    return Some(terms);
                }
            }
            ',' if depth == 1 => {
                terms.push(sql[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// One key column of an index: index name, unique, origin (`c`, `u`, `pk`), `CREATE INDEX`
/// text (absent for automatic indexes), column name (absent for an expression).
type IndexRow = (String, i64, String, Option<String>, Option<String>);

/// One column of a foreign key: declaring table, id, referenced table, from, to, on update,
/// on delete.
type ForeignKeyRow = (String, i64, String, String, Option<String>, String, String);

fn group_foreign_keys(rows: Vec<ForeignKeyRow>) -> Vec<ForeignKeyInfo> {
    fold_runs(
        rows,
        |r| (r.0.clone(), r.1),
        |(table, id, ref_table, from, to, on_update, on_delete)| ForeignKeyInfo {
            name: format!("fk_{table}_{id}"),
            schema: None,
            table,
            columns: vec![from],
            ref_schema: None,
            ref_table,
            ref_columns: vec![to.unwrap_or_default()],
            on_update,
            on_delete,
        },
        |fk, (_, _, _, from, to, _, _)| {
            fk.columns.push(from);
            fk.ref_columns.push(to.unwrap_or_default());
        },
    )
}

/// `None` when `table` is not a table or view in `main`.
///
/// Engine limits: SQLite does not report CHECK constraints (they would need parsing the
/// `CREATE TABLE` text), so none are listed. Foreign keys have no names in the pragmas; each is
/// named `fk_<table>_<id>` after its `pragma_foreign_key_list` id. The primary key constraint
/// takes the name of its automatic index, or `PRIMARY KEY` for a rowid alias
/// (`INTEGER PRIMARY KEY`), which has no index.
pub async fn properties(p: &SqlitePool, table: &str) -> Result<Option<TableProperties>, AppError> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT type FROM sqlite_master WHERE name = ? AND type IN ('table', 'view')",
    )
    .bind(table)
    .fetch_optional(p)
    .await?;
    match found.as_deref() {
        None => return Ok(None),
        Some("view") => return Ok(Some(TableProperties::default())),
        Some(_) => {}
    }

    let rows: Vec<IndexRow> = sqlx::query_as(
        "SELECT l.name, l.\"unique\", l.origin, m.sql, x.name \
         FROM pragma_index_list(?) l \
         JOIN pragma_index_xinfo(l.name) x ON x.key = 1 \
         LEFT JOIN sqlite_master m ON m.type = 'index' AND m.name = l.name \
         ORDER BY l.origin = 'pk' DESC, l.name, x.seqno",
    )
    .bind(table)
    .fetch_all(p)
    .await?;
    // Each index with whether it backs a UNIQUE constraint (origin `u`).
    let indexes = fold_runs(
        rows,
        |r| r.0.clone(),
        |(name, unique, origin, sql, column)| {
            let index = IndexInfo {
                name,
                columns: vec![column.unwrap_or_default()],
                unique: unique != 0,
                primary: origin == "pk",
                method: None,
                definition: sql,
                mongo: None,
            };
            (index, origin == "u")
        },
        |(i, _), (_, _, _, _, column)| i.columns.push(column.unwrap_or_default()),
    );
    let indexes: Vec<(IndexInfo, bool)> = indexes
        .into_iter()
        .map(|(mut i, u)| {
            // Expression terms have no column name; take their text from the CREATE INDEX.
            if i.columns.iter().any(String::is_empty) {
                let terms = i.definition.as_deref().and_then(index_terms);
                if let Some(terms) = terms.filter(|t| t.len() == i.columns.len()) {
                    for (col, term) in i.columns.iter_mut().zip(terms) {
                        if col.is_empty() {
                            *col = term;
                        }
                    }
                }
            }
            (i, u)
        })
        .collect();

    let pk: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?) WHERE pk > 0 ORDER BY pk")
            .bind(table)
            .fetch_all(p)
            .await?;
    let mut constraints = Vec::new();
    if !pk.is_empty() {
        let name = indexes
            .iter()
            .find(|(i, _)| i.primary)
            .map_or_else(|| "PRIMARY KEY".to_string(), |(i, _)| i.name.clone());
        constraints.push(ConstraintInfo {
            name,
            kind: ConstraintKind::Primary,
            columns: pk,
            definition: None,
        });
    }
    constraints.extend(
        indexes
            .iter()
            .filter(|(_, u)| *u)
            .map(|(i, _)| ConstraintInfo {
                name: i.name.clone(),
                kind: ConstraintKind::Unique,
                columns: i.columns.clone(),
                definition: None,
            }),
    );
    let indexes = indexes.into_iter().map(|(i, _)| i).collect();

    // A foreign key without a column list references the parent's primary key, in order.
    let foreign_keys: Vec<ForeignKeyRow> = sqlx::query_as(
        "SELECT ?, f.id, f.\"table\", f.\"from\", \
           COALESCE(f.\"to\", (SELECT t.name FROM pragma_table_info(f.\"table\") t \
                               WHERE t.pk = f.seq + 1)), \
           f.on_update, f.on_delete \
         FROM pragma_foreign_key_list(?) f ORDER BY f.id, f.seq",
    )
    .bind(table)
    .bind(table)
    .fetch_all(p)
    .await?;
    let references: Vec<ForeignKeyRow> = sqlx::query_as(
        "SELECT m.name, f.id, f.\"table\", f.\"from\", \
           COALESCE(f.\"to\", (SELECT t.name FROM pragma_table_info(f.\"table\") t \
                               WHERE t.pk = f.seq + 1)), \
           f.on_update, f.on_delete \
         FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f \
         WHERE m.type = 'table' AND f.\"table\" = ? COLLATE NOCASE \
         ORDER BY m.name, f.id, f.seq",
    )
    .bind(table)
    .fetch_all(p)
    .await?;

    Ok(Some(TableProperties {
        indexes,
        constraints,
        foreign_keys: group_foreign_keys(foreign_keys),
        references: group_foreign_keys(references),
        mongo: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_without_rowid() {
        assert!(without_rowid(
            "CREATE TABLE t (a PRIMARY KEY) WITHOUT ROWID"
        ));
        assert!(without_rowid(
            "create table t(a primary key)\n  without   rowid ;"
        ));
        assert!(!without_rowid("CREATE TABLE rowid_only (label TEXT)"));
        assert!(!without_rowid(""));
    }

    #[test]
    fn splits_index_terms() {
        assert_eq!(
            index_terms("CREATE INDEX ix ON t (a, lower(b) COLLATE NOCASE, c DESC) WHERE a > 0"),
            Some(vec![
                "a".to_string(),
                "lower(b) COLLATE NOCASE".into(),
                "c DESC".into()
            ])
        );
        assert_eq!(
            index_terms("CREATE INDEX \"i(x\" ON [t(y] (substr(\"a,b\", 1, ')'))"),
            Some(vec!["substr(\"a,b\", 1, ')')".to_string()])
        );
        assert_eq!(index_terms("CREATE INDEX ix ON t"), None);
        assert_eq!(index_terms("CREATE INDEX ix ON t (a"), None);
    }

    #[test]
    fn groups_foreign_key_columns() {
        let row = |id: i64, from: &str, to: Option<&str>| -> ForeignKeyRow {
            (
                "items".into(),
                id,
                "orders".into(),
                from.into(),
                to.map(Into::into),
                "NO ACTION".into(),
                "CASCADE".into(),
            )
        };
        let fks = group_foreign_keys(vec![
            row(0, "order_id", Some("id")),
            row(0, "line", Some("line")),
            row(1, "sku", None),
        ]);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].name, "fk_items_0");
        assert_eq!(fks[0].columns, vec!["order_id", "line"]);
        assert_eq!(fks[0].ref_columns, vec!["id", "line"]);
        assert_eq!(fks[1].ref_columns, vec![""]);
        assert_eq!(fks[1].on_delete, "CASCADE");
    }
}
