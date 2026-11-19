//! Turn Redis replies into grid rows without interpreting Redis byte strings as lossy text.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::str::from_utf8;

use redis::Value;
use serde_json::{Map, Value as Cell};

use super::{key::quote_arg, parse::Statement};
use crate::core::cell::{self, PREVIEW_BYTES};
use crate::core::result::ColumnMeta;

pub struct Shaped {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub notice: Option<String>,
}

pub fn shape(stmt: &Statement, value: Value) -> Shaped {
    shape_with_cap(stmt, &value, Some(PREVIEW_BYTES))
}

pub fn shape_full(stmt: &Statement, value: Value) -> Shaped {
    shape_with_cap(stmt, &value, None)
}

fn shape_with_cap(stmt: &Statement, value: &Value, cap: Cap) -> Shaped {
    let value = data(value);
    command_shape(&stmt.args, value, cap).unwrap_or_else(|| fallback(value, cap))
}

type Cap = Option<usize>;

fn data(mut value: &Value) -> &Value {
    while let Value::Attribute { data, .. } = value {
        value = data;
    }
    value
}

fn byte_cell(bytes: &[u8], cap: Cap) -> Cell {
    match from_utf8(bytes) {
        Ok(text) => cell::text_capped(text, cap),
        Err(_) => cell::bytes(bytes, cap),
    }
}

fn text_bytes(value: &Value) -> Option<&[u8]> {
    match data(value) {
        Value::BulkString(bytes) | Value::BigNumber(bytes) => Some(bytes),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => Some(text.as_bytes()),
        Value::Okay => Some(b"OK"),
        _ => None,
    }
}

fn text(value: &Value) -> Option<&str> {
    from_utf8(text_bytes(value)?).ok()
}

fn owned_text(text: String, cap: Cap) -> Cell {
    if cap.is_some_and(|cap| text.len() > cap) {
        cell::text_capped(&text, cap)
    } else {
        Cell::String(text)
    }
}

fn scalar(value: &Value, cap: Cap) -> Cell {
    match data(value) {
        Value::Nil => Cell::Null,
        Value::Int(n) => cell::int(*n),
        Value::Double(n) => cell::float(*n),
        Value::Boolean(value) => Cell::Bool(*value),
        Value::BulkString(bytes) | Value::BigNumber(bytes) => byte_cell(bytes, cap),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => {
            cell::text_capped(text, cap)
        }
        Value::Okay => cell::text("OK"),
        Value::ServerError(error) => match error.details() {
            Some(detail) => owned_text(format!("{} {detail}", error.code()), cap),
            None => cell::text_capped(error.code(), cap),
        },
        _ => cell::unsupported("Redis reply"),
    }
}

fn nested(value: &Value) -> bool {
    matches!(
        data(value),
        Value::Array(_) | Value::Map(_) | Value::Set(_) | Value::Push { .. }
    )
}

fn value_cell(value: &Value, cap: Cap) -> Cell {
    if nested(value) {
        json_cell(value, cap)
    } else {
        scalar(value, cap)
    }
}

fn json_cell(value: &Value, cap: Cap) -> Cell {
    json_text(
        if cap.is_none() {
            json(value)
        } else {
            json_with_cap(value, cap)
        },
        cap,
    )
}

fn json_text(value: Cell, cap: Cap) -> Cell {
    owned_text(value.to_string(), cap)
}

/// Convert a Redis value to JSON without replacing invalid UTF-8 bytes.
pub fn json(value: &Value) -> Cell {
    json_with_cap(value, None)
}

fn json_with_cap(value: &Value, cap: Cap) -> Cell {
    match data(value) {
        Value::Array(values) | Value::Set(values) => {
            Cell::Array(values.iter().map(|v| json_with_cap(v, cap)).collect())
        }
        Value::Map(pairs) => object(pairs.iter().map(|(key, value)| (key, value)), cap),
        Value::Push { kind, data } => serde_json::json!({
            "kind": kind.to_string(),
            "data": data.iter().map(|v| json_with_cap(v, cap)).collect::<Vec<_>>(),
        }),
        _ => scalar(value, cap),
    }
}

fn object<'a>(pairs: impl Iterator<Item = (&'a Value, &'a Value)>, cap: Cap) -> Cell {
    let mut fields = Map::new();
    for (key, value) in pairs {
        let name = text_bytes(key).map_or_else(|| json(key).to_string(), display_name);
        let name = unique_name(name, 1, |name| fields.contains_key(name));
        fields.insert(name, json_with_cap(value, cap));
    }
    Cell::Object(fields)
}

fn display_name(bytes: &[u8]) -> String {
    match from_utf8(bytes) {
        Ok(name) => name.to_owned(),
        Err(_) => quote_arg(bytes),
    }
}

fn unique_name(name: String, first: usize, mut exists: impl FnMut(&str) -> bool) -> String {
    if first == 1 && !exists(&name) {
        return name;
    }
    for n in first.max(2).. {
        let candidate = format!("{name} ({n})");
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn column(name: impl Into<String>, data_type: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        data_type: data_type.to_owned(),
        mixed: false,
    }
}

fn shaped(columns: Vec<ColumnMeta>, rows: Vec<Vec<Cell>>) -> Shaped {
    Shaped {
        columns,
        rows,
        notice: None,
    }
}

fn table<const N: usize>(columns: [(&str, &str); N], rows: Vec<Vec<Cell>>) -> Shaped {
    shaped(columns.map(|(name, ty)| column(name, ty)).into(), rows)
}

fn command_shape(args: &[Vec<u8>], value: &Value, cap: Cap) -> Option<Shaped> {
    let command = args.first()?.as_slice();
    let command = if command.iter().any(u8::is_ascii_lowercase) {
        Cow::Owned(command.to_ascii_uppercase())
    } else {
        Cow::Borrowed(command)
    };
    let columns = |name| [(name, "string"), ("value", "string")];
    Some(match command.as_ref() {
        b"GET" | b"GETDEL" | b"GETEX" | b"GETSET" | b"GETRANGE" | b"SUBSTR" => table(
            columns("key"),
            vec![vec![byte_cell(args.get(1)?, cap), value_cell(value, cap)]],
        ),
        b"MGET" | b"HMGET" => {
            let (name, start) = if command.as_ref() == b"MGET" {
                ("key", 1)
            } else {
                ("field", 2)
            };
            let rows = args
                .get(start..)?
                .iter()
                .zip(value.as_sequence()?)
                .map(|(key, value)| vec![byte_cell(key, cap), value_cell(value, cap)]);
            table(columns(name), rows.collect())
        }
        b"HGETALL" => table(columns("field"), pair_rows(value, cap, false)?),
        b"HSCAN" => with_cursor(value, |batch| {
            if hscan_no_values(args) {
                return Some(table(
                    columns("field"),
                    batch
                        .as_sequence()?
                        .iter()
                        .map(|field| vec![value_cell(field, cap), Cell::Null])
                        .collect(),
                ));
            }
            Some(table(columns("field"), pair_rows(batch, cap, false)?))
        })?,
        b"HKEYS" => sequence(value, "field", cap)?,
        b"HVALS" => sequence(value, "value", cap)?,
        b"LRANGE" => {
            let start = from_utf8(args.get(2)?).ok()?.parse::<i64>().ok()?.max(0);
            table(
                [("index", "integer"), ("value", "string")],
                value
                    .as_sequence()?
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        vec![
                            cell::int(start.saturating_add(i as i64)),
                            value_cell(value, cap),
                        ]
                    })
                    .collect(),
            )
        }
        b"SMEMBERS" | b"SINTER" | b"SUNION" | b"SDIFF" | b"SRANDMEMBER" => {
            sequence(value, "member", cap)?
        }
        b"SSCAN" => with_cursor(value, |batch| sequence(batch, "member", cap))?,
        b"ZRANGE" | b"ZRANGEBYSCORE" | b"ZRANGEBYLEX" | b"ZREVRANGE" | b"ZREVRANGEBYSCORE"
        | b"ZREVRANGEBYLEX" => zset(value, cap, with_scores(args, 4))?,
        b"ZRANDMEMBER" => zset(value, cap, with_scores(args, 3))?,
        b"ZINTER" | b"ZUNION" | b"ZDIFF" => {
            let keys = from_utf8(args.get(1)?).ok()?.parse::<usize>().ok()?;
            zset(value, cap, with_scores(args, 2_usize.saturating_add(keys)))?
        }
        b"ZPOPMIN" | b"ZPOPMAX" => zset(value, cap, true)?,
        b"ZSCAN" => with_cursor(value, |batch| zset(batch, cap, true))?,
        b"XRANGE" | b"XREVRANGE" => stream_range(value, cap)?,
        b"XREAD" | b"XREADGROUP" => stream_read(value, cap)?,
        b"KEYS" => sequence(value, "key", cap)?,
        b"SCAN" => with_cursor(value, |batch| sequence(batch, "key", cap))?,
        b"CONFIG" if has_arg(args, 1, b"GET") => table(
            [("parameter", "string"), ("value", "string")],
            pair_rows(value, cap, false)?,
        ),
        b"INFO" => info(text(value)?, cap),
        b"CLIENT" if has_arg(args, 1, b"LIST") => client_list(text(value)?, cap),
        b"SLOWLOG" if has_arg(args, 1, b"GET") => slowlog(value, cap)?,
        _ => return None,
    })
}

fn has_arg(args: &[Vec<u8>], at: usize, want: &[u8]) -> bool {
    args.get(at)
        .is_some_and(|arg| arg.eq_ignore_ascii_case(want))
}

fn hscan_no_values(args: &[Vec<u8>]) -> bool {
    let mut args = args.iter().skip(3);
    while let Some(arg) = args.next() {
        if arg.eq_ignore_ascii_case(b"NOVALUES") {
            return true;
        }
        // MATCH and COUNT consume the next argument, which is not an option.
        args.next();
    }
    false
}

fn with_scores(args: &[Vec<u8>], start: usize) -> bool {
    args.iter()
        .skip(start)
        .any(|arg| arg.eq_ignore_ascii_case(b"WITHSCORES"))
}

fn sequence(value: &Value, name: &str, cap: Cap) -> Option<Shaped> {
    let rows = if let Some(values) = value.as_sequence() {
        values
            .iter()
            .map(|value| vec![value_cell(value, cap)])
            .collect()
    } else if !nested(value) {
        // SRANDMEMBER and ZRANDMEMBER without a count return one member, not an array.
        vec![vec![value_cell(value, cap)]]
    } else {
        return None;
    };
    Some(table([(name, "string")], rows))
}

fn with_cursor(
    value: &Value,
    shape_batch: impl FnOnce(&Value) -> Option<Shaped>,
) -> Option<Shaped> {
    let [cursor, batch] = value.as_sequence()? else {
        return None;
    };
    let cursor = match data(cursor) {
        Value::Int(n) => u64::try_from(*n).ok()?,
        other => text(other)?.parse::<u64>().ok()?,
    };
    let mut reply = shape_batch(data(batch))?;
    if cursor != 0 {
        reply.notice = Some(format!("Next cursor: {cursor}"));
    }
    Some(reply)
}

fn pair(value: &Value) -> Option<(&Value, &Value)> {
    let [key, value] = data(value).as_sequence()? else {
        return None;
    };
    Some((key, value))
}

/// Redis uses maps (HGETALL), flat pairs (HSCAN) and arrays of pairs (ZRANGE, streams).
fn pairs(value: &Value) -> Option<impl Iterator<Item = (&Value, &Value)>> {
    let value = data(value);
    let entries = value
        .as_sequence()
        .filter(|items| items.iter().all(|item| pair(item).is_some()));
    let map = if entries.is_none() {
        Some(value.as_map_iter()?)
    } else {
        None
    };
    Some(
        entries
            .into_iter()
            .flatten()
            .filter_map(pair)
            .chain(map.into_iter().flatten()),
    )
}

fn pair_rows(value: &Value, cap: Cap, scores: bool) -> Option<Vec<Vec<Cell>>> {
    Some(
        pairs(value)?
            .map(|(left, right)| {
                let right = if scores {
                    match data(right) {
                        Value::Int(n) => cell::float(*n as f64),
                        Value::Double(n) => cell::float(*n),
                        value => text(value)
                            .and_then(|text| text.parse::<f64>().ok())
                            .map_or_else(|| value_cell(value, cap), cell::float),
                    }
                } else {
                    value_cell(right, cap)
                };
                vec![value_cell(left, cap), right]
            })
            .collect(),
    )
}

fn zset(value: &Value, cap: Cap, scores: bool) -> Option<Shaped> {
    if scores {
        Some(table(
            [("member", "string"), ("score", "double")],
            pair_rows(value, cap, true)?,
        ))
    } else {
        sequence(value, "member", cap)
    }
}

#[derive(Default)]
struct StreamField {
    columns: Vec<usize>,
    row: usize,
    occurrences: usize,
}

fn stream_range(value: &Value, cap: Cap) -> Option<Shaped> {
    let mut columns = vec![column("id", "string")];
    let mut names = HashSet::from(["id".to_owned()]);
    let mut fields = HashMap::<&[u8], StreamField>::new();
    let mut rows = Vec::new();
    for (row_index, (id, entries)) in pairs(value)?.enumerate() {
        let mut row = vec![value_cell(id, cap)];
        for (field, value) in pairs(entries)? {
            let field = text_bytes(field)?;
            let slot = fields.entry(field).or_default();
            if slot.row != row_index {
                slot.row = row_index;
                slot.occurrences = 0;
            }
            let i = match slot.columns.get(slot.occurrences) {
                Some(&i) => i,
                None => {
                    // The stream ID and repeated field names need independent grid columns.
                    let name = unique_name(display_name(field), slot.occurrences + 1, |name| {
                        names.contains(name)
                    });
                    names.insert(name.clone());
                    let i = columns.len();
                    columns.push(column(name, "string"));
                    slot.columns.push(i);
                    i
                }
            };
            slot.occurrences += 1;
            if i >= row.len() {
                row.resize(i + 1, Cell::Null);
            }
            row[i] = value_cell(value, cap);
        }
        rows.push(row);
    }
    for row in &mut rows {
        row.resize(columns.len(), Cell::Null);
    }
    Some(shaped(columns, rows))
}

fn stream_read(value: &Value, cap: Cap) -> Option<Shaped> {
    let mut rows = Vec::new();
    for (stream, entries) in pairs(value)? {
        for (id, fields) in pairs(entries)? {
            // RESP2 stream fields are alternating keys/values, not a JSON array of values.
            rows.push(vec![
                value_cell(stream, cap),
                value_cell(id, cap),
                json_text(object(pairs(fields)?, cap), cap),
            ]);
        }
    }
    Some(table(
        [("stream", "string"), ("id", "string"), ("fields", "string")],
        rows,
    ))
}

fn info(text: &str, cap: Cap) -> Shaped {
    let mut section = "";
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("# ") {
            section = name;
        } else if let Some((name, value)) = line.split_once(':') {
            rows.push(vec![
                cell::text_capped(section, cap),
                cell::text_capped(name, cap),
                cell::text_capped(value, cap),
            ]);
        }
    }
    table(
        [
            ("section", "string"),
            ("field", "string"),
            ("value", "string"),
        ],
        rows,
    )
}

fn client_fields(line: &str) -> impl Iterator<Item = (&str, &str)> {
    line.split_ascii_whitespace()
        .filter_map(|field| field.split_once('='))
}

fn client_list(text: &str, cap: Cap) -> Shaped {
    let fields = text.lines().next().into_iter().flat_map(client_fields);
    let mut columns = Vec::new();
    let mut index = HashMap::new();
    for (name, _) in fields {
        if let std::collections::hash_map::Entry::Vacant(entry) = index.entry(name) {
            entry.insert(columns.len());
            columns.push(column(name, "string"));
        }
    }
    let rows = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut row = vec![Cell::Null; columns.len()];
            for (name, value) in client_fields(line) {
                if let Some(&i) = index.get(name) {
                    row[i] = cell::text_capped(value, cap);
                }
            }
            row
        })
        .collect();
    shaped(columns, rows)
}

fn slowlog(value: &Value, cap: Cap) -> Option<Shaped> {
    const COLUMNS: [(&str, &str); 6] = [
        ("id", "integer"),
        ("timestamp", "integer"),
        ("duration_us", "integer"),
        ("command", "string"),
        ("client", "string"),
        ("name", "string"),
    ];
    let mut rows = Vec::new();
    for entry in value.as_sequence()? {
        let entry = data(entry).as_sequence()?;
        rows.push(
            (0..COLUMNS.len())
                .map(|i| {
                    entry
                        .get(i)
                        .map_or(Cell::Null, |value| value_cell(value, cap))
                })
                .collect(),
        );
    }
    Some(table(COLUMNS, rows))
}

fn cell_type(value: &Value) -> Option<&'static str> {
    Some(match data(value) {
        Value::Nil => return None,
        Value::Int(_) => "integer",
        Value::Double(_) => "double",
        Value::Boolean(_) => "boolean",
        Value::Array(_) | Value::Map(_) | Value::Set(_) | Value::Push { .. } => "mixed",
        _ => "string",
    })
}

fn merge_type(ty: &mut Option<&'static str>, value: &Value) {
    if let Some(value_type) = cell_type(value) {
        *ty = Some(match *ty {
            None => value_type,
            Some(previous) if previous == value_type => previous,
            Some(_) => "mixed",
        });
    }
}

fn matrix_width(values: &[Value]) -> Option<usize> {
    let Value::Array(first) = data(values.first()?) else {
        return None;
    };
    values
        .iter()
        .all(|row| {
            matches!(data(row), Value::Array(items)
                if items.len() == first.len() && items.iter().all(|v| !nested(v)))
        })
        .then_some(first.len())
}

fn matrix(values: &[Value], width: usize, cap: Cap) -> Shaped {
    let mut types = vec![None; width];
    let mut rows = Vec::with_capacity(values.len());
    for (i, value) in values.iter().enumerate() {
        let Value::Array(items) = data(value) else {
            unreachable!("matrix rows checked above");
        };
        let mut row = Vec::with_capacity(width + 1);
        row.push(cell::int(i as i64 + 1));
        row.extend(items.iter().zip(&mut types).map(|(item, ty)| {
            merge_type(ty, item);
            scalar(item, cap)
        }));
        rows.push(row);
    }
    let columns = std::iter::once(column("#", "integer"))
        .chain(
            types
                .into_iter()
                .enumerate()
                .map(|(i, ty)| column((i + 1).to_string(), ty.unwrap_or("mixed"))),
        )
        .collect();
    shaped(columns, rows)
}

fn fallback(value: &Value, cap: Cap) -> Shaped {
    if let Value::Array(values) = value
        && let Some(width) = matrix_width(values)
    {
        return matrix(values, width, cap);
    }
    match value {
        Value::Array(values) | Value::Set(values) => {
            let mut ty = None;
            let rows = values
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    merge_type(&mut ty, value);
                    vec![cell::int(i as i64 + 1), value_cell(value, cap)]
                })
                .collect();
            table([("#", "integer"), ("value", ty.unwrap_or("mixed"))], rows)
        }
        Value::Map(entries) if entries.iter().all(|(k, v)| !nested(k) && !nested(v)) => {
            let (mut key_type, mut value_type) = (None, None);
            let rows = entries
                .iter()
                .map(|(key, value)| {
                    merge_type(&mut key_type, key);
                    merge_type(&mut value_type, value);
                    vec![scalar(key, cap), scalar(value, cap)]
                })
                .collect();
            table(
                [
                    ("field", key_type.unwrap_or("mixed")),
                    ("value", value_type.unwrap_or("mixed")),
                ],
                rows,
            )
        }
        other if nested(other) => table(
            [("#", "integer"), ("value", "mixed")],
            vec![vec![cell::int(1), json_cell(other, cap)]],
        ),
        other => table(
            [("value", cell_type(other).unwrap_or("mixed"))],
            vec![vec![scalar(other, cap)]],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::redis::parse::parse;
    use serde_json::json;

    fn stmt(text: &str) -> Statement {
        parse(text).unwrap().pop().unwrap()
    }

    fn bulk(bytes: impl AsRef<[u8]>) -> Value {
        Value::BulkString(bytes.as_ref().to_vec())
    }

    fn array(values: impl Into<Vec<Value>>) -> Value {
        Value::Array(values.into())
    }

    fn assert_table(shaped: Shaped, names: &[&str], rows: serde_json::Value) {
        assert_eq!(
            shaped
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            names
        );
        assert_eq!(serde_json::to_value(shaped.rows).unwrap(), rows);
    }

    #[test]
    fn string_replies_keep_keys_and_nulls_and_do_not_decode_binary_lossily() {
        for cmd in ["get", "GETDEL", "GETEX", "GETSET", "GETRANGE", "SUBSTR"] {
            assert_table(
                shape(
                    &stmt(&format!(r#"{cmd} "\xff\x00""#)),
                    bulk(b"\xff\x00\xfe"),
                ),
                &["key", "value"],
                json!([[{"$bytes":"/wA=", "len":2}, {"$bytes":"/wD+", "len":3}]]),
            );
        }
        assert_table(
            shape(
                &stmt("MGET a b a"),
                array([bulk("one"), Value::Nil, bulk("one")]),
            ),
            &["key", "value"],
            json!([["a", "one"], ["b", null], ["a", "one"]]),
        );
        assert_table(
            shape(
                &stmt(r#"HMGET h "display name" missing"#),
                array([bulk("Ada"), Value::Nil]),
            ),
            &["field", "value"],
            json!([["display name", "Ada"], ["missing", null]]),
        );
    }

    #[test]
    fn command_shapes_accept_resp2_and_resp3_hashes_sets_and_scans() {
        let flat = array([bulk("name"), bulk("Ada"), bulk("age"), bulk("37")]);
        let map = Value::Map(vec![(bulk("name"), bulk("Ada")), (bulk("age"), bulk("37"))]);
        for data in [flat, map] {
            assert_table(
                shape(&stmt("HGETALL h"), data.clone()),
                &["field", "value"],
                json!([["name", "Ada"], ["age", "37"]]),
            );
            let scanned = shape(
                &stmt("HSCAN h 0"),
                array([bulk("18446744073709551615"), data]),
            );
            assert_eq!(
                scanned.notice.as_deref(),
                Some("Next cursor: 18446744073709551615")
            );
            assert_table(
                scanned,
                &["field", "value"],
                json!([["name", "Ada"], ["age", "37"]]),
            );
        }
        let flat = array([bulk("maxmemory"), bulk("0")]);
        let map = Value::Map(vec![(bulk("maxmemory"), bulk("0"))]);
        for value in [flat, map] {
            assert_table(
                shape(&stmt("config get maxmemory"), value),
                &["parameter", "value"],
                json!([["maxmemory", "0"]]),
            );
        }
        for (cmd, name) in [
            ("HKEYS h", "field"),
            ("HVALS h", "value"),
            ("SMEMBERS s", "member"),
            ("SINTER s t", "member"),
            ("SUNION s t", "member"),
            ("SDIFF s t", "member"),
            ("SRANDMEMBER s 1", "member"),
            ("KEYS *", "key"),
        ] {
            for value in [array([bulk("x")]), Value::Set(vec![bulk("x")])] {
                assert_table(shape(&stmt(cmd), value), &[name], json!([["x"]]));
            }
        }
        assert_table(
            shape(&stmt("SRANDMEMBER s"), bulk("x")),
            &["member"],
            json!([["x"]]),
        );
        assert_table(
            shape(
                &stmt("HSCAN h 0 MATCH NOVALUES NOVALUES"),
                array([bulk("0"), array([bulk("a"), bulk("b")])]),
            ),
            &["field", "value"],
            json!([["a", null], ["b", null]]),
        );
        let scan = shape(&stmt("SCAN 0"), array([bulk("0"), array([bulk(b"\xff")])]));
        assert_eq!(scan.notice, None);
        assert_table(scan, &["key"], json!([[{"$bytes":"/w==", "len":1}]]));
        let scan = shape(
            &stmt("SSCAN s 0"),
            array([Value::Int(11), Value::Set(vec![bulk("x")])]),
        );
        assert_eq!(scan.notice.as_deref(), Some("Next cursor: 11"));
        assert_table(scan, &["member"], json!([["x"]]));
    }

    #[test]
    fn list_indexes_and_sorted_set_options_shape_the_native_replies() {
        for (start, expected) in [(3, 3), (-2, 0)] {
            let list = shape(
                &stmt(&format!("LRANGE l {start} -1")),
                array([bulk("a"), bulk("b")]),
            );
            assert_eq!(list.columns[0].data_type, "integer");
            assert_table(
                list,
                &["index", "value"],
                json!([[expected, "a"], [expected + 1, "b"]]),
            );
        }
        for cmd in [
            "ZRANGE z 0 5 wItHsCoReS",
            "ZREVRANGE z 0 5 WITHSCORES",
            "ZRANGEBYSCORE z 0 +inf WITHSCORES",
            "ZRANDMEMBER z 2 WITHSCORES",
            "ZINTER 2 a b WITHSCORES",
            "ZUNION 2 a b WITHSCORES",
            "ZDIFF 2 a b WITHSCORES",
            "ZPOPMIN z",
            "ZPOPMAX z 2",
        ] {
            for reply in [
                array([bulk("a"), bulk("1.25"), bulk("b"), bulk("-inf")]),
                array([
                    array([bulk("a"), Value::Double(1.25)]),
                    array([bulk("b"), Value::Double(f64::NEG_INFINITY)]),
                ]),
                Value::Map(vec![
                    (bulk("a"), Value::Double(1.25)),
                    (bulk("b"), bulk("-inf")),
                ]),
            ] {
                let shaped = shape(&stmt(cmd), reply);
                assert_eq!(shaped.columns[1].data_type, "double");
                assert_table(
                    shaped,
                    &["member", "score"],
                    json!([["a", 1.25], ["b", "-inf"]]),
                );
            }
        }
        assert_table(
            shape(
                &stmt("ZUNION 2 WITHSCORES z"),
                array([bulk("a"), bulk("b")]),
            ),
            &["member"],
            json!([["a"], ["b"]]),
        );
        let scan = shape(
            &stmt("ZSCAN z 0"),
            array([bulk("7"), array([bulk("a"), bulk("1")])]),
        );
        assert_eq!(scan.notice.as_deref(), Some("Next cursor: 7"));
        assert_table(scan, &["member", "score"], json!([["a", 1.0]]));
    }

    #[test]
    fn stream_range_keeps_field_order_and_separates_collisions_and_duplicates() {
        let fields = array([
            bulk("id"),
            bulk("user id"),
            bulk("name"),
            bulk("Ada"),
            bulk("name"),
            bulk("A"),
            bulk("id (2)"),
            bulk("literal suffix"),
            bulk(b"\xff"),
            bulk("binary field"),
            bulk(r#""\xff""#),
            bulk("literal quoting"),
        ]);
        let next = Value::Map(vec![
            (bulk("name"), bulk("Grace")),
            (bulk("extra"), bulk("new")),
        ]);
        for cmd in ["XRANGE s - +", "XREVRANGE s + -"] {
            assert_table(
                shape(
                    &stmt(cmd),
                    array([
                        array([bulk("1-0"), fields.clone()]),
                        array([bulk("2-0"), next.clone()]),
                    ]),
                ),
                &[
                    "id",
                    "id (2)",
                    "name",
                    "name (2)",
                    "id (2) (2)",
                    r#""\xff""#,
                    r#""\xff" (2)"#,
                    "extra",
                ],
                json!([
                    [
                        "1-0",
                        "user id",
                        "Ada",
                        "A",
                        "literal suffix",
                        "binary field",
                        "literal quoting",
                        null
                    ],
                    ["2-0", null, "Grace", null, null, null, null, "new"],
                ]),
            );
        }
    }

    #[test]
    fn stream_read_normalizes_both_envelopes_to_field_objects() {
        let fields = array([bulk("type"), bulk("pay"), bulk("value"), bulk(b"\xff")]);
        let entries = array([array([bulk("1-0"), fields])]);
        let cmd = stmt("XREAD STREAMS s 0");
        for reply in [
            array([array([bulk("s"), entries.clone()])]),
            Value::Map(vec![(bulk("s"), entries)]),
        ] {
            assert_table(
                shape(&cmd, reply),
                &["stream", "id", "fields"],
                json!([[
                    "s",
                    "1-0",
                    r#"{"type":"pay","value":{"$bytes":"/w==","len":1}}"#
                ]]),
            );
        }
        assert_table(
            shape(&cmd, Value::Nil),
            &["stream", "id", "fields"],
            json!([]),
        );
    }

    #[test]
    fn info_client_list_and_slowlog_parse_server_text_and_sequences() {
        assert_table(
            shape(
                &stmt("INFO"),
                bulk("# Server\r\nredis_version:7.4.1\r\n\r\n# Replication\r\nmaster_host:::1\r\n"),
            ),
            &["section", "field", "value"],
            json!([
                ["Server", "redis_version", "7.4.1"],
                ["Replication", "master_host", "::1"]
            ]),
        );
        assert_table(
            shape(
                &stmt("client list"),
                bulk("id=1 addr=127.0.0.1:62144 name=\nid=2 name=x=y extra=ignored\n"),
            ),
            &["id", "addr", "name"],
            json!([["1", "127.0.0.1:62144", ""], ["2", null, "x=y"]]),
        );
        assert_table(
            shape(
                &stmt("SLOWLOG GET"),
                array([array([
                    Value::Int(7),
                    Value::Int(1710000000),
                    Value::Int(188),
                    array([bulk("GET"), bulk("sample")]),
                    bulk("127.0.0.1:62144"),
                    bulk(""),
                ])]),
            ),
            &[
                "id",
                "timestamp",
                "duration_us",
                "command",
                "client",
                "name",
            ],
            json!([[
                7,
                1710000000,
                188,
                r#"["GET","sample"]"#,
                "127.0.0.1:62144",
                ""
            ]]),
        );
    }

    #[test]
    fn fallback_distinguishes_scalars_matrices_and_nested_replies() {
        let cmd = stmt("EVAL_RO script 0");
        let reply = redis::parse_redis_value(b"*5\r\n+OK\r\n#t\r\n:9223372036854775807\r\n(1234567890123456789012345\r\n-ERR bad argument\r\n").unwrap();
        let shaped = shape(&cmd, reply);
        assert_eq!(shaped.columns[1].data_type, "mixed");
        assert_table(
            shaped,
            &["#", "value"],
            json!([
                [1, "OK"],
                [2, true],
                [3, "9223372036854775807"],
                [4, "1234567890123456789012345"],
                [5, "ERR bad argument"]
            ]),
        );
        let reply = Value::Attribute {
            attributes: vec![(bulk("meta"), Value::Int(1))],
            data: Box::new(Value::Boolean(true)),
        };
        let shaped = shape(&cmd, reply);
        assert_eq!(shaped.columns[0].data_type, "boolean");
        assert_table(shaped, &["value"], json!([[true]]));
        let map = Value::Map(vec![(bulk(b"\xff"), Value::Int(5))]);
        assert_table(
            shape(&cmd, map),
            &["field", "value"],
            json!([[{"$bytes":"/w==", "len":1}, 5]]),
        );
        let matrix = array([
            array([Value::Int(4), bulk("x")]),
            array([Value::Nil, bulk("y")]),
        ]);
        let shaped = shape(&cmd, matrix);
        assert_eq!(shaped.columns[1].data_type, "integer");
        assert_table(
            shaped,
            &["#", "1", "2"],
            json!([[1, 4, "x"], [2, null, "y"]]),
        );
        let ragged = array([
            array([Value::Int(5)]),
            Value::Set(vec![bulk("x"), Value::Nil]),
        ]);
        assert_table(
            shape(&cmd, ragged),
            &["#", "value"],
            json!([[1, "[5]"], [2, r#"["x",null]"#]]),
        );
        let nested = Value::Map(vec![(
            bulk("n"),
            array([bulk(b"\xff"), Value::Double(1.5)]),
        )]);
        assert_eq!(
            super::json(&nested),
            json!({"n":[{"$bytes":"/w==", "len":1},1.5]})
        );
        assert_table(
            shape(&cmd, nested),
            &["#", "value"],
            json!([[1, r#"{"n":[{"$bytes":"/w==","len":1},1.5]}"#]]),
        );
    }

    #[test]
    fn full_results_keep_bytes_and_utf8_that_grid_previews_cap() {
        let cmd = stmt("GET k");
        let text = format!("{}é", "a".repeat(PREVIEW_BYTES - 1));
        let preview = shape(&cmd, bulk(&text));
        assert_eq!(
            preview.rows[0][1],
            json!(format!("{}…", "a".repeat(PREVIEW_BYTES - 1)))
        );
        assert_eq!(shape_full(&cmd, bulk(&text)).rows[0][1], json!(text));

        let bytes = vec![0xff; PREVIEW_BYTES + 1];
        let preview = shape(&cmd, bulk(&bytes));
        let full = shape_full(&cmd, bulk(&bytes));
        assert_eq!(preview.rows[0][1], cell::bytes(&bytes, Some(PREVIEW_BYTES)));
        assert_eq!(full.rows[0][1], cell::bytes(&bytes, None));
        assert_ne!(preview.rows, full.rows);
    }
}
