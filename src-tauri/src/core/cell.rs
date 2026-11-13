//! How one value becomes a JSON cell. Shared by the SQL decoders and the MongoDB flattener, so
//! the grid, the AI read result, the CSV export and the Mongo previews agree on every rule.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Number, Value, json};

/// Bytes shown per binary or text cell in the grid. The SQL decoders pick this or the full
/// value through `db::value::View`; MongoDB passes it explicitly.
pub const PREVIEW_BYTES: usize = 4096;

/// Largest integer a JS number holds exactly (2^53 - 1).
const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;

pub fn bytes(b: &[u8], cap: Option<usize>) -> Value {
    let shown = match cap {
        Some(c) => &b[..b.len().min(c)],
        None => b,
    };
    json!({"$bytes": STANDARD.encode(shown), "len": b.len()})
}

pub fn int(i: i64) -> Value {
    if i.unsigned_abs() <= MAX_SAFE_INT {
        Value::from(i)
    } else {
        Value::String(i.to_string())
    }
}

/// NaN and infinities have no JSON number form; they become strings.
pub fn float(f: f64) -> Value {
    Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(f.to_string()))
}

pub fn unsupported(type_name: &str) -> Value {
    Value::String(format!("<unsupported: {type_name}>"))
}

pub fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

/// Text cut to at most `cap` bytes on a char boundary, with `…` when cut. Geometry as WKT or
/// hex EWKB runs to megabytes per cell; sending it whole to the grid freezes the UI.
pub fn text_capped(s: &str, cap: Option<usize>) -> Value {
    match cap {
        Some(c) if s.len() > c => {
            let end = (0..=c).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
            Value::String(format!("{}…", &s[..end]))
        }
        _ => text(s),
    }
}

/// RFC 4180: quote when the field has a comma, quote, CR or LF; double inner quotes.
pub fn escape(field: &str) -> std::borrow::Cow<'_, str> {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\"")).into()
    } else {
        field.into()
    }
}

/// One CSV field of a JSON cell: NULL → empty, bytes → base64, JSON values → compact JSON,
/// the rest → their text.
pub fn field(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(o) => match o.get("$bytes") {
            Some(Value::String(b64)) if o.contains_key("len") => b64.clone(),
            _ => v.to_string(),
        },
        Value::Array(_) => v.to_string(),
    }
}

/// One CSV row, CRLF-terminated.
pub fn line<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut s = fields.into_iter().map(escape).collect::<Vec<_>>().join(",");
    s.push_str("\r\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_stay_exact() {
        assert_eq!(int(42), json!(42));
        assert_eq!(int(i64::MAX), json!(i64::MAX.to_string()));
    }

    #[test]
    fn text_is_cut_on_a_char_boundary() {
        let s = "日本語";
        let cut = text_capped(s, Some(4));
        assert_eq!(cut, json!("日…"));
        assert_eq!(text_capped(s, Some(PREVIEW_BYTES)), json!(s));
    }

    #[test]
    fn floats_without_a_json_form_become_strings() {
        assert_eq!(float(1.5), json!(1.5));
        assert_eq!(float(f64::NAN), json!("NaN"));
        assert_eq!(float(f64::INFINITY), json!("inf"));
    }

    #[test]
    fn csv_quoting_follows_rfc4180() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(line(["a", "b,c", ""]), "a,\"b,c\",\r\n");
        assert_eq!(field(&Value::Null), "");
        assert_eq!(field(&json!({"a": [1, 2]})), r#"{"a":[1,2]}"#);
        assert_eq!(field(&json!({"$bytes": "3q2+7w==", "len": 4})), "3q2+7w==");
    }

    #[test]
    fn bytes_report_the_full_length() {
        assert_eq!(
            bytes(&[1, 2, 3, 4], Some(2)),
            json!({"$bytes": "AQI=", "len": 4})
        );
    }
}
