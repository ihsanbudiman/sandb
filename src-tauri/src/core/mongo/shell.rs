//! BSON values as mongosh text: the grid's shell preview, the index definitions of the
//! properties view, and the script `export_table_sql` writes.
//!
//! Every value renders to text that `parse` reads back as the same BSON type and the same bits,
//! so an exported collection is the collection it came from. Types that have no constructor in
//! mongosh (Symbol, Undefined, DbPointer, CodeWithScope) are refused, never approximated.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Bson, DateTime, Document, Regex};
use uuid::Uuid;

use crate::core::error::AppError;
use crate::core::mongo::schema::type_name;

/// The largest date `new Date(ms)` reads back exactly: above 2^53 a JavaScript number loses
/// digits.
const MAX_EXACT_DOUBLE: u64 = 1 << 53;

/// `v` as mongosh text. Documents and arrays are indented by two spaces; a scalar is one line.
pub fn literal(v: &Bson) -> Result<String, AppError> {
    let mut out = String::new();
    write(v, false, 0, &mut out)?;
    Ok(out)
}

/// `v` as one line, for a definition or a script statement.
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
        Bson::Double(d) => out.push_str(&double(*d)),
        Bson::Decimal128(d) => out.push_str(&format!("NumberDecimal(\"{d}\")")),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null => out.push_str("null"),
        Bson::DateTime(d) => out.push_str(&date(*d)?),
        Bson::ObjectId(id) => out.push_str(&format!("ObjectId(\"{}\")", id.to_hex())),
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            // The length check makes `from_slice` infallible.
            let u = Uuid::from_slice(&b.bytes).expect("16 bytes");
            out.push_str(&format!("UUID(\"{u}\")"));
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
fn double(d: f64) -> String {
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

/// `ISODate("...")`, or the milliseconds when the date is outside the years 0 to 9999.
fn date(d: DateTime) -> Result<String, AppError> {
    if let Ok(s) = d.try_to_rfc3339_string() {
        return Ok(format!("ISODate(\"{s}\")"));
    }
    let ms = d.timestamp_millis();
    if ms.unsigned_abs() <= MAX_EXACT_DOUBLE {
        // The parser reads the milliseconds of `new Date(...)` as an int or a double.
        Ok(format!("new Date({ms})"))
    } else {
        Err(AppError::invalid(
            "Cannot write a date outside the range ISODate and new Date read back",
        ))
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

#[cfg(test)]
mod tests {
    use mongodb::bson::oid::ObjectId;
    use mongodb::bson::{Binary, Decimal128, JavaScriptCodeWithScope, Timestamp, doc};

    use super::*;
    use crate::core::mongo::parse;

    /// The BSON bytes of a value: equal bytes are the same type and the same bits (NaN too).
    fn bits(v: &Bson) -> Vec<u8> {
        let mut out = Vec::new();
        doc! { "v": v.clone() }.to_writer(&mut out).unwrap();
        out
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

    fn read_back(text: &str) -> Bson {
        parse::parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"))
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
        let pointer = Bson::try_from(serde_json::json!({
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
}
