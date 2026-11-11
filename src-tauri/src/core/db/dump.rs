//! One table as SQL: its DDL, and an export of that DDL plus INSERT statements for every row.
//! Reads only, on the connection's pool (never a tab session), so read-only connections work.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::TryStreamExt;
use serde::Deserialize;
use sqlx::mysql::MySqlRow;
use sqlx::postgres::PgRow;
use sqlx::{Decode, MySql, MySqlPool, PgPool, Row, Sqlite, SqlitePool, TypeInfo, ValueRef};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use crate::core::connections::{self, Connection, Engine};
use crate::core::db::export::{self, ExportResult};
use crate::core::db::introspect::not_found_table;
use crate::core::db::pool::DbPool;
use crate::core::db::value::SqliteRaw;
use crate::core::error::AppError;
use crate::core::mongo;
use crate::core::state::AppState;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableExportRequest {
    pub conn_id: String,
    pub database: String,
    /// PG schema (default `public`); ignored elsewhere.
    #[serde(default)]
    pub schema: Option<String>,
    pub table: String,
    /// Also write INSERT statements for every row (tables only; a view exports its DDL alone).
    pub data: bool,
}

/// Rows per INSERT statement.
const BATCH: usize = 100;

/// The DDL of one table or view, as statements that each end with `;`. On MongoDB: the mongosh
/// text that creates the collection with its options, validator and indexes.
pub async fn table_ddl(
    state: &AppState,
    conn_id: &str,
    database: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<String, AppError> {
    if connections::get(state, conn_id)?.engine == Engine::Mongo {
        return mongo::dump::collection_ddl(
            &mongo::client::client(state, conn_id).await?,
            database,
            table,
        )
        .await;
    }
    let ddl = match connections::pool_for(state, conn_id, Some(database))? {
        DbPool::Pg(p) => pg_ddl(&p, schema.unwrap_or("public"), table).await?.ddl,
        DbPool::MySql(p) => mysql_ddl(&p, database, table).await?.ddl,
        DbPool::Sqlite(p) => sqlite_ddl(&p, table).await?.ddl,
    };
    Ok(ddl)
}

/// Writes a header comment, the DDL and (when `req.data`) the rows to a temp file next to
/// `path`, then renames it over `path`. On failure only the temp file is removed.
pub async fn export_table_sql(
    state: &AppState,
    req: TableExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    if !path.is_absolute() {
        return Err(AppError::invalid("Choose a file to export to"));
    }
    let conn = connections::get(state, &req.conn_id)?;
    if conn.engine == Engine::Mongo {
        return mongo::dump::export_collection(state, &conn, req, path).await;
    }
    let pool = connections::pool_for(state, &conn.id, Some(&req.database))?;
    let tmp = export::temp_path(path);
    let file = tokio::fs::File::create(&tmp).await?;
    let mut out = BufWriter::new(file);
    let res = async {
        let rows = match &pool {
            DbPool::Pg(p) => pg_export(p, &conn, &req, &mut out).await?,
            DbPool::MySql(p) => mysql_export(p, &conn, &req, &mut out).await?,
            DbPool::Sqlite(p) => sqlite_export(p, &conn, &req, &mut out).await?,
        };
        out.flush().await?;
        out.get_ref().sync_all().await?;
        Ok::<_, AppError>(rows)
    }
    .await;
    drop(out);
    let res = match res {
        Ok(rows) => tokio::fs::rename(&tmp, path)
            .await
            .map(|()| rows)
            .map_err(AppError::from),
        Err(e) => Err(e),
    };
    match res {
        Ok(rows) => Ok(ExportResult {
            cancelled: false,
            rows,
            path: path.display().to_string(),
        }),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

/// A resolved table or view and its DDL.
struct Target {
    ddl: String,
    view: bool,
    /// Quoted name the INSERT statements target.
    qualified: String,
}

// ---------------------------------------------------------------------------------------------
// Literals and identifiers (pure, unit tested)

/// `"name"` (SQLite, standard SQL): inner `"` doubled.
pub(crate) fn quote_dq(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `` `name` `` (MySQL): inner backticks doubled.
pub(crate) fn quote_bt(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// `'text'` with `'` doubled: PG (standard_conforming_strings, the default) and SQLite.
fn push_quoted(buf: &mut String, s: &str) {
    buf.push('\'');
    for c in s.chars() {
        if c == '\'' {
            buf.push('\'');
        }
        buf.push(c);
    }
    buf.push('\'');
}

/// MySQL string literal: `'` doubled and backslash escapes for `\`, NUL, LF, CR and ^Z, as
/// mysqldump writes them (the default sql_mode reads `\` as an escape).
fn push_mysql_quoted(buf: &mut String, s: &str) {
    buf.push('\'');
    for c in s.chars() {
        match c {
            '\'' => buf.push_str("''"),
            '\\' => buf.push_str("\\\\"),
            '\0' => buf.push_str("\\0"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\x1a' => buf.push_str("\\Z"),
            c => buf.push(c),
        }
    }
    buf.push('\'');
}

/// `X'DEADBEEF'`: a binary string in MySQL, a blob in SQLite.
fn push_hex(buf: &mut String, b: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    buf.reserve(b.len() * 2 + 3);
    buf.push_str("X'");
    for &x in b {
        buf.push(HEX[usize::from(x >> 4)] as char);
        buf.push(HEX[usize::from(x & 0xf)] as char);
    }
    buf.push('\'');
}

/// PG value read as `::text`: NULL or a quoted literal the column type parses back.
fn push_pg_value(buf: &mut String, v: Option<&str>) {
    match v {
        None => buf.push_str("NULL"),
        Some(s) => push_quoted(buf, s),
    }
}

/// A MySQL text-protocol value by the sqlx type name: numbers raw, binary types and BIT as hex,
/// the rest as a string literal (hex when it is not UTF-8).
fn push_mysql_value(buf: &mut String, type_name: &str, raw: &[u8]) {
    let t = type_name.to_ascii_uppercase();
    let t = t.strip_suffix(" UNSIGNED").unwrap_or(&t);
    match t {
        "BOOLEAN" | "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" | "FLOAT"
        | "DOUBLE" | "DECIMAL" | "NUMERIC"
            if !raw.is_empty()
                && raw.iter().all(|b| {
                    b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')
                }) =>
        {
            // Checked ASCII above.
            buf.push_str(std::str::from_utf8(raw).unwrap_or_default());
        }
        "BIT" | "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB"
        | "GEOMETRY" => push_hex(buf, raw),
        _ => match std::str::from_utf8(raw) {
            Ok(s) => push_mysql_quoted(buf, s),
            Err(_) => push_hex(buf, raw),
        },
    }
}

/// A SQLite value by its storage class. Text that is not UTF-8 is written as
/// `CAST(X'…' AS TEXT)` by the reader so it stays text.
fn push_sqlite_value(buf: &mut String, v: SqliteRaw<'_>) {
    use std::fmt::Write as _;
    match v {
        SqliteRaw::Null => buf.push_str("NULL"),
        SqliteRaw::Int(i) => {
            let _ = write!(buf, "{i}");
        }
        // SQLite stores NaN as NULL and reads 9e999 as infinity. `{:?}` keeps a `.0` or an
        // exponent, so the value reads back as REAL with the same bits.
        SqliteRaw::Real(f) if f.is_nan() => buf.push_str("NULL"),
        SqliteRaw::Real(f) if f.is_infinite() => {
            buf.push_str(if f > 0.0 { "9e999" } else { "-9e999" })
        }
        SqliteRaw::Real(f) => {
            let _ = write!(buf, "{f:?}");
        }
        SqliteRaw::Text(s) => push_quoted(buf, s),
        SqliteRaw::Blob(b) => push_hex(buf, b),
    }
}

/// MySQL/MariaDB `information_schema.COLUMNS.EXTRA` of a generated column (`VIRTUAL GENERATED`,
/// `STORED GENERATED`, MariaDB's older `PERSISTENT`). `DEFAULT_GENERATED` is a default only.
fn mysql_generated(extra: &str) -> bool {
    let e = extra.to_ascii_uppercase();
    e.contains("VIRTUAL") || e.contains("STORED") || e.contains("PERSISTENT")
}

/// One PG column line of `CREATE TABLE`. `name`, `ty` and `collation` come quoted from the server.
fn pg_column_def(c: &PgColumn) -> String {
    let mut s = format!("{} {}", c.name, c.ty);
    if let Some(co) = &c.collation {
        s.push_str(" COLLATE ");
        s.push_str(co);
    }
    match (c.generated.as_str(), c.identity.as_str(), &c.default) {
        ("s", _, Some(e)) => s.push_str(&format!(" GENERATED ALWAYS AS ({e}) STORED")),
        ("v", _, Some(e)) => s.push_str(&format!(" GENERATED ALWAYS AS ({e}) VIRTUAL")),
        (_, "a", _) => s.push_str(" GENERATED ALWAYS AS IDENTITY"),
        (_, "d", _) => s.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
        (_, _, Some(e)) => s.push_str(&format!(" DEFAULT {e}")),
        _ => {}
    }
    if c.not_null {
        s.push_str(" NOT NULL");
    }
    s
}

/// `s` with one trailing `;`.
fn statement(s: &str) -> String {
    let s = s.trim_end();
    if s.ends_with(';') {
        s.to_string()
    } else {
        format!("{s};")
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` for seconds since the Unix epoch (civil-from-days, proleptic
/// Gregorian).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

pub(crate) fn header(conn: &Connection, table: &str) -> String {
    let engine = if conn.version.is_empty() {
        match conn.engine {
            Engine::Pg => "PostgreSQL",
            Engine::Mysql => "MySQL",
            Engine::Sqlite => "SQLite",
            Engine::Mongo => "MongoDB",
        }
    } else {
        conn.version.as_str()
    };
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // A line break in a name would end the comment.
    let one_line = |s: &str| s.replace(['\r', '\n'], " ");
    format!(
        "-- sandb SQL export\n-- Table: {}\n-- Engine: {}\n-- Date: {}\n\n",
        one_line(table),
        one_line(engine),
        format_utc(secs)
    )
}

/// INSERT statements of up to `batch` rows each, written as rows arrive.
pub(crate) struct Inserts<'a, W> {
    out: &'a mut W,
    head: String,
    /// Written after the rows of each statement, before its `;` (an `ON CONFLICT …` clause).
    tail: String,
    batch: usize,
    in_batch: usize,
    rows: u64,
}

impl<'a, W: AsyncWrite + Unpin> Inserts<'a, W> {
    /// `head`: `INSERT INTO t (a, b) VALUES`.
    pub(crate) fn new(out: &'a mut W, head: String) -> Self {
        Self::with(out, head, String::new(), BATCH)
    }

    /// `batch` is at least 1.
    pub(crate) fn with(out: &'a mut W, head: String, tail: String, batch: usize) -> Self {
        Self {
            out,
            head,
            tail,
            batch: batch.max(1),
            in_batch: 0,
            rows: 0,
        }
    }

    /// `tuple`: `(v1, v2)`.
    pub(crate) async fn row(&mut self, tuple: &str) -> Result<(), AppError> {
        if self.in_batch == 0 {
            self.out.write_all(self.head.as_bytes()).await?;
            self.out.write_all(b"\n").await?;
        } else {
            self.out.write_all(b",\n").await?;
        }
        self.out.write_all(tuple.as_bytes()).await?;
        self.in_batch += 1;
        self.rows += 1;
        if self.in_batch == self.batch {
            self.end().await?;
        }
        Ok(())
    }

    async fn end(&mut self) -> Result<(), AppError> {
        self.out.write_all(self.tail.as_bytes()).await?;
        self.out.write_all(b";\n").await?;
        self.in_batch = 0;
        Ok(())
    }

    pub(crate) async fn finish(mut self) -> Result<u64, AppError> {
        if self.in_batch > 0 {
            self.end().await?;
        }
        Ok(self.rows)
    }
}

async fn write_ddl(
    out: &mut (impl AsyncWrite + Unpin),
    header: &str,
    ddl: &str,
) -> Result<(), AppError> {
    out.write_all(header.as_bytes()).await?;
    out.write_all(ddl.as_bytes()).await?;
    out.write_all(b"\n").await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// PostgreSQL

struct PgColumn {
    name: String,
    ty: String,
    collation: Option<String>,
    not_null: bool,
    /// `pg_attribute.attidentity`: `a` always, `d` by default, empty.
    identity: String,
    /// `pg_attribute.attgenerated`: `s` stored, `v` virtual (PG 18), empty.
    generated: String,
    /// Default or generation expression.
    default: Option<String>,
}

type PgColumnRow = (
    String,
    String,
    Option<String>,
    bool,
    String,
    String,
    Option<String>,
);

async fn pg_columns(p: &PgPool, rel: &str) -> Result<Vec<PgColumn>, AppError> {
    let rows: Vec<PgColumnRow> = sqlx::query_as(
        "SELECT quote_ident(a.attname), format_type(a.atttypid, a.atttypmod), \
           CASE WHEN a.attcollation <> 0 AND a.attcollation <> t.typcollation \
             THEN format('%I.%I', cn.nspname, co.collname) END, \
           a.attnotnull, a.attidentity::text, a.attgenerated::text, \
           pg_get_expr(d.adbin, d.adrelid) \
         FROM pg_attribute a \
         JOIN pg_type t ON t.oid = a.atttypid \
         LEFT JOIN pg_collation co ON co.oid = a.attcollation \
         LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace \
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE a.attrelid = $1::regclass AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
    )
    .bind(rel)
    .fetch_all(p)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(name, ty, collation, not_null, identity, generated, default)| PgColumn {
                name,
                ty,
                collation,
                not_null,
                identity,
                generated,
                default,
            },
        )
        .collect())
}

async fn pg_indexes(p: &PgPool, rel: &str) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT pg_get_indexdef(i.indexrelid) || ';' \
         FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid \
         WHERE i.indrelid = $1::regclass AND NOT EXISTS ( \
           SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid \
             AND c.conrelid = i.indrelid AND c.contype IN ('p', 'u', 'x')) \
         ORDER BY ic.relname",
    )
    .bind(rel)
    .fetch_all(p)
    .await?)
}

async fn pg_ddl(p: &PgPool, schema: &str, table: &str) -> Result<Target, AppError> {
    let found: Option<(String, String, String)> = sqlx::query_as(
        "SELECT c.relkind::text, c.relpersistence::text, format('%I.%I', n.nspname, c.relname) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p', 'f', 'v', 'm')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(p)
    .await?;
    let Some((kind, persistence, rel)) = found else {
        return Err(not_found_table(table));
    };
    if kind == "v" || kind == "m" {
        let def: String = sqlx::query_scalar("SELECT pg_get_viewdef($1::regclass, true)")
            .bind(&rel)
            .fetch_one(p)
            .await?;
        let def = def.trim_end().trim_end_matches(';');
        let mut ddl = if kind == "v" {
            format!("CREATE OR REPLACE VIEW {rel} AS\n{def};")
        } else {
            format!("CREATE MATERIALIZED VIEW {rel} AS\n{def};")
        };
        for ix in pg_indexes(p, &rel).await? {
            ddl.push('\n');
            ddl.push_str(&ix);
        }
        return Ok(Target {
            ddl,
            view: true,
            qualified: rel,
        });
    }

    // Sequences behind serial columns (pg_depend 'a'); identity sequences ('i') come with
    // the column's GENERATED … AS IDENTITY.
    let sequences: Vec<String> = sqlx::query_scalar(
        "SELECT format('CREATE SEQUENCE IF NOT EXISTS %I.%I AS %s INCREMENT BY %s MINVALUE %s \
           MAXVALUE %s START WITH %s CACHE %s%s;', sn.nspname, s.relname, \
           format_type(q.seqtypid, NULL), q.seqincrement, q.seqmin, q.seqmax, q.seqstart, \
           q.seqcache, CASE WHEN q.seqcycle THEN ' CYCLE' ELSE '' END) \
         FROM pg_depend d \
         JOIN pg_class s ON s.oid = d.objid AND s.relkind = 'S' \
         JOIN pg_namespace sn ON sn.oid = s.relnamespace \
         JOIN pg_sequence q ON q.seqrelid = s.oid \
         WHERE d.classid = 'pg_class'::regclass AND d.refclassid = 'pg_class'::regclass \
           AND d.refobjid = $1::regclass AND d.deptype = 'a' \
         ORDER BY s.relname",
    )
    .bind(&rel)
    .fetch_all(p)
    .await?;
    let columns = pg_columns(p, &rel).await?;
    let constraints: Vec<String> = sqlx::query_scalar(
        "SELECT format('CONSTRAINT %I %s', conname, pg_get_constraintdef(oid, true)) \
         FROM pg_constraint WHERE conrelid = $1::regclass AND contype IN ('p', 'u', 'c', 'f', 'x') \
         ORDER BY position(contype::text IN 'pucfx'), conname",
    )
    .bind(&rel)
    .fetch_all(p)
    .await?;
    // Partition key, foreign server, and the ATTACH of a partition to its parent.
    let (partition_by, server, attach): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT pg_get_partkeydef(c.oid), \
               (SELECT format(' SERVER %I', s.srvname) || coalesce(' OPTIONS (' || ( \
                  SELECT string_agg(format('%I %L', split_part(o, '=', 1), \
                    substr(o, strpos(o, '=') + 1)), ', ') FROM unnest(ft.ftoptions) o) || ')', '') \
                FROM pg_foreign_table ft JOIN pg_foreign_server s ON s.oid = ft.ftserver \
                WHERE ft.ftrelid = c.oid), \
               (SELECT format('ALTER TABLE %I.%I ATTACH PARTITION %s %s;', pn.nspname, \
                  pc.relname, $1::text, pg_get_expr(c.relpartbound, c.oid)) \
                FROM pg_inherits i JOIN pg_class pc ON pc.oid = i.inhparent \
                JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
                WHERE i.inhrelid = c.oid AND c.relispartition) \
             FROM pg_class c WHERE c.oid = $1::regclass",
        )
        .bind(&rel)
        .fetch_one(p)
        .await?;
    let indexes = pg_indexes(p, &rel).await?;

    let mut ddl = String::new();
    for s in &sequences {
        ddl.push_str(s);
        ddl.push('\n');
    }
    let create = match (kind.as_str(), persistence.as_str()) {
        ("f", _) => "CREATE FOREIGN TABLE",
        (_, "u") => "CREATE UNLOGGED TABLE",
        _ => "CREATE TABLE",
    };
    let body: Vec<String> = columns
        .iter()
        .map(pg_column_def)
        .chain(constraints)
        .collect();
    ddl.push_str(&format!("{create} {rel} ("));
    if !body.is_empty() {
        ddl.push_str("\n    ");
        ddl.push_str(&body.join(",\n    "));
        ddl.push('\n');
    }
    ddl.push(')');
    if let Some(k) = partition_by {
        ddl.push_str(" PARTITION BY ");
        ddl.push_str(&k);
    }
    if let Some(s) = server {
        ddl.push_str(&s);
    }
    ddl.push(';');
    for s in attach.iter().chain(&indexes) {
        ddl.push('\n');
        ddl.push_str(s);
    }
    Ok(Target {
        ddl,
        view: false,
        qualified: rel,
    })
}

async fn pg_export<W: AsyncWrite + Unpin>(
    p: &PgPool,
    conn: &Connection,
    req: &TableExportRequest,
    out: &mut W,
) -> Result<u64, AppError> {
    let schema = req.schema.as_deref().unwrap_or("public");
    let t = pg_ddl(p, schema, &req.table).await?;
    write_ddl(
        out,
        &header(conn, &format!("{schema}.{}", req.table)),
        &t.ddl,
    )
    .await?;
    if !req.data || t.view {
        return Ok(0);
    }
    let cols: Vec<PgColumn> = pg_columns(p, &t.qualified)
        .await?
        .into_iter()
        .filter(|c| c.generated.is_empty())
        .collect();
    if cols.is_empty() {
        return Ok(0);
    }
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    let select = format!(
        "SELECT {} FROM {}",
        names
            .iter()
            .map(|n| format!("{n}::text"))
            .collect::<Vec<_>>()
            .join(", "),
        t.qualified
    );
    let overriding = if cols.iter().any(|c| c.identity == "a") {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    out.write_all(b"\n").await?;
    let mut ins = Inserts::new(
        out,
        format!(
            "INSERT INTO {} ({}){overriding} VALUES",
            t.qualified,
            names.join(", ")
        ),
    );
    let mut c = p.acquire().await?;
    let mut rows = sqlx::raw_sql(&select).fetch(&mut *c);
    let mut buf = String::new();
    while let Some(row) = rows.try_next().await? {
        buf.clear();
        buf.push('(');
        for i in 0..row.len() {
            if i > 0 {
                buf.push_str(", ");
            }
            push_pg_value(&mut buf, row.try_get::<Option<&str>, _>(i)?);
        }
        buf.push(')');
        ins.row(&buf).await?;
    }
    ins.finish().await
}

/// One cell of a `raw_sql` row as a literal. The simple query protocol sends every value as
/// its text, which a quoted literal gives back to the column type.
pub(crate) fn push_pg_cell(buf: &mut String, row: &PgRow, i: usize) -> Result<(), AppError> {
    let v = row.try_get_raw(i)?;
    if v.is_null() {
        push_pg_value(buf, None);
        return Ok(());
    }
    let s = v
        .as_str()
        .map_err(|e| AppError::internal("Could not read a value").with_detail(e.to_string()))?;
    push_pg_value(buf, Some(s));
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// MySQL / MariaDB

/// Catalog values as raw bytes: MySQL 8 reports some as binary strings.
fn mysql_text(row: &MySqlRow, i: usize) -> Option<String> {
    let v = row.try_get_raw(i).ok()?;
    if v.is_null() {
        return None;
    }
    <&[u8] as Decode<MySql>>::decode(v)
        .ok()
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

async fn mysql_ddl(p: &MySqlPool, database: &str, table: &str) -> Result<Target, AppError> {
    let found = sqlx::query(
        "SELECT TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
    )
    .bind(database)
    .bind(table)
    .fetch_optional(p)
    .await?;
    let Some(found) = found else {
        return Err(not_found_table(table));
    };
    let ty = mysql_text(&found, 0).unwrap_or_default();
    let view = ty.eq_ignore_ascii_case("VIEW") || ty.eq_ignore_ascii_case("SYSTEM VIEW");
    // Second column: `Create Table`, or `Create View` for a view.
    let sql = format!(
        "SHOW CREATE TABLE {}.{}",
        quote_bt(database),
        quote_bt(table)
    );
    let row = sqlx::raw_sql(&sql).fetch_one(p).await?;
    let ddl = mysql_text(&row, 1).ok_or_else(|| not_found_table(table))?;
    Ok(Target {
        ddl: statement(&ddl),
        view,
        qualified: quote_bt(table),
    })
}

async fn mysql_export<W: AsyncWrite + Unpin>(
    p: &MySqlPool,
    conn: &Connection,
    req: &TableExportRequest,
    out: &mut W,
) -> Result<u64, AppError> {
    let t = mysql_ddl(p, &req.database, &req.table).await?;
    write_ddl(
        out,
        &header(conn, &format!("{}.{}", req.database, req.table)),
        &t.ddl,
    )
    .await?;
    if !req.data || t.view {
        return Ok(0);
    }
    let cols = sqlx::query(
        "SELECT COLUMN_NAME, EXTRA FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
    )
    .bind(&req.database)
    .bind(&req.table)
    .fetch_all(p)
    .await?;
    let names: Vec<String> = cols
        .iter()
        .filter(|r| !mysql_generated(&mysql_text(r, 1).unwrap_or_default()))
        .map(|r| quote_bt(&mysql_text(r, 0).unwrap_or_default()))
        .collect();
    if names.is_empty() {
        return Ok(0);
    }
    let list = names.join(", ");
    let select = format!(
        "SELECT {list} FROM {}.{}",
        quote_bt(&req.database),
        t.qualified
    );
    out.write_all(b"\n").await?;
    let mut ins = Inserts::new(out, format!("INSERT INTO {} ({list}) VALUES", t.qualified));
    let mut c = p.acquire().await?;
    // Text protocol: every value arrives as its text (or raw bytes for binary types).
    let mut rows = sqlx::raw_sql(&select).fetch(&mut *c);
    let mut buf = String::new();
    while let Some(row) = rows.try_next().await? {
        buf.clear();
        buf.push('(');
        for i in 0..row.len() {
            if i > 0 {
                buf.push_str(", ");
            }
            push_mysql_cell(&mut buf, &row, i)?;
        }
        buf.push(')');
        ins.row(&buf).await?;
    }
    ins.finish().await
}

/// One cell of a text-protocol MySQL row as a literal.
pub(crate) fn push_mysql_cell(buf: &mut String, row: &MySqlRow, i: usize) -> Result<(), AppError> {
    let v = row.try_get_raw(i)?;
    if v.is_null() {
        buf.push_str("NULL");
        return Ok(());
    }
    let ty = v.type_info().name().to_string();
    let raw = <&[u8] as Decode<MySql>>::decode(v)
        .map_err(|e| AppError::internal("Could not read a value").with_detail(e.to_string()))?;
    push_mysql_value(buf, &ty, raw);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// SQLite

async fn sqlite_ddl(p: &SqlitePool, table: &str) -> Result<Target, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT type, sql FROM sqlite_master WHERE tbl_name = ? AND sql IS NOT NULL \
         ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 WHEN 'index' THEN 1 ELSE 2 END, \
           name",
    )
    .bind(table)
    .fetch_all(p)
    .await?;
    let view = match rows.first() {
        Some((t, _)) if t == "table" => false,
        Some((t, _)) if t == "view" => true,
        _ => return Err(not_found_table(table)),
    };
    let ddl = rows
        .iter()
        .map(|(_, sql)| statement(sql))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Target {
        ddl,
        view,
        qualified: quote_dq(table),
    })
}

async fn sqlite_export<W: AsyncWrite + Unpin>(
    p: &SqlitePool,
    conn: &Connection,
    req: &TableExportRequest,
    out: &mut W,
) -> Result<u64, AppError> {
    let t = sqlite_ddl(p, &req.table).await?;
    write_ddl(out, &header(conn, &req.table), &t.ddl).await?;
    if !req.data || t.view {
        return Ok(0);
    }
    // `table_info` leaves out generated (hidden) columns.
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
            .bind(&req.table)
            .fetch_all(p)
            .await?;
    if names.is_empty() {
        return Ok(0);
    }
    let list = names
        .iter()
        .map(|n| quote_dq(n))
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!("SELECT {list} FROM {}", t.qualified);
    out.write_all(b"\n").await?;
    let mut ins = Inserts::new(out, format!("INSERT INTO {} ({list}) VALUES", t.qualified));
    let mut c = p.acquire().await?;
    let mut rows = sqlx::raw_sql(&select).fetch(&mut *c);
    let mut buf = String::new();
    while let Some(row) = rows.try_next().await? {
        buf.clear();
        buf.push('(');
        for i in 0..row.len() {
            if i > 0 {
                buf.push_str(", ");
            }
            push_sqlite_cell(&mut buf, &row, i)?;
        }
        buf.push(')');
        ins.row(&buf).await?;
    }
    ins.finish().await
}

pub(crate) fn push_sqlite_cell(
    buf: &mut String,
    row: &sqlx::sqlite::SqliteRow,
    i: usize,
) -> Result<(), AppError> {
    let v = row.try_get_raw(i)?;
    if v.is_null() {
        push_sqlite_value(buf, SqliteRaw::Null);
        return Ok(());
    }
    let bad = |e: sqlx::error::BoxDynError| {
        AppError::internal("Could not read a value").with_detail(e.to_string())
    };
    // A value's own type is its storage class (INTEGER, REAL, TEXT, BLOB).
    let storage = v.type_info().name().to_string();
    match storage.as_str() {
        "INTEGER" | "BOOLEAN" => push_sqlite_value(
            buf,
            SqliteRaw::Int(<i64 as Decode<Sqlite>>::decode(v).map_err(bad)?),
        ),
        "REAL" => push_sqlite_value(
            buf,
            SqliteRaw::Real(<f64 as Decode<Sqlite>>::decode(v).map_err(bad)?),
        ),
        "BLOB" => push_sqlite_value(
            buf,
            SqliteRaw::Blob(<&[u8] as Decode<Sqlite>>::decode(v).map_err(bad)?),
        ),
        _ => match <&str as Decode<Sqlite>>::decode(v) {
            Ok(s) => push_sqlite_value(buf, SqliteRaw::Text(s)),
            // Text that is not UTF-8: its bytes, kept as text.
            Err(_) => {
                let b = <&[u8] as Decode<Sqlite>>::decode(row.try_get_raw(i)?).map_err(bad)?;
                buf.push_str("CAST(");
                push_hex(buf, b);
                buf.push_str(" AS TEXT)");
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{ConnectionInput, Env, SslMode};
    use crate::core::error::ErrorCode;
    use crate::core::secrets::MemoryStore;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
    use sqlx::{Connection as _, Executor};

    fn lit(f: impl FnOnce(&mut String)) -> String {
        let mut s = String::new();
        f(&mut s);
        s
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_dq("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_dq("plain"), "\"plain\"");
        assert_eq!(quote_bt("a`b"), "`a``b`");
    }

    #[test]
    fn pg_literals() {
        assert_eq!(lit(|b| push_pg_value(b, None)), "NULL");
        assert_eq!(lit(|b| push_pg_value(b, Some("it's"))), "'it''s'");
        // standard_conforming_strings: backslashes are literal.
        assert_eq!(lit(|b| push_pg_value(b, Some("a\\b"))), "'a\\b'");
        assert_eq!(lit(|b| push_pg_value(b, Some("\\xdead"))), "'\\xdead'");
        assert_eq!(lit(|b| push_pg_value(b, Some(""))), "''");
        assert_eq!(lit(|b| push_pg_value(b, Some("日本 🚀"))), "'日本 🚀'");
    }

    #[test]
    fn mysql_literals() {
        assert_eq!(
            lit(|b| push_mysql_quoted(b, "it's a\\b\n\r\0\x1a")),
            "'it''s a\\\\b\\n\\r\\0\\Z'"
        );
        assert_eq!(lit(|b| push_mysql_value(b, "INT", b"-42")), "-42");
        assert_eq!(
            lit(|b| push_mysql_value(b, "BIGINT UNSIGNED", b"18446744073709551615")),
            "18446744073709551615"
        );
        assert_eq!(lit(|b| push_mysql_value(b, "BOOLEAN", b"1")), "1");
        assert_eq!(lit(|b| push_mysql_value(b, "DOUBLE", b"1.5e-7")), "1.5e-7");
        assert_eq!(lit(|b| push_mysql_value(b, "DECIMAL", b"12.30")), "12.30");
        // Anything odd in a numeric column is quoted rather than written raw.
        assert_eq!(lit(|b| push_mysql_value(b, "INT", b"1; x")), "'1; x'");
        assert_eq!(
            lit(|b| push_mysql_value(b, "BLOB", &[0xde, 0xad, 0x00])),
            "X'DEAD00'"
        );
        assert_eq!(lit(|b| push_mysql_value(b, "VARBINARY", b"")), "X''");
        assert_eq!(lit(|b| push_mysql_value(b, "BIT", &[5])), "X'05'");
        assert_eq!(lit(|b| push_mysql_value(b, "VARCHAR", b"o'k")), "'o''k'");
        assert_eq!(
            lit(|b| push_mysql_value(b, "JSON", br#"{"a": "x\"y"}"#)),
            r#"'{"a": "x\\"y"}'"#
        );
        assert_eq!(
            lit(|b| push_mysql_value(b, "DATETIME", b"2024-01-02 03:04:05")),
            "'2024-01-02 03:04:05'"
        );
        assert_eq!(lit(|b| push_mysql_value(b, "TEXT", &[0xff])), "X'FF'");
    }

    #[test]
    fn mysql_generated_columns() {
        assert!(mysql_generated("VIRTUAL GENERATED"));
        assert!(mysql_generated("STORED GENERATED"));
        assert!(mysql_generated("PERSISTENT"));
        assert!(!mysql_generated("DEFAULT_GENERATED"));
        assert!(!mysql_generated(
            "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"
        ));
        assert!(!mysql_generated("auto_increment"));
        assert!(!mysql_generated(""));
    }

    #[test]
    fn sqlite_literals() {
        assert_eq!(lit(|b| push_sqlite_value(b, SqliteRaw::Null)), "NULL");
        assert_eq!(lit(|b| push_sqlite_value(b, SqliteRaw::Int(-7))), "-7");
        assert_eq!(lit(|b| push_sqlite_value(b, SqliteRaw::Real(1.0))), "1.0");
        assert_eq!(lit(|b| push_sqlite_value(b, SqliteRaw::Real(0.1))), "0.1");
        assert_eq!(
            lit(|b| push_sqlite_value(b, SqliteRaw::Real(1e300))),
            "1e300"
        );
        assert_eq!(
            lit(|b| push_sqlite_value(b, SqliteRaw::Real(f64::INFINITY))),
            "9e999"
        );
        assert_eq!(
            lit(|b| push_sqlite_value(b, SqliteRaw::Real(f64::NAN))),
            "NULL"
        );
        assert_eq!(
            lit(|b| push_sqlite_value(b, SqliteRaw::Text("a'b\\c"))),
            "'a''b\\c'"
        );
        assert_eq!(
            lit(|b| push_sqlite_value(b, SqliteRaw::Blob(&[1, 0xab]))),
            "X'01AB'"
        );
    }

    #[test]
    fn pg_column_lines() {
        let col = |identity: &str, generated: &str, default: Option<&str>| PgColumn {
            name: "\"Id\"".into(),
            ty: "bigint".into(),
            collation: None,
            not_null: true,
            identity: identity.into(),
            generated: generated.into(),
            default: default.map(Into::into),
        };
        assert_eq!(
            pg_column_def(&col("a", "", None)),
            "\"Id\" bigint GENERATED ALWAYS AS IDENTITY NOT NULL"
        );
        assert_eq!(
            pg_column_def(&col("d", "", None)),
            "\"Id\" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL"
        );
        assert_eq!(
            pg_column_def(&col("", "s", Some("(a * 2)"))),
            "\"Id\" bigint GENERATED ALWAYS AS ((a * 2)) STORED NOT NULL"
        );
        assert_eq!(
            pg_column_def(&col("", "", Some("nextval('t_id_seq'::regclass)"))),
            "\"Id\" bigint DEFAULT nextval('t_id_seq'::regclass) NOT NULL"
        );
        let mut c = col("", "", None);
        c.not_null = false;
        c.ty = "text".into();
        c.collation = Some("pg_catalog.\"C\"".into());
        assert_eq!(pg_column_def(&c), "\"Id\" text COLLATE pg_catalog.\"C\"");
    }

    #[test]
    fn utc_dates() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(951_868_800), "2000-03-01 00:00:00 UTC");
        assert_eq!(format_utc(1_709_251_199), "2024-02-29 23:59:59 UTC");
        assert_eq!(format_utc(4_102_444_800), "2100-01-01 00:00:00 UTC");
    }

    #[test]
    fn statements_end_with_one_semicolon() {
        assert_eq!(statement("CREATE TABLE t (a)"), "CREATE TABLE t (a);");
        assert_eq!(statement("CREATE TABLE t (a);\n"), "CREATE TABLE t (a);");
    }

    #[tokio::test]
    async fn inserts_are_batched() {
        let mut out: Vec<u8> = Vec::new();
        let mut ins = Inserts::new(&mut out, "INSERT INTO t (a) VALUES".into());
        for i in 0..(BATCH + 2) {
            ins.row(&format!("({i})")).await.unwrap();
        }
        assert_eq!(ins.finish().await.unwrap(), (BATCH + 2) as u64);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("INSERT INTO t (a) VALUES\n").count(), 2);
        assert!(s.contains("(98),\n(99);\nINSERT INTO t (a) VALUES\n(100),\n(101);\n"));
        assert!(s.starts_with("INSERT INTO t (a) VALUES\n(0),\n(1),\n"));
    }

    fn input(file: &str) -> ConnectionInput {
        ConnectionInput {
            name: "t".into(),
            engine: Engine::Sqlite,
            env: Env::Local,
            host: String::new(),
            port: 0,
            database: "main".into(),
            user: String::new(),
            ssl: SslMode::Disable,
            file: file.into(),
            ai_write: false,
            read_only: true,
            version: None,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    const SETUP: &str = "\
        CREATE TABLE \"it's\" (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, \
          data BLOB, twice INTEGER GENERATED ALWAYS AS (id * 2) STORED); \
        CREATE INDEX its_name ON \"it's\" (name); \
        CREATE TRIGGER its_trg AFTER INSERT ON \"it's\" BEGIN SELECT 1; END; \
        CREATE VIEW v AS SELECT id, name FROM \"it's\"; \
        WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 150) \
        INSERT INTO \"it's\" (id, name, score, data) SELECT i, 'n' || i, i / 4.0, NULL FROM n; \
        UPDATE \"it's\" SET name = 'o''k \\ \"q\"', score = NULL, data = X'00FF' WHERE id = 1;";

    #[tokio::test]
    async fn sqlite_ddl_and_export_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        let file = dir.path().join("src.db");
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&file)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        (&mut c).execute(sqlx::raw_sql(SETUP)).await.unwrap();
        c.close().await.unwrap();
        let conn = connections::create(&st, input(&file.to_string_lossy()), None).unwrap();

        let ddl = table_ddl(&st, &conn.id, "main", None, "it's")
            .await
            .unwrap();
        let lines: Vec<&str> = ddl.lines().collect();
        assert!(lines[0].starts_with("CREATE TABLE \"it's\" (id INTEGER PRIMARY KEY"));
        assert_eq!(lines[1], "CREATE INDEX its_name ON \"it's\" (name);");
        assert!(lines[2].starts_with("CREATE TRIGGER its_trg"));
        assert!(ddl.ends_with("END;"));
        assert_eq!(
            table_ddl(&st, &conn.id, "main", None, "v").await.unwrap(),
            "CREATE VIEW v AS SELECT id, name FROM \"it's\";"
        );
        let e = table_ddl(&st, &conn.id, "main", None, "nope")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);

        let path = dir.path().join("out.sql");
        let req = |table: &str, data: bool| TableExportRequest {
            conn_id: conn.id.clone(),
            database: "main".into(),
            schema: None,
            table: table.into(),
            data,
        };
        let r = export_table_sql(&st, req("it's", true), &path)
            .await
            .unwrap();
        assert_eq!(r.rows, 150);
        assert!(!r.cancelled);
        let sql = std::fs::read_to_string(&path).unwrap();
        assert!(sql.starts_with("-- sandb SQL export\n-- Table: it's\n-- Engine: SQLite"));
        assert!(sql.contains(&ddl));
        let head = "INSERT INTO \"it's\" (\"id\", \"name\", \"score\", \"data\") VALUES\n";
        assert_eq!(sql.matches(head).count(), 2);
        assert!(sql.contains(&format!(
            "{head}(1, 'o''k \\ \"q\"', NULL, X'00FF'),\n(2, 'n2', 0.5, NULL),"
        )));
        assert!(sql.ends_with("(150, 'n150', 37.5, NULL);\n"));
        // Only the final file remains: the temp file was renamed.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(names.is_empty(), "{names:?}");

        // The file rebuilds the same table and rows.
        let mut fresh = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        (&mut fresh).execute(sqlx::raw_sql(&sql)).await.unwrap();
        let q = "SELECT id, name, score, hex(data), twice FROM \"it's\" ORDER BY id";
        type R = (i64, String, Option<f64>, String, i64);
        let got: Vec<R> = sqlx::query_as(q).fetch_all(&mut fresh).await.unwrap();
        let p = connections::pool_for(&st, &conn.id, Some("main")).unwrap();
        let DbPool::Sqlite(p) = p else { unreachable!() };
        let want: Vec<R> = sqlx::query_as(q).fetch_all(&p).await.unwrap();
        assert_eq!(got, want);
        assert_eq!(got[0].4, 2, "generated column recomputed");

        // DDL only, and a view never exports rows.
        let r = export_table_sql(&st, req("it's", false), &path)
            .await
            .unwrap();
        assert_eq!(r.rows, 0);
        let ddl_only = std::fs::read_to_string(&path).unwrap();
        assert!(!ddl_only.contains("INSERT INTO"));
        assert!(ddl_only.ends_with(&format!("{ddl}\n")));
        let r = export_table_sql(&st, req("v", true), &path).await.unwrap();
        assert_eq!(r.rows, 0);
        let sql = std::fs::read_to_string(&path).unwrap();
        assert!(sql.ends_with("CREATE VIEW v AS SELECT id, name FROM \"it's\";\n"));

        // A missing table leaves an existing file untouched and no temp file behind.
        let e = export_table_sql(&st, req("nope", true), &path)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), sql);
        let parts = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")
            })
            .count();
        assert_eq!(parts, 0);
    }
}
