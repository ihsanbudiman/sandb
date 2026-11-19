//! Check Redis grid edits on the server before applying the whole batch.

use std::collections::BTreeMap;
use std::str::from_utf8;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use redis::{Cmd, RedisError, Value};
use serde_json::Value as Cell;

use crate::core::cell::{self, PREVIEW_BYTES};
use crate::core::connections::{Connection, RedisMode, target_database};
use crate::core::engine::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{
    ApplyEditsRequest, ApplyEditsResult, CellValueRequest, EditFailure, EditKind, EditOperation,
    EditableRequest, EditableResult,
};

use super::client::{self, Access};
use super::command::{self, Kind};
use super::conn::{RedisConn, mode};
use super::key::quote_arg;
use super::parse::{self, Statement};
use super::{reply, run};

const UNSUPPORTED: &str = "Only GET, HGETALL, HSCAN, LRANGE, SMEMBERS, SSCAN and ZRANGE … WITHSCORES results can be edited";
const CHANGED_TYPE: &str = "The key changed type";
const GONE: &str = "The row is gone. Reload the result.";
const CONFLICT: &str = "The value changed after you loaded it. Reload and try again.";
const SAVE_UNKNOWN: &str = "The save may have run on the server. Reload before trying again.";
const SAVE: &str = include_str!("edit.lua");

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyType {
    String,
    Hash,
    List,
    Set,
    Zset,
}

impl KeyType {
    fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Hash => "hash",
            Self::List => "list",
            Self::Set => "set",
            Self::Zset => "zset",
        }
    }

    fn pk(self) -> &'static str {
        match self {
            Self::String => "key",
            Self::Hash => "field",
            Self::List => "index",
            Self::Set | Self::Zset => "member",
        }
    }

    fn value(self) -> Option<&'static str> {
        match self {
            Self::String | Self::Hash | Self::List => Some("value"),
            Self::Zset => Some("score"),
            Self::Set => None,
        }
    }
}

struct Target {
    stmt: Statement,
    kind: KeyType,
}

impl Target {
    fn parse(sql: &str) -> Option<Self> {
        let mut statements = parse::parse(sql).ok()?;
        let stmt = statements.pop()?;
        if !statements.is_empty() || command::classify(&stmt) != Kind::Read {
            return None;
        }
        let [name, _, args @ ..] = &stmt.args[..] else {
            return None;
        };
        let is = |cmd: &[u8]| name.eq_ignore_ascii_case(cmd);
        let kind = match args {
            [] if is(b"GET") => KeyType::String,
            [] if is(b"HGETALL") => KeyType::Hash,
            [] if is(b"SMEMBERS") => KeyType::Set,
            [start, stop]
                if is(b"LRANGE")
                    && index(start).is_some_and(|n| n >= 0)
                    && index(stop).is_some() =>
            {
                KeyType::List
            }
            [start, stop, option]
                if is(b"ZRANGE")
                    && index(start).is_some()
                    && index(stop).is_some()
                    && option.eq_ignore_ascii_case(b"WITHSCORES") =>
            {
                KeyType::Zset
            }
            [cursor, options @ ..] if is(b"HSCAN") && scan(cursor, options) => KeyType::Hash,
            [cursor, options @ ..] if is(b"SSCAN") && scan(cursor, options) => KeyType::Set,
            _ => return None,
        };
        Some(Self { stmt, kind })
    }

    fn key(&self) -> &[u8] {
        &self.stmt.args[1]
    }

    async fn matches(&self, handle: &mut RedisConn) -> redis::RedisResult<bool> {
        let value = handle.query(redis::cmd("TYPE").arg(self.key())).await?;
        let kind: String = redis::from_redis_value(value).map_err(RedisError::from)?;
        Ok(kind == self.kind.name())
    }
}

fn index(bytes: &[u8]) -> Option<i64> {
    let digits = bytes.strip_prefix(b"-").unwrap_or(bytes);
    if bytes != b"0"
        && (!matches!(digits.first(), Some(b'1'..=b'9')) || !digits.iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    from_utf8(bytes).ok()?.parse().ok()
}

fn scan(cursor: &[u8], options: &[Vec<u8>]) -> bool {
    cursor.iter().all(u8::is_ascii_digit)
        && from_utf8(cursor).is_ok_and(|s| s.parse::<u64>().is_ok())
        && options.chunks(2).all(|pair| match pair {
            [option, value] => {
                option.eq_ignore_ascii_case(b"MATCH")
                    || (option.eq_ignore_ascii_case(b"COUNT")
                        && index(value).is_some_and(|n| n > 0))
            }
            // HSCAN NOVALUES has no original values to compare.
            _ => false,
        })
}

fn database(conn: &Connection, requested: &str) -> Result<String, AppError> {
    run::database_index(
        &target_database(conn, Some(requested)),
        mode(conn) == RedisMode::Cluster,
    )
}

fn request_error(
    access: &impl Access,
    conn: &Connection,
    db: &str,
    error: RedisError,
    save: bool,
) -> AppError {
    let error = AppError::from(error);
    if matches!(error.code, ErrorCode::ConnectFailed | ErrorCode::Timeout) {
        access.redis().evict(&conn.id, db);
        if save {
            let detail = match &error.detail {
                Some(detail) => format!("{detail}\n{SAVE_UNKNOWN}"),
                None => SAVE_UNKNOWN.to_owned(),
            };
            return error.with_detail(detail);
        }
    }
    error
}

pub async fn editable_result(
    access: &impl Access,
    conn: &Connection,
    req: EditableRequest,
) -> Result<EditableResult, AppError> {
    let Some(target) = Target::parse(&req.sql) else {
        return Ok(EditableResult::not_editable(UNSUPPORTED));
    };
    let db = database(conn, &req.database)?;
    let mut handle = client::connection(access, &conn.id, &db).await?;
    if !target
        .matches(&mut handle)
        .await
        .map_err(|e| request_error(access, conn, &db, e, false))?
    {
        return Ok(EditableResult::not_editable(CHANGED_TYPE));
    }
    Ok(EditableResult {
        table: Some(quote_arg(target.key())),
        schema: None,
        pk: vec![target.kind.pk().to_owned()],
        reason: None,
        transactions: Some(true),
    })
}

fn invalid_operation() -> AppError {
    AppError::invalid("Invalid Redis grid edit")
}

fn index_cell(cell: &Cell) -> Option<i64> {
    match cell {
        Cell::Number(number) => number.as_i64(),
        Cell::String(text) => index(text.as_bytes()),
        _ => None,
    }
    .filter(|n| *n >= 0)
}

fn is_cut(text: &str) -> bool {
    text.len() >= PREVIEW_BYTES && text.ends_with('…')
}

fn identity(cell: &Cell) -> Result<&str, AppError> {
    let Some(text) = cell.as_str().filter(|text| !is_cut(text)) else {
        return Err(AppError::invalid(
            "Binary or truncated Redis row identities cannot be changed",
        ));
    };
    Ok(text)
}

fn text_cell<'a>(cells: &'a BTreeMap<String, Cell>, name: &str) -> Result<&'a str, AppError> {
    cells
        .get(name)
        .and_then(Cell::as_str)
        .ok_or_else(invalid_operation)
}

fn score(text: &str) -> Result<String, AppError> {
    if ["inf", "+inf", "infinity", "+infinity"]
        .iter()
        .any(|s| text.eq_ignore_ascii_case(s))
    {
        return Ok("inf".to_owned());
    }
    if text.eq_ignore_ascii_case("-inf") || text.eq_ignore_ascii_case("-infinity") {
        return Ok("-inf".to_owned());
    }
    if let Ok(n) = text.parse::<f64>()
        && n.is_finite()
        && (n != 0.0
            || !text
                .bytes()
                .take_while(|b| !matches!(b, b'e' | b'E'))
                .any(|b| matches!(b, b'1'..=b'9')))
    {
        return Ok(format!("{n:e}"));
    }
    Err(AppError::invalid(
        "A sorted set score must be a number or infinity",
    ))
}

fn prepare(cmd: &mut Cmd, target: &Target, op: &EditOperation) -> Result<(), AppError> {
    let kind = target.kind;
    let pk = kind.pk();
    let value_col = kind.value();
    let insert = matches!(op.kind, EditKind::Insert);
    let delete = matches!(op.kind, EditKind::Delete);
    if kind == KeyType::String && (insert || delete) {
        return Err(AppError::invalid(
            "A string key has one value. Use Delete key in the tree.",
        ));
    }

    let has_insert_pk = insert && !matches!(kind, KeyType::String | KeyType::List);
    let mutable_pk = matches!(kind, KeyType::Hash | KeyType::Set | KeyType::Zset);
    let is_column = |name: &str| name == pk || value_col == Some(name);
    if op.document.is_some()
        || op.pk.keys().any(|name| name != pk)
        || (insert && !has_insert_pk && !op.pk.is_empty())
        || (delete && !op.set.is_empty())
        || (!delete && op.set.is_empty())
        || op
            .set
            .keys()
            .any(|name| !(value_col == Some(name.as_str()) || mutable_pk && name == pk))
        || (insert && !op.old.is_empty())
        || op
            .old
            .iter()
            .any(|(name, value)| !is_column(name) || value.ty != "string" || value.value.is_none())
    {
        return Err(invalid_operation());
    }

    let old = |name: &str| op.old.get(name).and_then(|v| v.value.as_deref());
    let list_index;
    let src = if insert {
        ""
    } else {
        let cell = op.pk.get(pk).ok_or_else(invalid_operation)?;
        let src = if kind == KeyType::List {
            list_index = index_cell(cell).ok_or_else(invalid_operation)?.to_string();
            &list_index
        } else {
            identity(cell)?
        };
        if (kind == KeyType::String && src.as_bytes() != target.key())
            || old(pk).is_some_and(|original| original != src)
        {
            return Err(invalid_operation());
        }
        src
    };
    let dst = if has_insert_pk || op.set.contains_key(pk) {
        text_cell(&op.set, pk)?
    } else {
        src
    };
    if has_insert_pk && op.pk.get(pk).is_some_and(|cell| cell.as_str() != Some(dst)) {
        return Err(invalid_operation());
    }
    let rename = !insert && !delete && src != dst;
    if !insert && !delete && !rename && value_col.is_none_or(|col| !op.set.contains_key(col)) {
        return Err(invalid_operation());
    }

    let old_score;
    let mut original = if insert {
        ""
    } else if let Some(col) = value_col {
        old(col).ok_or_else(invalid_operation)?
    } else {
        ""
    };
    if !insert && kind == KeyType::Zset {
        old_score = score(original)?;
        original = &old_score;
    }
    let updated_score;
    let value = match value_col {
        Some(col) if !delete && (insert || op.set.contains_key(col)) => {
            let text = text_cell(&op.set, col)?;
            if kind == KeyType::Zset {
                updated_score = score(text)?;
                &updated_score
            } else {
                text
            }
        }
        Some(_) if !delete => original,
        _ => "",
    };
    let action = if insert {
        "i"
    } else if delete {
        "d"
    } else if rename {
        "r"
    } else {
        "u"
    };
    cmd.arg(action).arg(src).arg(dst).arg(original).arg(value);
    Ok(())
}

pub async fn apply_edits(
    access: &impl Access,
    conn: &Connection,
    req: ApplyEditsRequest,
) -> Result<ApplyEditsResult, AppError> {
    let target = Target::parse(&req.sql).ok_or_else(|| AppError::invalid(UNSUPPORTED))?;
    let db = database(conn, &req.database)?;
    let mut cmd = redis::cmd("EVAL");
    cmd.arg(SAVE)
        .arg(1)
        .arg(target.key())
        .arg(target.kind.name());
    if target.kind == KeyType::List
        && req
            .operations
            .iter()
            .any(|op| matches!(op.kind, EditKind::Delete))
    {
        cmd.arg(format!("__sandb_deleted_{}", uuid::Uuid::new_v4()));
    } else {
        cmd.arg("");
    }
    for op in &req.operations {
        prepare(&mut cmd, &target, op)?;
    }
    let saved = ApplyEditsResult {
        failed: None,
        partial: false,
    };
    if req.operations.is_empty() {
        return Ok(saved);
    }

    let active = req
        .query_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| {
            access
                .queries()
                .register(id, &conn.id, &db, Engine::Redis, 1)
        })
        .transpose()?;
    let mut handle = if let Some(active) = &active {
        tokio::select! {
            biased;
            () = active.cancelled.cancelled() => return Err(AppError::cancelled()),
            handle = client::connection(access, &conn.id, &db) => handle?,
        }
    } else {
        client::connection(access, &conn.id, &db).await?
    };
    let result = if let Some(active) = &active {
        let outcome = run::until_cancelled(&active.cancelled, handle.query(&cmd)).await?;
        let Some(outcome) = outcome else {
            access.redis().evict(&conn.id, &db);
            return Err(AppError::cancelled().with_detail(SAVE_UNKNOWN));
        };
        outcome
    } else {
        handle.query(&cmd).await
    }
    .map_err(|e| request_error(access, conn, &db, e, true))?;
    let Value::Array(items) = result else {
        return Err(unexpected_save_reply());
    };
    match items.as_slice() {
        [Value::Int(0)] => Ok(saved),
        [Value::Int(n)] => {
            let index = usize::try_from(*n)
                .ok()
                .and_then(|one_based| one_based.checked_sub(1))
                .filter(|&index| index < req.operations.len())
                .ok_or_else(unexpected_save_reply)?;
            Ok(ApplyEditsResult {
                failed: Some(EditFailure {
                    index,
                    message: CONFLICT.to_owned(),
                }),
                partial: false,
            })
        }
        _ => Err(unexpected_save_reply()),
    }
}

fn unexpected_save_reply() -> AppError {
    AppError::internal("Unexpected Redis save reply").with_detail(SAVE_UNKNOWN)
}

fn cell_bytes(cell: &Cell) -> Option<Vec<u8>> {
    let bytes = STANDARD.decode(cell.get("$bytes")?.as_str()?).ok()?;
    (bytes.len() as u64 == cell.get("len")?.as_u64()?).then_some(bytes)
}

pub async fn cell_value(
    access: &impl Access,
    conn: &Connection,
    req: CellValueRequest,
) -> Result<Option<String>, AppError> {
    let target = Target::parse(&req.sql).ok_or_else(|| AppError::invalid(UNSUPPORTED))?;
    let pk = target.kind.pk();
    let Some(id) = req.pk.get(pk).filter(|_| req.pk.len() == 1) else {
        return Err(AppError::invalid(
            "Expected the original Redis row identity",
        ));
    };
    let index_id;
    let id = if target.kind == KeyType::List {
        index_id = cell::int(index_cell(id).ok_or_else(invalid_operation)?);
        &index_id
    } else {
        if id.as_str().map_or_else(|| cell_bytes(id).is_none(), is_cut) {
            return Err(AppError::invalid(
                "A truncated Redis row identity cannot be used to find the row",
            ));
        }
        id
    };

    let db = database(conn, &req.database)?;
    let mut handle = client::connection(access, &conn.id, &db).await?;
    if !target
        .matches(&mut handle)
        .await
        .map_err(|e| request_error(access, conn, &db, e, false))?
    {
        return Err(AppError::not_found(GONE));
    }
    let value = run::query(&mut handle, &target.stmt).await.map_err(|e| {
        if e.code() == Some("WRONGTYPE") {
            AppError::not_found(GONE)
        } else {
            request_error(access, conn, &db, e, false)
        }
    })?;
    if matches!(value, Value::Nil) {
        return Err(AppError::not_found(GONE));
    }
    let shaped = reply::shape_full(&target.stmt, value);
    let Some(pk_index) = shaped.columns.iter().position(|column| column.name == pk) else {
        return Err(AppError::not_found(GONE));
    };
    let column = shaped
        .columns
        .iter()
        .position(|column| column.name == req.column)
        .ok_or_else(|| AppError::invalid("Column does not exist"))?;
    let mut row = shaped
        .rows
        .into_iter()
        .find(|row| row.get(pk_index) == Some(id))
        .ok_or_else(|| AppError::not_found(GONE))?;
    Ok(match row.swap_remove(column) {
        Cell::Null => None,
        Cell::String(text) => Some(text),
        cell @ Cell::Object(_) => {
            let bytes =
                cell_bytes(&cell).ok_or_else(|| AppError::internal("Invalid Redis byte cell"))?;
            Some(quote_arg(&bytes))
        }
        cell => Some(cell.to_string()),
    })
}
