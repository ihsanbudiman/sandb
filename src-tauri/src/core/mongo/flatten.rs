//! Flattens MongoDB documents into grid columns and preview cells.
//!
//! Top-level keys become columns. A nested sub-document becomes child columns (`a.b`) while the
//! whole document fits inside [`FLATTEN_DEPTH`]; a sub-document at the limit collapses into one
//! summary cell (`{N fields}`). Arrays always stay one cell (`[N items]`). Column order follows
//! first appearance across documents.

use std::collections::HashMap;

use mongodb::bson::{Bson, Document};
use serde_json::Value;

use crate::core::mongo::schema::type_labels;
use crate::core::mongo::value::{cell, type_name};
use crate::core::result::ColumnMeta;

/// Deepest flattened column: `a.b.c`. A document at that depth is one summary cell.
pub const FLATTEN_DEPTH: usize = 3;

pub struct Page {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub cell_types: Vec<Vec<&'static str>>,
    pub documents: Vec<Value>,
}

struct Cell {
    column: usize,
    value: Value,
    ty: &'static str,
}

#[derive(Default)]
pub struct Flattener {
    columns: Vec<ColumnMeta>,
    index: HashMap<String, usize>,
    rows: Vec<Vec<Cell>>,
    documents: Vec<Value>,
}

impl Flattener {
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
                meta.mixed = counts.len() > 1;
                meta.data_type = type_labels(counts);
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

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{DateTime, doc, oid::ObjectId};
    use serde_json::json;

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
