//! One BSON value model: the type name the grid sees, the preview cell, the mongosh literal and
//! the typed `{type, value}` wire value. Each direction is one exhaustive `Bson` match, so a type
//! that has no representation is refused in exactly one place.
//!
//! Every literal renders to text that `parse` reads back as the same BSON type and the same bits,
//! so an exported collection is the collection it came from. Types that have no constructor in
//! mongosh (Symbol, Undefined, DbPointer, CodeWithScope) are refused, never approximated.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Document, Regex};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::core::cell::{self, PREVIEW_BYTES};
use crate::core::error::AppError;
use crate::core::mongo::parse;
use crate::core::result::TypedValue;

/// The largest date `new Date(ms)` reads back exactly: above 2^53 a JavaScript number loses
/// digits.
const MAX_EXACT_DOUBLE: u64 = 1 << 53;

/// The byte length of a UUID-subtype binary; only this length is the `uuid` type of the grid.
const UUID_BYTES: usize = 16;

/// Whether a binary is the 16-byte UUID subtype; anything else is `binData`, in every direction.
fn is_uuid(binary: &Binary) -> bool {
    binary.subtype == BinarySubtype::Uuid && binary.bytes.len() == UUID_BYTES
}

/// The grid's type name of a value, e.g. "int32", "objectId". The frontend cell editor depends
/// on these strings.
pub fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        // Only the standard UUID subtype (0x04) of the right length is a uuid; the legacy
        // subtype and a short or long payload stay binData, as the preview and the literal have
        // them.
        Bson::Binary(binary) if is_uuid(binary) => "uuid",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::Undefined => "undefined",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int32",
        Bson::Int64(_) => "int64",
        Bson::Timestamp(_) => "timestamp",
        Bson::Decimal128(_) => "decimal128",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// The server's `$type` alias of a value, for a filter that must match the stored type exactly.
pub fn type_alias(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

pub fn cell(v: &Bson) -> Value {
    match v {
        Bson::Double(d) => match *d {
            d if d.is_nan() => json!("NaN"),
            f64::INFINITY => json!("Infinity"),
            f64::NEG_INFINITY => json!("-Infinity"),
            d => json!(d),
        },
        Bson::Int32(i) => json!(i),
        Bson::Int64(i) => cell::int(*i),
        Bson::Decimal128(d) => json!(d.to_string()),
        Bson::String(s) | Bson::Symbol(s) => cell::text_capped(s, Some(PREVIEW_BYTES)),
        Bson::Boolean(b) => json!(b),
        Bson::Null => Value::Null,
        // Not a null cell: the grid cannot write it, so it must not look like one.
        Bson::Undefined => json!("undefined"),
        Bson::ObjectId(id) => json!(id.to_hex()),
        Bson::Document(d) if d.is_empty() => json!("{}"),
        Bson::Document(d) => json!(format!("{{{}}}", summary(d.len(), "field", "fields"))),
        Bson::Array(a) => json!(format!("[{}]", summary(a.len(), "item", "items"))),
        Bson::DateTime(dt) => match date_text(*dt) {
            DateText::Iso(s) => json!(s),
            DateText::Millis(ms) => json!(ms.to_string()),
        },
        Bson::Timestamp(ts) => json!(format!("Timestamp({}, {})", ts.time, ts.increment)),
        Bson::Binary(b) if is_uuid(b) => json!(uuid_text(b)),
        Bson::Binary(b) => cell::bytes(&b.bytes, Some(PREVIEW_BYTES)),
        Bson::RegularExpression(re) => json!(format!("/{}/{}", re.pattern, re.options)),
        Bson::JavaScriptCode(code) => cell::text_capped(code, Some(PREVIEW_BYTES)),
        Bson::JavaScriptCodeWithScope(code) => cell::text_capped(&code.code, Some(PREVIEW_BYTES)),
        Bson::MinKey => json!("MinKey"),
        Bson::MaxKey => json!("MaxKey"),
        Bson::DbPointer(_) => json!("DBPointer"),
    }
}

/// The full value of one document field for CSV: text and bytes uncut, arrays and documents as
/// compact relaxed Extended JSON, everything else as the grid shows it. The grid caps text and
/// binary and collapses arrays, so [`cell`] is not the CSV rule; `core::cell::field` alone does
/// not know the BSON values.
pub fn csv(v: &Bson) -> String {
    match v {
        Bson::String(s) | Bson::Symbol(s) => s.clone(),
        Bson::JavaScriptCode(code) => code.clone(),
        Bson::Array(_) | Bson::Document(_) => v.clone().into_relaxed_extjson().to_string(),
        Bson::Binary(b) if !is_uuid(b) => B64.encode(&b.bytes),
        _ => cell::field(&cell(v)),
    }
}

fn summary(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn uuid_text(binary: &Binary) -> String {
    // `is_uuid` checked the length, so `from_slice` is infallible.
    Uuid::from_slice(&binary.bytes)
        .expect("16 bytes")
        .to_string()
}

/// `v` as mongosh text. Documents and arrays are indented by two spaces; a scalar is one line.
pub fn literal(v: &Bson) -> Result<String, AppError> {
    let mut out = String::new();
    write(v, false, 0, &mut out)?;
    Ok(out)
}

pub fn compact(v: &Bson) -> Result<String, AppError> {
    let mut out = String::new();
    write(v, true, 0, &mut out)?;
    Ok(out)
}

/// `doc` as one line. A refused value names the type and, when the document has one, its `_id`,
/// so the user can find the document in the collection.
pub fn document(doc: &Document) -> Result<String, AppError> {
    let mut out = String::new();
    match write_doc(doc, true, 0, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => Err(match doc.get("_id") {
            Some(id) => AppError::invalid(format!(
                "{} (document _id: {})",
                e.message,
                id.clone().into_relaxed_extjson()
            )),
            None => e,
        }),
    }
}

/// A string as a mongosh double-quoted literal: the JSON escapes are the JavaScript ones.
pub fn quoted(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

/// A field key of an object literal. Even a quoted `"__proto__"` sets the prototype of the new
/// object instead of adding an own field, so that one key is written computed: `["__proto__"]`.
fn key_literal(key: &str) -> String {
    if key == "__proto__" {
        format!("[{}]", quoted(key))
    } else {
        quoted(key)
    }
}

fn write(v: &Bson, one_line: bool, depth: usize, out: &mut String) -> Result<(), AppError> {
    match v {
        Bson::Document(d) => write_doc(d, one_line, depth, out)?,
        Bson::Array(a) => write_array(a, one_line, depth, out)?,
        Bson::String(s) => out.push_str(&quoted(s)),
        Bson::Int32(i) => out.push_str(&i.to_string()),
        Bson::Int64(i) => out.push_str(&format!("NumberLong(\"{i}\")")),
        Bson::Double(d) => out.push_str(&number(*d)),
        Bson::Decimal128(d) => out.push_str(&format!("NumberDecimal(\"{d}\")")),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null => out.push_str("null"),
        Bson::DateTime(d) => out.push_str(&date_literal(*d)?),
        Bson::ObjectId(id) => out.push_str(&format!("ObjectId(\"{}\")", id.to_hex())),
        Bson::Binary(b) if is_uuid(b) => {
            out.push_str(&format!("UUID(\"{}\")", uuid_text(b)));
        }
        Bson::Binary(b) => out.push_str(&format!(
            "BinData({}, \"{}\")",
            u8::from(b.subtype),
            B64.encode(&b.bytes)
        )),
        Bson::Timestamp(ts) => out.push_str(&format!(
            "Timestamp({{t: {}, i: {}}})",
            ts.time, ts.increment
        )),
        Bson::RegularExpression(re) if regex_literal_ok(re) => {
            out.push_str(&format!("/{}/{}", re.pattern, re.options));
        }
        Bson::RegularExpression(re) => out.push_str(&format!(
            "RegExp({}, {})",
            quoted(&re.pattern),
            quoted(&re.options)
        )),
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        Bson::JavaScriptCode(code) => out.push_str(&format!("Code({})", quoted(code))),
        Bson::Undefined
        | Bson::Symbol(_)
        | Bson::DbPointer(_)
        | Bson::JavaScriptCodeWithScope(_) => {
            return Err(AppError::invalid(format!(
                "Cannot write BSON type {}: mongosh has no constructor for it",
                type_name(v)
            )));
        }
    }
    Ok(())
}

fn write_doc(d: &Document, one_line: bool, depth: usize, out: &mut String) -> Result<(), AppError> {
    if d.is_empty() {
        out.push_str("{}");
        return Ok(());
    }
    if one_line {
        out.push('{');
        for (i, (key, value)) in d.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&key_literal(key));
            out.push_str(": ");
            write(value, true, depth + 1, out)?;
        }
        out.push('}');
        return Ok(());
    }
    out.push_str("{\n");
    for (i, (key, value)) in d.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        indent(out, depth + 1);
        out.push_str(&key_literal(key));
        out.push_str(": ");
        write(value, false, depth + 1, out)?;
    }
    out.push('\n');
    indent(out, depth);
    out.push('}');
    Ok(())
}

fn write_array(a: &[Bson], one_line: bool, depth: usize, out: &mut String) -> Result<(), AppError> {
    if a.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    if one_line {
        out.push('[');
        for (i, value) in a.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write(value, true, depth + 1, out)?;
        }
        out.push(']');
        return Ok(());
    }
    out.push_str("[\n");
    for (i, value) in a.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        indent(out, depth + 1);
        write(value, false, depth + 1, out)?;
    }
    out.push('\n');
    indent(out, depth);
    out.push(']');
    Ok(())
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// A double as text that reads back as the same double. A bare integer would read back as an
/// Int32, so an integral double keeps the `Double(...)` constructor; a fraction keeps the plain
/// literal, whose digits round-trip.
fn number(d: f64) -> String {
    if d.is_nan() {
        return "NaN".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if d.fract() == 0.0 {
        return format!("Double({d:?})");
    }
    format!("{d:?}")
}

/// The text of a date: RFC 3339 when the date has a year inside 0 to 9999, else the milliseconds.
/// The preview and the literal both read this, so they agree on which dates have two forms.
enum DateText {
    Iso(String),
    Millis(i64),
}

fn date_text(d: DateTime) -> DateText {
    match d.try_to_rfc3339_string() {
        Ok(s) => DateText::Iso(s),
        Err(_) => DateText::Millis(d.timestamp_millis()),
    }
}

/// `ISODate("...")`, or `new Date(ms)` when the date is outside the years 0 to 9999 and a
/// JavaScript number holds its milliseconds exactly.
fn date_literal(d: DateTime) -> Result<String, AppError> {
    match date_text(d) {
        DateText::Iso(s) => Ok(format!("ISODate(\"{s}\")")),
        DateText::Millis(ms) if ms.unsigned_abs() <= MAX_EXACT_DOUBLE => Ok(format!("new Date({ms})")),
        DateText::Millis(_) => Err(AppError::invalid(
            "Cannot write a date outside the range ISODate and new Date read back",
        )),
    }
}

/// Whether `/pattern/options` reads back as the same regex: the lexer ends the pattern at the
/// first `/` outside a class, `//` starts a comment, and the parser keeps sorted `imsux` flags.
fn regex_literal_ok(re: &Regex) -> bool {
    let mut class = false;
    let mut chars = re.pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            // A line terminator ends a literal: `/a\nb/` is not one regex in JavaScript.
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => return false,
            '\\' if chars.next().is_none() => return false,
            '\\' => {}
            '[' => class = true,
            ']' => class = false,
            '/' if !class => return false,
            _ => {}
        }
    }
    let flags = re.options.as_bytes();
    !re.pattern.is_empty()
        && !class
        && flags.iter().all(|f| b"imsux".contains(f))
        && flags.windows(2).all(|w| w[0] < w[1])
}

/// The BSON types the grid cell editor can write, in the order the type picker shows them.
/// `shell` is a literal mode of [`typed`], not a type, so it is not here.
pub const WRITABLE_TYPES: &[&str] = &[
    "string",
    "int32",
    "int64",
    "double",
    "decimal128",
    "bool",
    "date",
    "objectId",
    "uuid",
    "null",
];

/// The BSON value of one typed grid value. The type decides, not the text: `typed("double", "42")`
/// is the Double 42.0. `null` ignores `text`; `shell` is one mongosh value literal.
pub fn typed(ty: &str, text: &str) -> Result<Bson, AppError> {
    let bad = || AppError::invalid(format!("Invalid {ty} value: {text}"));
    let t = text.trim();
    Ok(match ty {
        "string" => Bson::String(text.to_string()),
        "int32" => Bson::Int32(t.parse().map_err(|_| bad())?),
        "int64" => Bson::Int64(t.parse().map_err(|_| bad())?),
        "double" => Bson::Double(double(t).ok_or_else(bad)?),
        "decimal128" => Bson::Decimal128(t.parse().map_err(|_| bad())?),
        "bool" => match t {
            "true" => Bson::Boolean(true),
            "false" => Bson::Boolean(false),
            _ => return Err(bad()),
        },
        "date" => Bson::DateTime(date(t).ok_or_else(bad)?),
        "objectId" => Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str(t).map_err(|_| bad())?),
        "uuid" => Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: Uuid::parse_str(t).map_err(|_| bad())?.as_bytes().to_vec(),
        }),
        "null" => Bson::Null,
        "shell" => parse::parse_value(text).map_err(AppError::from)?,
        _ => return Err(cannot_write(ty)),
    })
}

fn cannot_write(ty: &str) -> AppError {
    AppError::invalid(format!("The grid cannot write BSON type {ty}"))
}

/// The JavaScript number texts; Rust alone would also read `inf` and `nan`.
fn double(t: &str) -> Option<f64> {
    match t {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ if t
            .bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) =>
        {
            t.parse().ok()
        }
        _ => None,
    }
}

/// RFC 3339, or the milliseconds the grid shows for a date outside the years 0 to 9999.
fn date(t: &str) -> Option<DateTime> {
    match t.parse::<i64>() {
        Ok(ms) => Some(DateTime::from_millis(ms)),
        Err(_) => DateTime::parse_rfc3339_str(t).ok(),
    }
}

pub fn from_wire(t: &TypedValue) -> Result<Option<Bson>, AppError> {
    match (t.ty.as_str(), t.value.as_deref()) {
        ("missing", _) => Ok(None),
        ("null", _) => Ok(Some(Bson::Null)),
        (ty, Some(text)) => typed(ty, text).map(Some),
        (ty, None) => Err(AppError::invalid(format!("A {ty} value needs a text"))),
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::oid::ObjectId;
    use mongodb::bson::{Decimal128, JavaScriptCodeWithScope, Timestamp, doc};

    use super::*;

    /// The BSON bytes of a value: equal bytes are the same type and the same bits (NaN too).
    fn bits(v: &Bson) -> Vec<u8> {
        let mut out = Vec::new();
        doc! { "v": v.clone() }.to_writer(&mut out).unwrap();
        out
    }

    fn read_back(text: &str) -> Bson {
        parse::parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    /// Every value the renderer must keep, with its edge cases.
    fn values() -> Vec<Bson> {
        let mut v = vec![
            Bson::String(String::new()),
            Bson::String("héllo \"q\" \\ \n\t".into()),
            Bson::Int32(0),
            Bson::Int32(i32::MIN),
            Bson::Int32(i32::MAX),
            Bson::Int64(i64::MIN),
            Bson::Int64(i64::MAX),
            Bson::Boolean(true),
            Bson::Boolean(false),
            Bson::Null,
            Bson::ObjectId(ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap()),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e")
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::UserDefined(0x80),
                bytes: vec![],
            }),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![0, 1, 2, 255],
            }),
            Bson::Timestamp(Timestamp {
                time: u32::MAX,
                increment: 0,
            }),
            Bson::RegularExpression(Regex {
                pattern: "^a[/]b\\/c$".into(),
                options: "imsx".into(),
            }),
            // Not a regex literal: an empty pattern, a slash, a class left open, odd flags.
            Bson::RegularExpression(Regex {
                pattern: String::new(),
                options: String::new(),
            }),
            Bson::RegularExpression(Regex {
                pattern: "a/b".into(),
                options: String::new(),
            }),
            Bson::RegularExpression(Regex {
                pattern: "a[".into(),
                options: String::new(),
            }),
            Bson::RegularExpression(Regex {
                pattern: "a".into(),
                options: "l".into(),
            }),
            Bson::MinKey,
            Bson::MaxKey,
            Bson::JavaScriptCode("function () { return 1; }".into()),
            Bson::Document(doc! { "": "empty key", "a.b": 1, "$x": 2, "naïve 😀": "ü" }),
            Bson::Array(vec![
                Bson::Document(doc! { "x": [1, "two"] }),
                Bson::Int64(3),
            ]),
        ];
        for d in [
            0.0,
            -0.0,
            1.5,
            -1.5,
            42.0,
            0.1,
            1e21,
            -1e21,
            1e-7,
            5e-324,
            f64::MAX,
            f64::MIN_POSITIVE,
            9_007_199_254_740_993.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            v.push(Bson::Double(d));
        }
        for s in [
            "0",
            "1.50",
            "-0.0001",
            "1E+3",
            "NaN",
            "Infinity",
            "-Infinity",
        ] {
            v.push(Bson::Decimal128(s.parse::<Decimal128>().unwrap()));
        }
        for ms in [
            0,
            -1,
            1_700_000_000_123,
            253_402_300_799_999, // 9999-12-31T23:59:59.999Z
            253_402_300_800_000, // year 10000: milliseconds
            -62_167_219_200_001, // before year 0: milliseconds
        ] {
            v.push(Bson::DateTime(DateTime::from_millis(ms)));
        }
        v
    }

    fn dec(s: &str) -> Bson {
        Bson::Decimal128(s.parse::<Decimal128>().unwrap())
    }

    fn uuid_value() -> Bson {
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e")
                .unwrap()
                .as_bytes()
                .to_vec(),
        })
    }

    #[test]
    fn literal_shapes() {
        let d = Bson::Document(doc! { "a": [1, 2.0], "b": {}, "c": [] });
        assert_eq!(
            literal(&d).unwrap(),
            "{\n  \"a\": [\n    1,\n    Double(2.0)\n  ],\n  \"b\": {},\n  \"c\": []\n}"
        );
        // An integral double keeps its constructor: a bare `42` would read back as an Int32.
        assert_eq!(literal(&Bson::Double(42.0)).unwrap(), "Double(42.0)");
        assert_eq!(literal(&Bson::Double(1e21)).unwrap(), "Double(1e21)");
        assert_eq!(literal(&Bson::Double(-0.0)).unwrap(), "Double(-0.0)");
        assert_eq!(literal(&Bson::Double(1.5)).unwrap(), "1.5");
        assert_eq!(
            literal(&Bson::Int64(5)).unwrap(),
            "NumberLong(\"5\")"
        );
        assert_eq!(
            literal(&dec("1.50")).unwrap(),
            "NumberDecimal(\"1.50\")"
        );
        assert_eq!(
            literal(&Bson::DateTime(DateTime::from_millis(0))).unwrap(),
            "ISODate(\"1970-01-01T00:00:00Z\")"
        );
        assert_eq!(
            literal(&Bson::DateTime(DateTime::from_millis(253_402_300_800_000))).unwrap(),
            "new Date(253402300800000)"
        );
        assert_eq!(
            literal(&uuid_value()).unwrap(),
            "UUID(\"0f8fad5b-d9cb-469f-a165-70867728950e\")"
        );
        assert_eq!(
            literal(&Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![1, 2, 3],
            }))
            .unwrap(),
            "BinData(0, \"AQID\")"
        );
        assert_eq!(literal(&Bson::MinKey).unwrap(), "MinKey()");
        assert_eq!(
            literal(&Bson::String("a\"b".into())).unwrap(),
            "\"a\\\"b\""
        );
    }

    #[test]
    fn every_value_reads_back() {
        for v in values() {
            let text = compact(&v).unwrap();
            assert_eq!(bits(&read_back(&text)), bits(&v), "{v:?} → {text}");
            let text = literal(&v).unwrap();
            assert_eq!(bits(&read_back(&text)), bits(&v), "{v:?} → {text}");
        }
    }

    #[test]
    fn a_document_keeps_nested_values_and_odd_keys() {
        let d = doc! {
            "_id": 1,
            "": "empty key",
            "a.b": [1, 2.0, Bson::Null],
            "$x": { "$y": Bson::JavaScriptCode("1".into()) },
            "naïve 😀": Bson::MaxKey,
        };
        let text = document(&d).unwrap();
        assert!(!text.contains('\n'), "{text}");
        assert_eq!(bits(&read_back(&text)), bits(&Bson::Document(d.clone())));
        let pretty = literal(&Bson::Document(d.clone())).unwrap();
        assert!(pretty.contains('\n'), "{pretty}");
        assert_eq!(bits(&read_back(&pretty)), bits(&Bson::Document(d)));
    }

    #[test]
    fn shapes() {
        assert_eq!(compact(&Bson::Double(42.0)).unwrap(), "Double(42.0)");
        assert_eq!(compact(&Bson::Double(-0.0)).unwrap(), "Double(-0.0)");
        assert_eq!(compact(&Bson::Double(1.5)).unwrap(), "1.5");
        assert_eq!(compact(&Bson::Double(f64::NAN)).unwrap(), "NaN");
        assert_eq!(
            compact(&Bson::Double(f64::NEG_INFINITY)).unwrap(),
            "-Infinity"
        );
        assert_eq!(
            compact(&Bson::Timestamp(Timestamp {
                time: 1_700_000_000,
                increment: 4,
            }))
            .unwrap(),
            "Timestamp({t: 1700000000, i: 4})"
        );
        assert_eq!(
            compact(&Bson::RegularExpression(Regex {
                pattern: "a/b".into(),
                options: "i".into(),
            }))
            .unwrap(),
            "RegExp(\"a/b\", \"i\")"
        );
        assert_eq!(
            compact(&Bson::JavaScriptCode("x()".into())).unwrap(),
            "Code(\"x()\")"
        );
        assert_eq!(quoted("a\"b\n"), "\"a\\\"b\\n\"");
    }

    #[test]
    fn a_proto_key_is_a_computed_key() {
        // A quoted `"__proto__"` would set the prototype of the new object and lose the field.
        let d = doc! {
            "__proto__": { "__proto__": 1 },
            "ok": 2,
            "nested": { "a": [{ "__proto__": Bson::Null }] },
        };
        let text = document(&d).unwrap();
        assert!(text.contains("[\"__proto__\"]: "), "{text}");
        assert!(!text.contains("\"__proto__\": "), "{text}");
        assert_eq!(bits(&read_back(&text)), bits(&Bson::Document(d.clone())));
        let pretty = literal(&Bson::Document(d.clone())).unwrap();
        assert!(pretty.contains("[\"__proto__\"]: "), "{pretty}");
        assert_eq!(bits(&read_back(&pretty)), bits(&Bson::Document(d)));
    }

    #[test]
    fn every_line_terminator_leaves_the_literal() {
        for pattern in ["a\nb", "a\rb", "a\u{2028}b", "a\u{2029}b"] {
            let v = Bson::RegularExpression(Regex {
                pattern: pattern.into(),
                options: String::new(),
            });
            let text = compact(&v).unwrap();
            assert!(text.starts_with("RegExp("), "{pattern:?} → {text}");
            assert_eq!(bits(&read_back(&text)), bits(&v), "{pattern:?}");
        }
        let plain = Bson::RegularExpression(Regex {
            pattern: "a.b".into(),
            options: "i".into(),
        });
        assert_eq!(compact(&plain).unwrap(), "/a.b/i");
    }

    #[test]
    fn refused_types_name_the_document_id() {
        let pointer = Bson::try_from(json!({
            "$dbPointer": { "$ref": "c", "$id": { "$oid": "65a1b2c3d4e5f60718293a4b" } }
        }))
        .unwrap();
        assert!(matches!(pointer, Bson::DbPointer(_)));
        for (v, ty) in [
            (Bson::Undefined, "undefined"),
            (Bson::Symbol("s".into()), "symbol"),
            (
                Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                    code: "f()".into(),
                    scope: doc! {},
                }),
                "javascriptWithScope",
            ),
            (pointer, "dbPointer"),
        ] {
            let message = format!("Cannot write BSON type {ty}: mongosh has no constructor for it");
            for value in [
                v.clone(),
                Bson::Document(doc! { "a": [{ "b": v.clone() }] }),
            ] {
                assert_eq!(compact(&value).unwrap_err().message, message);
                assert_eq!(literal(&value).unwrap_err().message, message);
            }
            let doc = doc! { "_id": "abc", "nested": [{ "b": v }] };
            let e = document(&doc).unwrap_err();
            assert_eq!(e.message, format!("{message} (document _id: \"abc\")"));
        }
        assert!(compact(&Bson::DateTime(DateTime::from_millis(i64::MAX))).is_err());
    }

    #[test]
    fn the_type_decides_not_the_text() {
        assert_eq!(
            bits(&typed("double", "42").unwrap()),
            bits(&Bson::Double(42.0))
        );
        assert_eq!(bits(&typed("int64", "42").unwrap()), bits(&Bson::Int64(42)));
        assert_eq!(
            bits(&typed("decimal128", "42").unwrap()),
            bits(&Bson::Decimal128("42".parse().unwrap()))
        );
        assert_eq!(
            typed("string", " 42 ").unwrap(),
            Bson::String(" 42 ".into())
        );
        assert_eq!(typed("null", "whatever").unwrap(), Bson::Null);
        assert_eq!(
            bits(&typed("shell", "{a: 42.0}").unwrap()),
            bits(&Bson::Document(doc! { "a": 42.0 }))
        );
    }

    #[test]
    fn unknown_type_names_are_refused() {
        for ty in [
            "int", "object", "array", "missing", "regex", "binData", "Double", "",
        ] {
            let e = typed(ty, "1").unwrap_err();
            assert_eq!(e.message, format!("The grid cannot write BSON type {ty}"));
        }
    }

    #[test]
    fn invalid_texts_are_refused() {
        for (ty, text) in [
            ("int32", "2147483648"),
            ("int32", "1.5"),
            ("int32", ""),
            ("int64", "9223372036854775808"),
            ("int64", "abc"),
            ("double", "inf"),
            ("double", "nan"),
            ("double", "1,5"),
            ("double", ""),
            ("decimal128", "x"),
            ("bool", "yes"),
            ("bool", "True"),
            ("date", "2024-13-01T00:00:00Z"),
            ("date", "yesterday"),
            ("objectId", "123"),
            ("uuid", "xyz"),
            ("shell", "{a:"),
            ("shell", "1 2"),
            ("shell", ""),
            ("shell", "function () {}"),
        ] {
            assert!(typed(ty, text).is_err(), "{ty} {text:?}");
        }
        assert_eq!(
            typed("int32", "x").unwrap_err().message,
            "Invalid int32 value: x"
        );
    }

    #[test]
    fn the_wire_value_carries_type_and_text() {
        let wire = |v: serde_json::Value| serde_json::from_value::<TypedValue>(v).unwrap();
        assert_eq!(from_wire(&wire(json!({ "type": "missing" }))).unwrap(), None);
        assert_eq!(
            from_wire(&wire(json!({ "type": "null", "value": null }))).unwrap(),
            Some(Bson::Null)
        );
        assert_eq!(
            from_wire(&wire(json!({ "type": "int32", "value": "7" }))).unwrap(),
            Some(Bson::Int32(7))
        );
        let e = from_wire(&wire(json!({ "type": "int32" }))).unwrap_err();
        assert_eq!(e.message, "A int32 value needs a text");
        assert!(serde_json::from_value::<TypedValue>(json!({ "value": "7" })).is_err());
    }

    #[test]
    fn uuid_subtype_needs_sixteen_bytes_in_every_direction() {
        let short = Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: vec![1, 2, 3],
        });
        assert_eq!(type_name(&short), "binData");
        assert_eq!(cell(&short), json!({ "$bytes": "AQID", "len": 3 }));
        assert_eq!(compact(&short).unwrap(), "BinData(4, \"AQID\")");
        assert_eq!(type_alias(&short), "binData");
        // The full uuid is a uuid everywhere.
        assert_eq!(type_name(&uuid_value()), "uuid");
        assert_eq!(
            cell(&uuid_value()),
            json!("0f8fad5b-d9cb-469f-a165-70867728950e")
        );
        assert_eq!(
            compact(&uuid_value()).unwrap(),
            "UUID(\"0f8fad5b-d9cb-469f-a165-70867728950e\")"
        );
    }

    #[test]
    fn undefined_is_not_a_null_cell() {
        assert_eq!(type_name(&Bson::Undefined), "undefined");
        assert_eq!(cell(&Bson::Undefined), json!("undefined"));
        assert_eq!(type_alias(&Bson::Undefined), "undefined");
        assert!(compact(&Bson::Undefined).is_err());
        assert_eq!(csv(&Bson::Undefined), "undefined");
        // A real null still reads as an empty CSV field and a null cell.
        assert_eq!(cell(&Bson::Null), Value::Null);
        assert_eq!(csv(&Bson::Null), "");
    }

    #[test]
    fn csv_keeps_the_full_value() {
        let long = "x".repeat(5000);
        assert_eq!(csv(&Bson::String(long.clone())), long);
        assert_eq!(csv(&Bson::Symbol(long.clone())), long);
        assert_eq!(csv(&Bson::Array(vec![Bson::Int32(1)])), "[1]");
        assert_eq!(csv(&Bson::Document(doc! { "n": 1i32 })), "{\"n\":1}");
        let bytes = vec![0xde, 0xad];
        assert_eq!(
            csv(&Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes,
            })),
            "3q0="
        );
        assert_eq!(csv(&Bson::Int64(42)), "42");
        assert_eq!(csv(&Bson::Double(1.5)), "1.5");
        assert_eq!(csv(&Bson::Boolean(true)), "true");
    }

    #[test]
    fn cell_covers_every_bson_type() {
        let oid = ObjectId::from([1u8; 12]);
        let dec: Decimal128 = "12.34".parse().unwrap();
        let values = vec![
            (Bson::Double(1.5), json!(1.5)),
            (Bson::Double(f64::NAN), json!("NaN")),
            (Bson::Double(f64::INFINITY), json!("Infinity")),
            (Bson::Double(f64::NEG_INFINITY), json!("-Infinity")),
            (Bson::Int32(-2_147_483_648), json!(-2_147_483_648)),
            // Above 2^53 the grid shows a string; JS could not hold the number exactly.
            (
                Bson::Int64(9_007_199_254_740_993),
                json!("9007199254740993"),
            ),
            (Bson::Int64(42), json!(42)),
            (Bson::Decimal128(dec), json!("12.34")),
            (Bson::String("text".into()), json!("text")),
            (Bson::Symbol("sym".into()), json!("sym")),
            (Bson::Boolean(true), json!(true)),
            (Bson::Null, Value::Null),
            (Bson::Undefined, json!("undefined")),
            (Bson::ObjectId(oid), json!("010101010101010101010101")),
            (
                Bson::DateTime(DateTime::from_millis(1_700_000_000_000)),
                json!("2023-11-14T22:13:20Z"),
            ),
            (
                Bson::Timestamp(Timestamp {
                    time: 1,
                    increment: 2,
                }),
                json!("Timestamp(1, 2)"),
            ),
            (
                Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes: vec![0xaau8; 16],
                }),
                json!("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
            ),
            (
                Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![0xde, 0xad],
                }),
                json!({"$bytes": "3q0=", "len": 2}),
            ),
            (
                Bson::RegularExpression(Regex {
                    pattern: "abc".into(),
                    options: "i".into(),
                }),
                json!("/abc/i"),
            ),
            (Bson::JavaScriptCode("1 + 1".into()), json!("1 + 1")),
            (
                Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                    code: "1 + 1".into(),
                    scope: doc! { "x": 1i32 },
                }),
                json!("1 + 1"),
            ),
            (Bson::MinKey, json!("MinKey")),
            (Bson::MaxKey, json!("MaxKey")),
            (
                // DbPointer's fields are private; Extended JSON builds one.
                Bson::try_from(json!({
                    "$dbPointer": {
                        "$ref": "a.b",
                        "$id": { "$oid": "010101010101010101010101" }
                    }
                }))
                .expect("valid extjson DbPointer"),
                json!("DBPointer"),
            ),
            (Bson::Document(doc! { "n": 1i32 }), json!("{1 field}")),
            (Bson::Document(doc! {}), json!("{}")),
            (Bson::Array(vec![1i32.into(), 2.into()]), json!("[2 items]")),
            (Bson::Array(vec![1i32.into()]), json!("[1 item]")),
            (Bson::Array(vec![]), json!("[0 items]")),
        ];
        for (v, expected) in &values {
            assert_eq!(cell(v), *expected, "unexpected preview for {v:?}");
        }
    }

    #[test]
    fn long_strings_are_cut() {
        let long = "x".repeat(5000);
        let v = cell(&Bson::String(long.clone()));
        let s = v.as_str().unwrap();
        assert!(s.ends_with('…') && s.len() <= PREVIEW_BYTES + "…".len());
        assert!(long.starts_with(s.trim_end_matches('…')));
        // Symbol cuts the same way.
        assert_eq!(cell(&Bson::Symbol(long)), s);
    }

    #[test]
    fn dates_use_one_rule_for_the_cell_and_the_literal() {
        let iso = DateTime::from_millis(1_700_000_000_123);
        assert_eq!(cell(&Bson::DateTime(iso)), json!("2023-11-14T22:13:20.123Z"));
        assert_eq!(
            compact(&Bson::DateTime(iso)).unwrap(),
            "ISODate(\"2023-11-14T22:13:20.123Z\")"
        );
        let late = DateTime::from_millis(253_402_300_800_000);
        assert_eq!(cell(&Bson::DateTime(late)), json!("253402300800000"));
        assert_eq!(compact(&Bson::DateTime(late)).unwrap(), "new Date(253402300800000)");
        let too_exact = DateTime::from_millis(i64::MAX);
        assert_eq!(cell(&Bson::DateTime(too_exact)), json!(i64::MAX.to_string()));
        assert!(compact(&Bson::DateTime(too_exact)).is_err());
    }
}
