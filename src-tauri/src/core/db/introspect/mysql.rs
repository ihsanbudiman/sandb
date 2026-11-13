//! MySQL / MariaDB `information_schema`. The database is the namespace (no schema level).
//! Values are read as raw bytes: MySQL 8 reports some catalog columns as binary strings,
//! which sqlx's typed `String` decode rejects.

use sqlx::mysql::MySqlRow;
use sqlx::MySqlPool;

use super::{
    ColumnInfo, ConstraintInfo, ConstraintKind, ForeignKeyInfo, IndexInfo, SNAPSHOT_MAX_COLUMNS,
    SNAPSHOT_MAX_TABLES, SnapRow, SnapTable, TableInfo, TableKind, TableProperties, fold_runs,
};
use crate::core::db::reader::mysql_text;
use crate::core::engine::SqlEngine;
use crate::core::error::AppError;

/// The browser's `NOT IN` list, from the one system-schema table on the engine descriptor.
fn system_dbs() -> String {
    let names: Vec<String> = SqlEngine::Mysql
        .system_schemas()
        .iter()
        .map(|s| format!("'{}'", s.name))
        .collect();
    format!("({})", names.join(", "))
}

/// A catalog value for display. A value the server could not decode shows as empty: the only
/// consequence is a hidden or blank row in the browser, never SQL text.
fn req(row: &MySqlRow, i: usize) -> String {
    mysql_text(row, i).unwrap_or_default()
}

fn kind(table_type: &str) -> TableKind {
    if table_type.eq_ignore_ascii_case("VIEW") || table_type.eq_ignore_ascii_case("SYSTEM VIEW") {
        TableKind::View
    } else {
        TableKind::Table
    }
}

pub async fn databases(p: &MySqlPool) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
         WHERE SCHEMA_NAME NOT IN {} ORDER BY SCHEMA_NAME",
        system_dbs()
    ))
    .fetch_all(p)
    .await?;
    Ok(rows.iter().map(|r| req(r, 0)).collect())
}

pub async fn tables(p: &MySqlPool, database: &str) -> Result<Vec<TableInfo>, AppError> {
    let rows = sqlx::query(
        "SELECT TABLE_NAME, TABLE_TYPE, CAST(TABLE_ROWS AS CHAR) FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
    )
    .bind(database)
    .fetch_all(p)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let k = kind(&req(r, 1));
            TableInfo {
                name: req(r, 0),
                kind: k,
                row_estimate: match k {
                    TableKind::Table => mysql_text(r, 2).and_then(|s| s.parse().ok()),
                    TableKind::View | TableKind::Matview => None,
                },
                has_rowid: None,
                partition_of: None,
            }
        })
        .collect())
}

pub async fn columns(
    p: &MySqlPool,
    database: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, AppError> {
    let rows = sqlx::query(
        "SELECT c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, \
           CAST(k.ORDINAL_POSITION AS CHAR) \
         FROM information_schema.COLUMNS c \
         LEFT JOIN information_schema.KEY_COLUMN_USAGE k \
           ON k.TABLE_SCHEMA = c.TABLE_SCHEMA AND k.TABLE_NAME = c.TABLE_NAME \
           AND k.COLUMN_NAME = c.COLUMN_NAME AND k.CONSTRAINT_NAME = 'PRIMARY' \
         WHERE c.TABLE_SCHEMA = ? AND c.TABLE_NAME = ? ORDER BY c.ORDINAL_POSITION",
    )
    .bind(database)
    .bind(table)
    .fetch_all(p)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let pk = mysql_text(r, 4).and_then(|s| s.parse::<u32>().ok());
            ColumnInfo {
                name: req(r, 0),
                data_type: req(r, 1),
                nullable: req(r, 2).eq_ignore_ascii_case("YES"),
                // MariaDB reports "no default" on a nullable column as the literal NULL.
                default: mysql_text(r, 3).filter(|d| d != "NULL"),
                is_pk: pk.is_some(),
                pk_ordinal: pk,
                frequency: None,
                sample_size: None,
            }
        })
        .collect())
}

/// Optional `information_schema` features: `STATISTICS.EXPRESSION` (MySQL 8.0.13+),
/// `CHECK_CONSTRAINTS` (MySQL 8.0.16+, MariaDB 10.2+) and its `TABLE_NAME` (MariaDB only; check
/// names are unique per table there but per schema on MySQL).
#[derive(Default)]
struct Features {
    index_expression: bool,
    checks: bool,
    check_table_name: bool,
}

async fn features(p: &MySqlPool) -> Result<Features, AppError> {
    let rows = sqlx::query(
        "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = 'information_schema' AND ( \
           (TABLE_NAME = 'STATISTICS' AND COLUMN_NAME = 'EXPRESSION') \
           OR (TABLE_NAME = 'CHECK_CONSTRAINTS' AND COLUMN_NAME IN ('CHECK_CLAUSE', 'TABLE_NAME')))",
    )
    .fetch_all(p)
    .await?;
    let mut f = Features::default();
    for r in &rows {
        match (
            req(r, 0).to_ascii_uppercase().as_str(),
            req(r, 1).to_ascii_uppercase().as_str(),
        ) {
            ("STATISTICS", "EXPRESSION") => f.index_expression = true,
            ("CHECK_CONSTRAINTS", "CHECK_CLAUSE") => f.checks = true,
            ("CHECK_CONSTRAINTS", "TABLE_NAME") => f.check_table_name = true,
            _ => {}
        }
    }
    Ok(f)
}

struct IndexPart {
    name: String,
    column: String,
    non_unique: bool,
    method: String,
}

fn group_indexes(parts: Vec<IndexPart>) -> Vec<IndexInfo> {
    fold_runs(
        parts,
        |p| p.name.clone(),
        |p| IndexInfo {
            primary: p.name == "PRIMARY",
            unique: !p.non_unique,
            method: (!p.method.is_empty()).then_some(p.method),
            definition: None,
            mongo: None,
            columns: vec![p.column],
            name: p.name,
        },
        |i, p| i.columns.push(p.column),
    )
}

/// Rows arrive sorted by (table, name, position).
struct FkPart {
    table: String,
    name: String,
    column: String,
    ref_table: String,
    ref_column: String,
    on_update: String,
    on_delete: String,
    ref_schema: String,
}

/// Splits foreign keys into the ones `table` declares and the ones pointing at `database.table`.
fn split_foreign_keys(
    parts: Vec<FkPart>,
    database: &str,
    table: &str,
) -> (Vec<ForeignKeyInfo>, Vec<ForeignKeyInfo>) {
    let grouped = fold_runs(
        parts,
        |p| (p.table.clone(), p.name.clone()),
        |p| {
            let incoming = p.ref_schema == database && p.ref_table == table;
            let fk = ForeignKeyInfo {
                name: p.name,
                schema: None,
                table: p.table,
                columns: vec![p.column],
                ref_schema: None,
                ref_table: p.ref_table,
                ref_columns: vec![p.ref_column],
                on_update: p.on_update,
                on_delete: p.on_delete,
            };
            (fk, incoming)
        },
        |(fk, _), p| {
            fk.columns.push(p.column);
            fk.ref_columns.push(p.ref_column);
        },
    );
    let (mut declared, mut references) = (Vec::new(), Vec::new());
    for (fk, incoming) in grouped {
        let outgoing = fk.table == table;
        match (outgoing, incoming) {
            (true, true) => {
                declared.push(fk.clone());
                references.push(fk);
            }
            (true, false) => declared.push(fk),
            (false, _) => references.push(fk),
        }
    }
    (declared, references)
}

/// `None` when `database.table` does not exist. CHECK constraints are listed only when the
/// server has `information_schema.CHECK_CONSTRAINTS`; MySQL reports no columns for them.
pub async fn properties(
    p: &MySqlPool,
    database: &str,
    table: &str,
) -> Result<Option<TableProperties>, AppError> {
    let found = sqlx::query(
        "SELECT 1 FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
    )
    .bind(database)
    .bind(table)
    .fetch_optional(p)
    .await?;
    if found.is_none() {
        return Ok(None);
    }
    let f = features(p).await?;

    // Functional key parts (MySQL 8.0.13+) have no COLUMN_NAME; EXPRESSION holds their text.
    let column = if f.index_expression {
        "COALESCE(COLUMN_NAME, EXPRESSION)"
    } else {
        "COLUMN_NAME"
    };
    let rows = sqlx::query(&format!(
        "SELECT INDEX_NAME, {column}, CAST(NON_UNIQUE AS CHAR), INDEX_TYPE \
         FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX"
    ))
    .bind(database)
    .bind(table)
    .fetch_all(p)
    .await?;
    let indexes = group_indexes(
        rows.iter()
            .map(|r| IndexPart {
                name: req(r, 0),
                column: req(r, 1),
                non_unique: req(r, 2) != "0",
                method: req(r, 3),
            })
            .collect(),
    );

    let rows = sqlx::query(
        "SELECT tc.CONSTRAINT_NAME, tc.CONSTRAINT_TYPE, k.COLUMN_NAME \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.KEY_COLUMN_USAGE k \
           ON k.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA AND k.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
           AND k.TABLE_SCHEMA = tc.TABLE_SCHEMA AND k.TABLE_NAME = tc.TABLE_NAME \
         WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? \
           AND tc.CONSTRAINT_TYPE IN ('PRIMARY KEY', 'UNIQUE') \
         ORDER BY tc.CONSTRAINT_TYPE = 'PRIMARY KEY' DESC, tc.CONSTRAINT_NAME, k.ORDINAL_POSITION",
    )
    .bind(database)
    .bind(table)
    .fetch_all(p)
    .await?;
    let mut constraints = fold_runs(
        rows.iter(),
        |r| req(r, 0),
        |r| ConstraintInfo {
            name: req(r, 0),
            kind: if req(r, 1).eq_ignore_ascii_case("PRIMARY KEY") {
                ConstraintKind::Primary
            } else {
                ConstraintKind::Unique
            },
            columns: vec![req(r, 2)],
            definition: None,
        },
        |c, r| c.columns.push(req(r, 2)),
    );

    if f.checks {
        let same_table = if f.check_table_name {
            " AND cc.TABLE_NAME = tc.TABLE_NAME"
        } else {
            ""
        };
        let rows = sqlx::query(&format!(
            "SELECT tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE \
             FROM information_schema.TABLE_CONSTRAINTS tc \
             JOIN information_schema.CHECK_CONSTRAINTS cc \
               ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
               AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME{same_table} \
             WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? AND tc.CONSTRAINT_TYPE = 'CHECK' \
             ORDER BY tc.CONSTRAINT_NAME"
        ))
        .bind(database)
        .bind(table)
        .fetch_all(p)
        .await?;
        constraints.extend(rows.iter().map(|r| ConstraintInfo {
            name: req(r, 0),
            kind: ConstraintKind::Check,
            columns: vec![],
            definition: mysql_text(r, 1),
        }));
    }

    let rows = sqlx::query(
        "SELECT k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, \
           k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE, k.REFERENCED_TABLE_SCHEMA \
         FROM information_schema.KEY_COLUMN_USAGE k \
         JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
           ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
           AND r.TABLE_NAME = k.TABLE_NAME \
         WHERE k.TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL \
           AND (k.TABLE_NAME = ? OR (k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ?)) \
         ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
    )
    .bind(database)
    .bind(table)
    .bind(database)
    .bind(table)
    .fetch_all(p)
    .await?;
    let parts = rows
        .iter()
        .map(|r| FkPart {
            table: req(r, 0),
            name: req(r, 1),
            column: req(r, 2),
            ref_table: req(r, 3),
            ref_column: req(r, 4),
            on_update: req(r, 5),
            on_delete: req(r, 6),
            ref_schema: req(r, 7),
        })
        .collect();
    let (foreign_keys, references) = split_foreign_keys(parts, database, table);

    Ok(Some(TableProperties {
        indexes,
        constraints,
        foreign_keys,
        references,
        mongo: None,
    }))
}

pub async fn snapshot_tables(p: &MySqlPool, database: &str) -> Result<Vec<SnapTable>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME LIMIT {}",
        SNAPSHOT_MAX_TABLES + 1
    ))
    .bind(database)
    .fetch_all(p)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SnapTable {
            schema: None,
            table: req(r, 0),
            kind: kind(&req(r, 1)),
        })
        .collect())
}

pub async fn snapshot(p: &MySqlPool, database: &str) -> Result<Vec<SnapRow>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION LIMIT {}",
        SNAPSHOT_MAX_COLUMNS + 1
    ))
    .bind(database)
    .fetch_all(p)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SnapRow {
            schema: None,
            table: req(r, 0),
            column: req(r, 1),
            data_type: req(r, 2),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str, column: &str, non_unique: bool) -> IndexPart {
        IndexPart {
            name: name.into(),
            column: column.into(),
            non_unique,
            method: "BTREE".into(),
        }
    }

    #[test]
    fn groups_index_parts() {
        let idx = group_indexes(vec![
            part("PRIMARY", "a", false),
            part("PRIMARY", "b", false),
            part("ix_c", "c", true),
        ]);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].columns, vec!["a", "b"]);
        assert!(idx[0].primary && idx[0].unique);
        assert_eq!(idx[1].columns, vec!["c"]);
        assert!(!idx[1].primary && !idx[1].unique);
        assert_eq!(idx[1].method.as_deref(), Some("BTREE"));
    }

    fn fk(table: &str, name: &str, col: &str, ref_table: &str) -> FkPart {
        FkPart {
            table: table.into(),
            name: name.into(),
            column: col.into(),
            ref_table: ref_table.into(),
            ref_column: format!("r_{col}"),
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
            ref_schema: "shop".into(),
        }
    }

    #[test]
    fn splits_declared_and_incoming_foreign_keys() {
        let (declared, refs) = split_foreign_keys(
            vec![
                fk("items", "fk_order", "order_id", "orders"),
                fk("items", "fk_order", "line", "orders"),
                fk("orders", "fk_customer", "customer_id", "customers"),
                fk("orders", "fk_parent", "parent_id", "orders"),
            ],
            "shop",
            "orders",
        );
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].name, "fk_customer");
        assert_eq!(declared[1].name, "fk_parent");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].table, "items");
        assert_eq!(refs[0].columns, vec!["order_id", "line"]);
        assert_eq!(refs[0].ref_columns, vec!["r_order_id", "r_line"]);
        assert_eq!(refs[1].name, "fk_parent");
    }
}
