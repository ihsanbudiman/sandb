//! Flattens MongoDB documents into grid columns and preview cells.
//!
//! Top-level keys become columns. A nested sub-document becomes child columns (`a.b`) while the
//! whole document fits inside [`FLATTEN_DEPTH`]; a sub-document at the limit collapses into one
//! summary cell (`{N fields}`). Arrays always stay one cell (`[N items]`). Column order follows
//! first appearance across documents.

use std::collections::HashMap;

use mongodb::bson::{Bson, Document, spec::BinarySubtype};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::core::db::query::ColumnMeta;
use crate::core::db::value::{self, PREVIEW_BYTES};
use crate::core::mongo::schema::type_name;

/// Deepest flattened column: `a.b.c`. A document at that depth is one summary cell.
pub const FLATTEN_DEPTH: usize = 3;

/// One result set, ready for the grid: flattened columns, preview rows, per-cell BSON types and
/// the relaxed Extended JSON documents behind the rows.
pub struct Page {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub cell_types: Vec<Vec<&'static str>>,
    pub documents: Vec<Value>,
}

/// One preview cell: the column it belongs to, its JSON value and its BSON type name.
struct Cell {
    column: usize,
    value: Value,
    ty: &'static str,
}

/// Accumulates documents into columns and rows.
#[derive(Default)]
pub struct Flattener {
    columns: Vec<ColumnMeta>,
    index: HashMap<String, usize>,
    rows: Vec<Vec<Cell>>,
    documents: Vec<Value>,
}

impl Flattener {
    /// Adds one document as the next row. Takes it by value: it becomes `documents[i]`.
    pub fn push(&mut self, doc: Document) {
        let mut cells = Vec::new();
        for_each_cell(&doc, &mut |path, value| {
            self.add_cell(path, value, &mut cells)
        });
        self.rows.push(cells);
        self.documents
            .push(Bson::Document(doc).into_relaxed_extjson());
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Pads earlier rows to the final column count (null / "missing").
    pub fn finish(self) -> Page {
        let Self {
            columns,
            index: _,
            rows,
            documents,
        } = self;
        let width = columns.len();

        let mut counts: Vec<HashMap<&'static str, u32>> = vec![HashMap::new(); width];
        let mut out_rows = Vec::with_capacity(rows.len());
        let mut out_types = Vec::with_capacity(rows.len());
        for cells in rows {
            let mut values = vec![Value::Null; width];
            let mut types = vec!["missing"; width];
            for c in cells {
                *counts[c.column].entry(c.ty).or_insert(0) += 1;
                values[c.column] = c.value;
                types[c.column] = c.ty;
            }
            out_rows.push(values);
            out_types.push(types);
        }

        let columns = columns
            .into_iter()
            .zip(counts)
            .map(|(mut meta, counts)| {
                // Most frequent type first, ties by name, joined with ` | `.
                let mut names: Vec<(&'static str, u32)> = counts.into_iter().collect();
                names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                meta.mixed = names.len() > 1;
                meta.data_type = names
                    .into_iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>()
                    .join(" | ");
                meta
            })
            .collect();

        Page {
            columns,
            rows: out_rows,
            cell_types: out_types,
            documents,
        }
    }

    /// Places one value into the row: a scalar, an array (`[N items]`), an empty document (`{}`)
    /// or a summary cell (`{N fields}`) at the depth limit.
    fn add_cell(&mut self, path: &str, value: &Bson, cells: &mut Vec<Cell>) {
        let column = match self.index.get(path) {
            Some(&i) => i,
            None => {
                let i = self.columns.len();
                self.columns.push(ColumnMeta {
                    name: path.to_string(),
                    data_type: String::new(),
                    mixed: false,
                });
                self.index.insert(path.to_string(), i);
                i
            }
        };
        cells.push(Cell {
            column,
            value: cell(value),
            ty: type_name(value),
        });
    }
}

/// Calls `f` with the column path and value of every cell of `doc`, by the grid's rules: nested
/// sub-documents become child paths while the depth allows it. The CSV export uses the same walk.
pub fn for_each_cell(doc: &Document, f: &mut impl FnMut(&str, &Bson)) {
    visit(doc, "", 0, f);
}

fn visit(doc: &Document, prefix: &str, depth: usize, f: &mut impl FnMut(&str, &Bson)) {
    for (key, value) in doc {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Bson::Document(child) if depth + 1 < FLATTEN_DEPTH && !child.is_empty() => {
                visit(child, &path, depth + 1, f);
            }
            _ => f(&path, value),
        }
    }
}

/// The noun part of a summary cell: `3 items` / `1 item`.
fn summary(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Preview value of one BSON value for a grid cell.
pub fn cell(v: &Bson) -> Value {
    match v {
        Bson::Double(d) => match *d {
            d if d.is_nan() => json!("NaN"),
            f64::INFINITY => json!("Infinity"),
            f64::NEG_INFINITY => json!("-Infinity"),
            d => json!(d),
        },
        Bson::Int32(i) => json!(i),
        Bson::Int64(i) => value::int(*i),
        Bson::Decimal128(d) => json!(d.to_string()),
        Bson::String(s) | Bson::Symbol(s) => value::text_capped(s, Some(PREVIEW_BYTES)),
        Bson::Boolean(b) => json!(b),
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::ObjectId(id) => json!(id.to_hex()),
        // An empty document shows as `{}`, a non-empty one as `{N fields}`; arrays keep
        // `[N items]` even when empty.
        Bson::Document(d) if d.is_empty() => json!("{}"),
        Bson::Document(d) => json!(format!("{{{}}}", summary(d.len(), "field", "fields"))),
        Bson::Array(a) => json!(format!("[{}]", summary(a.len(), "item", "items"))),
        Bson::DateTime(dt) => match dt.try_to_rfc3339_string() {
            Ok(s) => json!(s),
            // Out-of-representation dates fall back to their milliseconds.
            Err(_) => json!(dt.timestamp_millis().to_string()),
        },
        Bson::Timestamp(ts) => json!(format!("Timestamp({}, {})", ts.time, ts.increment)),
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            // The length check makes `from_slice` infallible.
            json!(Uuid::from_slice(&b.bytes).expect("16 bytes").to_string())
        }
        Bson::Binary(b) => value::bytes(&b.bytes, Some(PREVIEW_BYTES)),
        Bson::RegularExpression(re) => json!(format!("/{}/{}", re.pattern, re.options)),
        Bson::JavaScriptCode(code) => value::text_capped(code, Some(PREVIEW_BYTES)),
        Bson::JavaScriptCodeWithScope(code) => value::text_capped(&code.code, Some(PREVIEW_BYTES)),
        Bson::MinKey => json!("MinKey"),
        Bson::MaxKey => json!("MaxKey"),
        Bson::DbPointer(_) => json!("DBPointer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{
        Binary, DateTime, Decimal128, JavaScriptCodeWithScope, Regex, Timestamp, doc, oid::ObjectId,
    };

    fn flattener() -> Flattener {
        Flattener::default()
    }

    #[test]
    fn depth_limit_leaf_and_summary() {
        let mut f = flattener();
        f.push(doc! { "a": { "b": { "c": 1i32 } } });
        f.push(doc! { "a": { "b": { "c": { "d": 1i32 } } } });
        let page = f.finish();
        assert_eq!(
            page.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a.b.c"]
        );
        assert_eq!(page.rows[0], vec![json!(1)]);
        assert_eq!(page.rows[1], vec![json!("{1 field}")]);
        assert_eq!(page.cell_types[0], vec!["int32"]);
        assert_eq!(page.cell_types[1], vec!["object"]);
    }

    #[test]
    fn arrays_are_one_cell() {
        let mut f = flattener();
        f.push(doc! { "a": [1i32, 2, 3] });
        f.push(doc! { "a": [] });
        let page = f.finish();
        assert_eq!(page.rows[0], vec![json!("[3 items]")]);
        assert_eq!(page.rows[1], vec![json!("[0 items]")]);
        assert_eq!(page.cell_types[0], vec!["array"]);
        assert_eq!(page.cell_types[1], vec!["array"]);
    }

    #[test]
    fn empty_document_is_one_cell() {
        let mut f = flattener();
        f.push(doc! { "a": {} });
        let page = f.finish();
        assert_eq!(page.rows[0], vec![json!("{}")]);
        assert_eq!(page.cell_types[0], vec!["object"]);
    }

    #[test]
    fn missing_fields_are_padded() {
        let mut f = flattener();
        f.push(doc! { "a": 1i32, "b": 2i32 });
        f.push(doc! { "a": 3i32 });
        let page = f.finish();
        assert_eq!(page.rows[0], vec![json!(1), json!(2)]);
        assert_eq!(page.rows[1], vec![json!(3), Value::Null]);
        assert_eq!(page.cell_types[1], vec!["int32", "missing"]);
    }

    #[test]
    fn column_order_follows_first_appearance() {
        let mut f = flattener();
        f.push(doc! { "a": 1i32, "b": 2i32 });
        f.push(doc! { "c": 3i32, "a": 4i32 });
        let page = f.finish();
        assert_eq!(
            page.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(page.rows[0], vec![json!(1), json!(2), Value::Null]);
        assert_eq!(page.rows[1], vec![json!(4), Value::Null, json!(3)]);
    }

    #[test]
    fn mixed_scalar_and_document_paths() {
        let mut f = flattener();
        f.push(doc! { "a": 1i32 });
        f.push(doc! { "a": { "b": "x" } });
        let page = f.finish();
        assert_eq!(
            page.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "a.b"]
        );
        assert_eq!(page.rows[0], vec![json!(1), Value::Null]);
        assert_eq!(page.rows[1], vec![Value::Null, json!("x")]);
        assert_eq!(page.cell_types[0], vec!["int32", "missing"]);
        assert_eq!(page.cell_types[1], vec!["missing", "string"]);
    }

    #[test]
    fn data_type_orders_most_frequent_first_then_by_name() {
        let mut f = flattener();
        f.push(doc! { "a": 1i32 });
        f.push(doc! { "a": "x" });
        f.push(doc! { "a": 2i32 });
        f.push(doc! { "a": 1.5 });
        f.push(doc! { "a": 3i32 });
        f.push(doc! { "a": 2.5 });
        f.push(doc! { "a": true });
        let page = f.finish();
        assert_eq!(page.columns[0].data_type, "int32 | double | bool | string");
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
            (Bson::Undefined, Value::Null),
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
    fn relaxed_ejson_of_date_and_object_id() {
        let mut f = flattener();
        f.push(doc! {
            "_id": ObjectId::from([0x64u8; 12]),
            "when": DateTime::from_millis(1_700_000_000_000),
        });
        let page = f.finish();
        assert_eq!(
            page.documents[0],
            json!({
                "_id": { "$oid": "646464646464646464646464" },
                "when": { "$date": "2023-11-14T22:13:20Z" },
            })
        );
    }
}
