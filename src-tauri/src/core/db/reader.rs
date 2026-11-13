//! One row value → JSON, per engine. The grid reads the preview, export and the AI read read the
//! preview too (the AI keeps its own byte budget), while `db::export`'s CSV writer reads the full
//! value: [`value::View`] makes that choice a type, not a flag.

use serde_json::Value;
use sqlx::mysql::{MySql, MySqlRow};
use sqlx::postgres::{PgRow, PgValueFormat};
use sqlx::sqlite::{Sqlite, SqliteRow};
use sqlx::{Column, Database, Decode, Row, TypeInfo, ValueRef};

use crate::core::cell;
use crate::core::db::value::{self, SqliteRaw, View};
use crate::core::result::ColumnMeta;

pub(crate) type CellFn<DB> = fn(&<DB as Database>::Row, usize) -> Value;

pub(crate) fn meta<C: Column>(c: &C) -> ColumnMeta {
    let t = c.type_info().name();
    ColumnMeta {
        name: c.name().to_string(),
        // sqlx names a PG type it has not looked up (e.g. an enum in a simple query) `?`.
        data_type: if t == "?" {
            "unknown".into()
        } else {
            t.to_string()
        },
        mixed: false,
    }
}

/// A MySQL catalog cell as text: MySQL 8 reports some catalog columns as binary strings, which
/// sqlx's typed `String` decode rejects.
pub(crate) fn mysql_text(row: &MySqlRow, i: usize) -> Option<String> {
    let v = row.try_get_raw(i).ok()?;
    if v.is_null() {
        return None;
    }
    <&[u8] as Decode<MySql>>::decode(v)
        .ok()
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

pub(crate) fn pg_cell<V: View>(row: &PgRow, i: usize) -> Value {
    let Ok(v) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if v.is_null() {
        return Value::Null;
    }
    let ty = v.type_info().name().to_string();
    match (v.format(), v.as_bytes()) {
        (PgValueFormat::Text, Ok(b)) => match std::str::from_utf8(b) {
            Ok(s) => value::pg_text::<V>(&ty, s),
            Err(_) => cell::bytes(b, V::CAP),
        },
        // raw_sql uses the simple query protocol, which always answers in text.
        _ => cell::unsupported(&ty),
    }
}

pub(crate) fn mysql_cell<V: View>(row: &MySqlRow, i: usize) -> Value {
    let Ok(v) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if v.is_null() {
        return Value::Null;
    }
    let ty = v.type_info().name().to_string();
    match <&[u8] as Decode<MySql>>::decode(v) {
        Ok(b) => value::mysql_value::<V>(&ty, b),
        Err(_) => cell::unsupported(&ty),
    }
}

pub(crate) fn sqlite_cell<V: View>(row: &SqliteRow, i: usize) -> Value {
    let declared = row
        .columns()
        .get(i)
        .map(|c| c.type_info().name().to_string())
        .unwrap_or_default();
    let Ok(v) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if v.is_null() {
        return Value::Null;
    }
    // A value's own type is its storage class (INTEGER, REAL, TEXT, BLOB).
    let storage = v.type_info().name().to_string();
    let decoded = match storage.as_str() {
        "INTEGER" | "BOOLEAN" => <i64 as Decode<Sqlite>>::decode(v)
            .map(|x| value::sqlite_value::<V>(&declared, SqliteRaw::Int(x))),
        "REAL" => <f64 as Decode<Sqlite>>::decode(v)
            .map(|x| value::sqlite_value::<V>(&declared, SqliteRaw::Real(x))),
        "BLOB" => <&[u8] as Decode<Sqlite>>::decode(v)
            .map(|x| value::sqlite_value::<V>(&declared, SqliteRaw::Blob(x))),
        _ => <&str as Decode<Sqlite>>::decode(v)
            .map(|x| value::sqlite_value::<V>(&declared, SqliteRaw::Text(x))),
    };
    decoded.unwrap_or_else(|_| match row.try_get_raw(i) {
        // Text that is not valid UTF-8 is shown as bytes.
        Ok(v) => <&[u8] as Decode<Sqlite>>::decode(v)
            .map(|b| cell::bytes(b, V::CAP))
            .unwrap_or_else(|_| cell::unsupported(&declared)),
        Err(_) => cell::unsupported(&declared),
    })
}

/// MySQL/MariaDB binary-protocol value (prepared statement) by the sqlx type name. Integers and
/// floats are little-endian; strings, blobs, decimals and BIT come as their content bytes;
/// DATE/TIME family is unpacked to ISO text.
pub(crate) fn mysql_cell_binary<V: View>(row: &MySqlRow, i: usize) -> Value {
    let Ok(v) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if v.is_null() {
        return Value::Null;
    }
    let ty = v.type_info().name().to_string();
    let t = ty.to_ascii_uppercase();
    let Ok(raw) = <&[u8] as Decode<MySql>>::decode(v) else {
        return cell::unsupported(&t);
    };
    match t.as_str() {
        "BOOLEAN" => return Value::Bool(raw.first() == Some(&1)),
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => {
            // Little-endian, two's complement; sign-extend widths below 8 bytes.
            let u = raw.iter().rev().fold(0u64, |a, b| (a << 8) | u64::from(*b));
            let i = if raw.len() < 8 && raw.last().is_some_and(|b| b & 0x80 != 0) {
                (u | (u64::MAX << (raw.len() * 8))) as i64
            } else {
                u as i64
            };
            return cell::int(i);
        }
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" => {
            let u = raw.iter().rev().fold(0u64, |a, b| (a << 8) | u64::from(*b));
            return match i64::try_from(u) {
                Ok(i) => cell::int(i),
                Err(_) => Value::String(u.to_string()),
            };
        }
        "YEAR" if raw.len() == 2 => {
            return cell::int(i64::from(u16::from_le_bytes([raw[0], raw[1]])));
        }
        "FLOAT" if raw.len() == 4 => {
            return cell::float(f32::from_le_bytes(raw.try_into().unwrap_or([0; 4])) as f64);
        }
        "DOUBLE" if raw.len() == 8 => {
            return cell::float(f64::from_le_bytes(raw.try_into().unwrap_or([0; 8])));
        }
        "DECIMAL" | "NUMERIC" => {
            return value::mysql_value::<V>("DECIMAL", raw);
        }
        "DATE" | "DATETIME" | "TIMESTAMP" => {
            if let Some(s) = mysql_binary_datetime(raw) {
                return Value::String(s);
            }
        }
        "TIME" => {
            if let Some(s) = mysql_binary_time(raw) {
                return Value::String(s);
            }
        }
        _ => {}
    }
    // Strings, blobs, JSON, enums: the content bytes are the value.
    value::mysql_value::<V>(&t, raw)
}

/// `YYYY-MM-DD[ HH:MM:SS[.ffffff]]` from the packed binary form (length byte first).
fn mysql_binary_datetime(raw: &[u8]) -> Option<String> {
    let (n, b) = (*raw.first()? as usize, &raw[1..]);
    if n == 0 || b.len() < 4 {
        return None;
    }
    let (y, mo, d) = (u16::from_le_bytes([b[0], b[1]]), b[2], b[3]);
    let mut s = format!("{y:04}-{mo:02}-{d:02}");
    if n >= 7 && b.len() >= 7 {
        s += &format!(" {:02}:{:02}:{:02}", b[4], b[5], b[6]);
        if n >= 11 && b.len() >= 11 {
            let us = u32::from_le_bytes([b[7], b[8], b[9], b[10]]);
            s += &format!(".{us:06}");
        }
    }
    Some(s)
}

/// `[-][d]HH:MM:SS[.ffffff]` from the packed binary TIME form.
fn mysql_binary_time(raw: &[u8]) -> Option<String> {
    let (n, b) = (*raw.first()? as usize, &raw[1..]);
    if n == 0 {
        return Some("00:00:00".to_string());
    }
    if n < 8 || b.len() < 8 {
        return None;
    }
    let neg = if b[0] == 1 { "-" } else { "" };
    let days = u32::from_le_bytes([b[1], b[2], b[3], b[4]]);
    let mut s = format!(
        "{neg}{:02}:{:02}:{:02}",
        days * 24 + u32::from(b[5]),
        b[6],
        b[7]
    );
    if n >= 12 && b.len() >= 12 {
        let us = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        s += &format!(".{us:06}");
    }
    Some(s)
}
