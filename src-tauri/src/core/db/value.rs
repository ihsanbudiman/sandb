//! Maps database values to JSON for the grid and CSV export. Pure functions, unit tested; the
//! per-engine row readers in `db/reader.rs` call them.
//!
//! Rules: integers → number while JS holds them exactly, else string; numeric/decimal → string
//! (keeps precision); floats → number; json → JSON value; bytes → `{"$bytes": base64, "len": n}`;
//! NULL → null; PostGIS geometry/geography → EWKT; the rest → text. The [`View`] type parameter
//! says whether long text and bytes are cut: the grid passes [`Preview`], export passes [`Full`].

use serde_json::Value;

use crate::core::cell::{self, PREVIEW_BYTES};
use crate::core::db::wkb;

/// How much of a value the caller wants. A type parameter, not an `Option<usize>` argument: a
/// caller cannot hand an export the preview cap by accident.
pub trait View {
    const CAP: Option<usize>;
}

/// The grid's cell: long text and bytes are cut at [`PREVIEW_BYTES`] and end with `…`.
pub struct Preview;

/// The whole value: an export or DDL file never truncates.
pub struct Full;

impl View for Preview {
    const CAP: Option<usize> = Some(PREVIEW_BYTES);
}

impl View for Full {
    const CAP: Option<usize> = None;
}

fn int_or_text(s: &str) -> Value {
    s.trim().parse::<i64>().map(cell::int).unwrap_or_else(|_| cell::text(s))
}

fn float_or_text(s: &str) -> Value {
    match s.trim().parse::<f64>() {
        Ok(f) if f.is_finite() => cell::float(f),
        _ => cell::text(s),
    }
}

fn json_or_text(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| cell::text(s))
}

/// PG text-format value (simple query protocol) by type name as sqlx reports it
/// (`INT4`, `TEXT[]`, …).
pub fn pg_text<V: View>(type_name: &str, s: &str) -> Value {
    pg_text_with(type_name, s, V::CAP)
}

fn pg_text_with(type_name: &str, s: &str, cap: Option<usize>) -> Value {
    let t = type_name.to_ascii_uppercase();
    if let Some(elem) = t.strip_suffix("[]") {
        return pg_array(elem, s, cap).unwrap_or_else(|| cell::text(s));
    }
    match t.as_str() {
        "BOOL" => match s {
            "t" => Value::Bool(true),
            "f" => Value::Bool(false),
            _ => cell::text(s),
        },
        "INT2" | "INT4" | "INT8" | "OID" => int_or_text(s),
        "FLOAT4" | "FLOAT8" => float_or_text(s),
        "JSON" | "JSONB" => json_or_text(s),
        "BYTEA" => pg_bytea(s).map_or_else(|| cell::text(s), |b| cell::bytes(&b, cap)),
        // Types sqlx has not looked up are `?`; PostGIS geometry and geography among them
        // arrive as hex EWKB and are shown as EWKT. Other values do not parse as EWKB.
        "?" => wkb::hex_ewkb_to_wkt(s, cap).map_or_else(|| cell::text_capped(s, cap), Value::String),
        // NUMERIC and MONEY stay strings on purpose; dates, uuid, enums, unknown types are text.
        _ => cell::text_capped(s, cap),
    }
}

/// `\xdeadbeef` (hex output, the default since PG 9.0) → bytes.
pub fn pg_bytea(s: &str) -> Option<Vec<u8>> {
    let hex = s.strip_prefix("\\x")?;
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// PG array literal (`{1,2,NULL}`, `{"a","b c"}`, `{{1,2},{3,4}}`, `[0:1]={…}`) → JSON array.
pub fn pg_array(elem_type: &str, s: &str, cap: Option<usize>) -> Option<Value> {
    let body = if s.starts_with('[') {
        &s[s.find('=')? + 1..]
    } else {
        s
    };
    let mut p = ArrayParser {
        chars: body.chars().collect(),
        pos: 0,
    };
    let v = p.array(elem_type, cap)?;
    p.skip_ws();
    (p.pos == p.chars.len()).then_some(v)
}

struct ArrayParser {
    chars: Vec<char>,
    pos: usize,
}

impl ArrayParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn array(&mut self, elem: &str, cap: Option<usize>) -> Option<Value> {
        self.skip_ws();
        if self.bump()? != '{' {
            return None;
        }
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Some(Value::Array(out));
        }
        loop {
            self.skip_ws();
            let v = match self.peek()? {
                '{' => self.array(elem, cap)?,
                '"' => {
                    self.pos += 1;
                    let mut s = String::new();
                    loop {
                        match self.bump()? {
                            '\\' => s.push(self.bump()?),
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    pg_text_with(elem, &s, cap)
                }
                _ => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c != ',' && c != '}') {
                        self.pos += 1;
                    }
                    let raw: String = self.chars[start..self.pos].iter().collect();
                    let raw = raw.trim();
                    if raw.eq_ignore_ascii_case("NULL") {
                        Value::Null
                    } else {
                        pg_text_with(elem, raw, cap)
                    }
                }
            };
            out.push(v);
            self.skip_ws();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(Value::Array(out)),
                _ => return None,
            }
        }
    }
}

/// MySQL/MariaDB text-protocol value by the sqlx type name (`INT`, `BOOLEAN`, `BLOB`, …).
pub fn mysql_value<V: View>(type_name: &str, raw: &[u8]) -> Value {
    mysql_value_with(type_name, raw, V::CAP)
}

fn mysql_value_with(type_name: &str, raw: &[u8], cap: Option<usize>) -> Value {
    let t = type_name.to_ascii_uppercase();
    let as_text = std::str::from_utf8(raw);
    match t.as_str() {
        "NULL" => Value::Null,
        "BOOLEAN" => match raw {
            b"0" => Value::Bool(false),
            b"1" => Value::Bool(true),
            _ => as_text.map_or_else(|_| cell::unsupported(&t), int_or_text),
        },
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" | "TINYINT UNSIGNED"
        | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" => {
            as_text.map_or_else(|_| cell::unsupported(&t), int_or_text)
        }
        "FLOAT" | "DOUBLE" => as_text.map_or_else(|_| cell::unsupported(&t), float_or_text),
        "JSON" => as_text.map_or_else(|_| cell::bytes(raw, cap), json_or_text),
        // sqlx reports text with a binary collation (utf8mb4_bin, MariaDB's JSON) as these types and
        // does not expose the collation, so readable UTF-8 is shown as text and anything else as bytes.
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => match as_text {
            Ok(s) if looks_like_text(s) => cell::text_capped(s, cap),
            _ => cell::bytes(raw, cap),
        },
        "BIT" | "GEOMETRY" => cell::bytes(raw, cap),
        // DECIMAL stays a string; text, enum, set, date and time types are text.
        "DECIMAL" | "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT"
        | "ENUM" | "SET" | "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" => {
            as_text.map_or_else(|_| cell::bytes(raw, cap), |s| cell::text_capped(s, cap))
        }
        _ => as_text.map_or_else(|_| cell::unsupported(&t), |s| cell::text_capped(s, cap)),
    }
}

fn looks_like_text(s: &str) -> bool {
    !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

pub enum SqliteRaw<'a> {
    Null,
    Int(i64),
    Real(f64),
    Text(&'a str),
    Blob(&'a [u8]),
}

/// `declared`: the sqlx column type name (`BOOLEAN` when declared so); storage class decides the rest.
pub fn sqlite_value<V: View>(declared: &str, raw: SqliteRaw<'_>) -> Value {
    sqlite_value_with(declared, raw, V::CAP)
}

fn sqlite_value_with(declared: &str, raw: SqliteRaw<'_>, cap: Option<usize>) -> Value {
    match raw {
        SqliteRaw::Null => Value::Null,
        SqliteRaw::Int(i) if declared.eq_ignore_ascii_case("BOOLEAN") && (i == 0 || i == 1) => {
            Value::Bool(i == 1)
        }
        SqliteRaw::Int(i) => cell::int(i),
        SqliteRaw::Real(f) => cell::float(f),
        SqliteRaw::Text(s) => cell::text_capped(s, cap),
        SqliteRaw::Blob(b) => cell::bytes(b, cap),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cell::{bytes, text_capped};
    use serde_json::json;

    fn pg_text(t: &str, s: &str) -> Value {
        super::pg_text::<Full>(t, s)
    }

    fn pg_preview(t: &str, s: &str) -> Value {
        super::pg_text::<Preview>(t, s)
    }

    fn mysql_value(t: &str, b: &[u8]) -> Value {
        super::mysql_value::<Full>(t, b)
    }

    fn sqlite_value(t: &str, r: SqliteRaw<'_>) -> Value {
        super::sqlite_value::<Full>(t, r)
    }

    #[test]
    fn bytes_are_base64_with_cap() {
        assert_eq!(
            bytes(&[0xde, 0xad, 0xbe, 0xef], None),
            json!({"$bytes": "3q2+7w==", "len": 4})
        );
        assert_eq!(
            bytes(&[1, 2, 3, 4], Some(2)),
            json!({"$bytes": "AQI=", "len": 4})
        );
    }

    #[test]
    fn long_text_is_cut_only_with_a_cap() {
        let wkt = format!("POLYGON(({}))", "106.8 -6.2, ".repeat(100_000));
        let v = pg_preview("TEXT", &wkt);
        let s = v.as_str().unwrap();
        assert!(s.ends_with('…') && s.len() <= PREVIEW_BYTES + '…'.len_utf8());
        assert!(wkt.starts_with(s.trim_end_matches('…')));
        assert_eq!(
            pg_text("TEXT", &wkt),
            json!(wkt),
            "export keeps the full value"
        );
        assert_eq!(text_capped("short", Some(PREVIEW_BYTES)), json!("short"));
        // A cut inside a multi-byte char backs off to the char start.
        assert_eq!(text_capped("ab日本", Some(3)), json!("ab…"));
        assert_eq!(
            super::mysql_value::<Preview>("LONGTEXT", wkt.as_bytes()),
            pg_preview("TEXT", &wkt)
        );
        assert_eq!(
            super::sqlite_value::<Preview>("TEXT", SqliteRaw::Text(&wkt)),
            pg_preview("TEXT", &wkt)
        );
    }

    #[test]
    fn pg_unlooked_up_types_show_geometry_as_wkt_and_keep_other_text() {
        // `SRID=4326;POINT(106.8 -6.2)` as PostGIS prints it.
        let point = "0101000020E61000003333333333B35A40CDCCCCCCCCCC18C0";
        assert_eq!(pg_text("?", point), json!("POINT(106.8 -6.2)"));
        // Enums and other extension types are `?` too and stay as they are.
        assert_eq!(pg_text("?", "active"), json!("active"));
        assert_eq!(pg_text("?", "0101"), json!("0101"));
        // A known text column that holds hex EWKB is the user's text, not a geometry.
        assert_eq!(pg_text("TEXT", point), json!(point));
    }

    #[test]
    fn pg_scalars() {
        assert_eq!(pg_text("INT4", "-2147483648"), json!(-2147483648_i64));
        assert_eq!(
            pg_text("INT8", "9007199254740993"),
            json!("9007199254740993")
        );
        assert_eq!(pg_text("INT8", "42"), json!(42));
        assert_eq!(pg_text("NUMERIC", "1234.5678"), json!("1234.5678"));
        assert_eq!(pg_text("FLOAT8", "-1.5e+300"), json!(-1.5e300));
        assert_eq!(pg_text("FLOAT8", "NaN"), json!("NaN"));
        assert_eq!(pg_text("BOOL", "t"), json!(true));
        assert_eq!(pg_text("BOOL", "f"), json!(false));
        assert_eq!(
            pg_text("JSONB", r#"{"a": [1, 2]}"#),
            json!({"a": [1, 2]})
        );
        assert_eq!(pg_text("JSON", "not json"), json!("not json"));
        assert_eq!(
            pg_text("BYTEA", "\\xdeadbeef"),
            json!({"$bytes": "3q2+7w==", "len": 4})
        );
        assert_eq!(pg_text("?", "happy"), json!("happy"));
        assert_eq!(
            pg_text("TIMESTAMPTZ", "2026-11-02 03:30:00+00"),
            json!("2026-11-02 03:30:00+00")
        );
        assert_eq!(pg_text("TEXT", "🚀 café"), json!("🚀 café"));
    }

    #[test]
    fn pg_bytea_rejects_bad_hex() {
        assert_eq!(pg_bytea("\\x00ff"), Some(vec![0, 255]));
        assert_eq!(pg_bytea("\\x0"), None);
        assert_eq!(pg_bytea("\\xzz"), None);
        assert_eq!(pg_bytea("abc"), None);
        assert_eq!(pg_text("BYTEA", "abc"), json!("abc"));
    }

    #[test]
    fn pg_arrays() {
        assert_eq!(pg_text("INT4[]", "{1,2,3}"), json!([1, 2, 3]));
        assert_eq!(pg_text("INT4[]", "{}"), json!([]));
        assert_eq!(
            pg_text("TEXT[]", r#"{a,"b c",NULL,"NULL","x\"y\\z"}"#),
            json!(["a", "b c", null, "NULL", "x\"y\\z"])
        );
        assert_eq!(
            pg_text("INT8[]", "{{1,2},{3,4}}"),
            json!([[1, 2], [3, 4]])
        );
        assert_eq!(pg_text("INT4[]", "[0:1]={7,8}"), json!([7, 8]));
        assert_eq!(
            pg_text("BYTEA[]", r#"{"\\x00"}"#),
            json!([{"$bytes": "AA==", "len": 1}])
        );
        assert_eq!(pg_text("INT4[]", "{1,2"), json!("{1,2"));
    }

    #[test]
    fn mysql_values() {
        assert_eq!(mysql_value("BOOLEAN", b"1"), json!(true));
        assert_eq!(mysql_value("BOOLEAN", b"0"), json!(false));
        assert_eq!(mysql_value("INT", b"-5"), json!(-5));
        assert_eq!(
            mysql_value("BIGINT", b"-9223372036854775808"),
            json!("-9223372036854775808")
        );
        assert_eq!(mysql_value("DECIMAL", b"-0.0001"), json!("-0.0001"));
        assert_eq!(mysql_value("BIGINT", b"1"), json!(1));
        assert_eq!(
            mysql_value("BLOB", &[0]),
            json!({"$bytes": "AA==", "len": 1})
        );
        assert_eq!(
            mysql_value("VARBINARY", b""),
            json!({"$bytes": "", "len": 0})
        );
        // Binary-collated text (utf8mb4_bin, MariaDB JSON) reads as text.
        assert_eq!(
            mysql_value("LONGBLOB", "{\"a\": 1} 🚀\n".as_bytes()),
            json!("{\"a\": 1} 🚀\n")
        );
        assert_eq!(mysql_value("DOUBLE", b"2.5e-3"), json!(0.0025));
        assert_eq!(mysql_value("JSON", br#"{"a": 1}"#), json!({"a": 1}));
        assert_eq!(
            mysql_value("BLOB", &[0xde, 0xad, 0xbe, 0xef]),
            json!({"$bytes": "3q2+7w==", "len": 4})
        );
        assert_eq!(mysql_value("ENUM", b"happy"), json!("happy"));
        assert_eq!(
            mysql_value("VARCHAR", "emoji 👍".as_bytes()),
            json!("emoji 👍")
        );
        assert_eq!(
            mysql_value("VARCHAR", &[0xff]),
            json!({"$bytes": "/w==", "len": 1})
        );
        assert_eq!(mysql_value("WHATEVER", b"x"), json!("x"));
        assert_eq!(
            mysql_value("WHATEVER", &[0xff]),
            json!("<unsupported: WHATEVER>")
        );
    }

    #[test]
    fn sqlite_values() {
        assert_eq!(
            sqlite_value("BOOLEAN", SqliteRaw::Int(1)),
            json!(true)
        );
        assert_eq!(sqlite_value("BOOLEAN", SqliteRaw::Int(7)), json!(7));
        assert_eq!(
            sqlite_value("INTEGER", SqliteRaw::Int(9_007_199_254_740_993)),
            json!("9007199254740993")
        );
        assert_eq!(sqlite_value("REAL", SqliteRaw::Real(0.5)), json!(0.5));
        assert_eq!(
            sqlite_value("TEXT", SqliteRaw::Text("日本語")),
            json!("日本語")
        );
        assert_eq!(
            sqlite_value("BLOB", SqliteRaw::Blob(&[0])),
            json!({"$bytes": "AA==", "len": 1})
        );
        assert_eq!(sqlite_value("TEXT", SqliteRaw::Null), Value::Null);
    }
}
