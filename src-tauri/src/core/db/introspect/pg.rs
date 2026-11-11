//! PostgreSQL catalogs. Text casts keep decoding independent of catalog column types.

use sqlx::PgPool;
use sqlx::postgres::types::Oid;

use super::{
    ColumnInfo, ConstraintInfo, ConstraintKind, ForeignKeyInfo, IndexInfo, SNAPSHOT_MAX_COLUMNS,
    SNAPSHOT_MAX_TABLES, SnapRow, SnapTable, TableInfo, TableKind, TableProperties,
};
use crate::core::error::AppError;

const SYSTEM_SCHEMAS: &str = "n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
    AND n.nspname NOT LIKE 'pg\\_temp\\_%' AND n.nspname NOT LIKE 'pg\\_toast\\_temp\\_%'";

pub async fn databases(p: &PgPool) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT datname::text FROM pg_database \
         WHERE NOT datistemplate AND datallowconn AND has_database_privilege(datname, 'CONNECT') \
         ORDER BY datname",
    )
    .fetch_all(p)
    .await?)
}

pub async fn schemas(p: &PgPool) -> Result<Vec<String>, AppError> {
    let sql = format!(
        "SELECT n.nspname::text FROM pg_namespace n WHERE {SYSTEM_SCHEMAS} \
         AND has_schema_privilege(n.oid, 'USAGE') ORDER BY n.nspname"
    );
    Ok(sqlx::query_scalar(&sql).fetch_all(p).await?)
}

pub async fn tables(p: &PgPool, schema: &str) -> Result<Vec<TableInfo>, AppError> {
    // A partition names its parent; the parent gets its schema only when it lives elsewhere.
    let rows: Vec<(String, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT c.relname::text, c.relkind::text, \
           CASE WHEN c.reltuples >= 0 THEN c.reltuples::int8 END, \
           CASE WHEN p.oid IS NULL THEN NULL \
             WHEN pn.nspname = n.nspname THEN p.relname::text \
             ELSE format('%I.%I', pn.nspname, p.relname) END \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_inherits i ON c.relispartition AND i.inhrelid = c.oid \
         LEFT JOIN pg_class p ON p.oid = i.inhparent \
         LEFT JOIN pg_namespace pn ON pn.oid = p.relnamespace \
         WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'f', 'v', 'm') \
         ORDER BY c.relname",
    )
    .bind(schema)
    .fetch_all(p)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, relkind, est, parent)| {
            let kind = kind(&relkind);
            TableInfo {
                name,
                kind,
                row_estimate: if kind == TableKind::View { None } else { est },
                has_rowid: None,
                partition_of: parent,
            }
        })
        .collect())
}

fn kind(relkind: &str) -> TableKind {
    match relkind {
        "v" => TableKind::View,
        "m" => TableKind::Matview,
        _ => TableKind::Table,
    }
}

/// name, formatted type, NOT NULL, default expression, position in the primary key.
type ColumnRow = (String, String, bool, Option<String>, Option<i32>);

pub async fn columns(p: &PgPool, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, AppError> {
    let rows: Vec<ColumnRow> = sqlx::query_as(
        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
           pg_get_expr(d.adbin, d.adrelid), \
           array_position(pk.conkey, a.attnum)::int4 \
         FROM pg_attribute a \
         JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         LEFT JOIN pg_constraint pk ON pk.conrelid = c.oid AND pk.contype = 'p' \
         WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, data_type, not_null, default, pk)| ColumnInfo {
            name,
            data_type,
            nullable: !not_null,
            default,
            is_pk: pk.is_some(),
            pk_ordinal: pk.map(|o| o as u32),
            frequency: None,
            sample_size: None,
        })
        .collect())
}

/// `pg_constraint.confupdtype` / `confdeltype` as the SQL action.
fn fk_action(code: &str) -> String {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
    .to_string()
}

/// name, unique, primary, access method, definition, columns (expressions as their text).
type IndexRow = (String, bool, bool, String, String, Vec<String>);
/// name, contype, columns, definition.
type ConstraintRow = (String, String, Vec<String>, String);
/// name, schema, table, columns, ref schema, ref table, ref columns, on update, on delete,
/// declared by this table, references this table.
type ForeignKeyRow = (
    String,
    String,
    String,
    Vec<String>,
    String,
    String,
    Vec<String>,
    String,
    String,
    bool,
    bool,
);

/// `None` when `schema.table` does not exist.
pub async fn properties(
    p: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Option<TableProperties>, AppError> {
    let oid: Option<Oid> = sqlx::query_scalar(
        "SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p', 'f', 'v', 'm')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(p)
    .await?;
    let Some(oid) = oid else {
        return Ok(None);
    };

    let indexes: Vec<IndexRow> = sqlx::query_as(
        "SELECT ic.relname::text, i.indisunique, i.indisprimary, am.amname::text, \
           pg_get_indexdef(i.indexrelid), \
           ARRAY(SELECT CASE WHEN k.attnum = 0 \
                   THEN pg_get_indexdef(i.indexrelid, k.ord::int, true) \
                   ELSE a.attname::text END \
                 FROM unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) \
                 LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                 ORDER BY k.ord) \
         FROM pg_index i \
         JOIN pg_class ic ON ic.oid = i.indexrelid \
         JOIN pg_am am ON am.oid = ic.relam \
         WHERE i.indrelid = $1 \
         ORDER BY i.indisprimary DESC, ic.relname",
    )
    .bind(oid)
    .fetch_all(p)
    .await?;

    let constraints: Vec<ConstraintRow> = sqlx::query_as(
        "SELECT co.conname::text, co.contype::text, \
           ARRAY(SELECT a.attname::text \
                 FROM unnest(co.conkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = co.conrelid AND a.attnum = k.attnum \
                 ORDER BY k.ord), \
           pg_get_constraintdef(co.oid, true) \
         FROM pg_constraint co \
         WHERE co.conrelid = $1 AND co.contype IN ('p', 'u', 'c', 'x') \
         ORDER BY CASE co.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'x' THEN 2 ELSE 3 END, \
           co.conname",
    )
    .bind(oid)
    .fetch_all(p)
    .await?;

    let fks: Vec<ForeignKeyRow> = sqlx::query_as(
        "SELECT co.conname::text, n.nspname::text, c.relname::text, \
           ARRAY(SELECT a.attname::text \
                 FROM unnest(co.conkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = co.conrelid AND a.attnum = k.attnum \
                 ORDER BY k.ord), \
           rn.nspname::text, rc.relname::text, \
           ARRAY(SELECT a.attname::text \
                 FROM unnest(co.confkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = co.confrelid AND a.attnum = k.attnum \
                 ORDER BY k.ord), \
           co.confupdtype::text, co.confdeltype::text, \
           co.conrelid = $1, co.confrelid = $1 \
         FROM pg_constraint co \
         JOIN pg_class c ON c.oid = co.conrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_class rc ON rc.oid = co.confrelid \
         JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
         WHERE co.contype = 'f' AND (co.conrelid = $1 OR co.confrelid = $1) \
         ORDER BY n.nspname, c.relname, co.conname",
    )
    .bind(oid)
    .fetch_all(p)
    .await?;

    let mut props = TableProperties {
        indexes: indexes
            .into_iter()
            .map(
                |(name, unique, primary, method, definition, columns)| IndexInfo {
                    name,
                    columns,
                    unique,
                    primary,
                    method: Some(method),
                    definition: Some(definition),
                    mongo: None,
                },
            )
            .collect(),
        constraints: constraints
            .into_iter()
            .filter_map(|(name, contype, columns, definition)| {
                let kind = match contype.as_str() {
                    "p" => ConstraintKind::Primary,
                    "u" => ConstraintKind::Unique,
                    "c" => ConstraintKind::Check,
                    "x" => ConstraintKind::Exclude,
                    _ => return None,
                };
                Some(ConstraintInfo {
                    name,
                    kind,
                    columns,
                    definition: Some(definition),
                })
            })
            .collect(),
        ..TableProperties::default()
    };
    for (name, sch, tbl, cols, ref_sch, ref_tbl, ref_cols, upd, del, outgoing, incoming) in fks {
        let fk = ForeignKeyInfo {
            name,
            schema: Some(sch),
            table: tbl,
            columns: cols,
            ref_schema: Some(ref_sch),
            ref_table: ref_tbl,
            ref_columns: ref_cols,
            on_update: fk_action(&upd),
            on_delete: fk_action(&del),
        };
        // A self-reference is both declared and referenced.
        if outgoing && incoming {
            props.foreign_keys.push(fk.clone());
            props.references.push(fk);
        } else if outgoing {
            props.foreign_keys.push(fk);
        } else {
            props.references.push(fk);
        }
    }
    Ok(Some(props))
}

pub async fn snapshot_tables(p: &PgPool) -> Result<Vec<SnapTable>, AppError> {
    let sql = format!(
        "SELECT n.nspname::text, c.relname::text, c.relkind::text \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE {SYSTEM_SCHEMAS} AND c.relkind IN ('r', 'p', 'f', 'v', 'm') \
         ORDER BY n.nspname, c.relname LIMIT {}",
        SNAPSHOT_MAX_TABLES + 1
    );
    let rows: Vec<(String, String, String)> = sqlx::query_as(&sql).fetch_all(p).await?;
    Ok(rows
        .into_iter()
        .map(|(schema, table, relkind)| SnapTable {
            schema: Some(schema),
            table,
            kind: kind(&relkind),
        })
        .collect())
}

pub async fn snapshot(p: &PgPool) -> Result<Vec<SnapRow>, AppError> {
    let sql = format!(
        "SELECT n.nspname::text, c.relname::text, a.attname::text, \
           format_type(a.atttypid, a.atttypmod) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
         WHERE {SYSTEM_SCHEMAS} AND c.relkind IN ('r', 'p', 'f', 'v', 'm') \
         ORDER BY n.nspname, c.relname, a.attnum LIMIT {}",
        SNAPSHOT_MAX_COLUMNS + 1
    );
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(&sql).fetch_all(p).await?;
    Ok(rows
        .into_iter()
        .map(|(schema, table, column, data_type)| SnapRow {
            schema: Some(schema),
            table,
            column,
            data_type,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_fk_action_codes() {
        assert_eq!(fk_action("a"), "NO ACTION");
        assert_eq!(fk_action("r"), "RESTRICT");
        assert_eq!(fk_action("c"), "CASCADE");
        assert_eq!(fk_action("n"), "SET NULL");
        assert_eq!(fk_action("d"), "SET DEFAULT");
    }
}
