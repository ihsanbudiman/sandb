//! Field summary of sampled documents. The caller streams documents into `FieldSummary::add`;
//! the summary keeps only field paths, the number of documents that have each path and the BSON
//! type counts per path. Paths are dot-separated (`a.b`); a field of a document inside an array
//! gets `items[].name`. The merge is lossy on purpose: it never stores a document or a value.

use std::collections::{HashMap, HashSet};

use mongodb::bson::{Bson, Document, spec::BinarySubtype};

use crate::core::db::introspect::ColumnInfo;

/// Full column browse: documents read per collection.
pub const SAMPLE_DOCS: i64 = 1000;
/// Full column browse: byte cap of the `$sample` stage.
pub const SAMPLE_BYTES: usize = 8 * 1024 * 1024;
/// Schema snapshot: smaller sample so the snapshot stays fast.
pub const SNAPSHOT_SAMPLE_DOCS: i64 = 100;
/// Schema snapshot: byte cap of the `$sample` stage.
pub const SNAPSHOT_SAMPLE_BYTES: usize = 1024 * 1024;
/// Deepest path kept: `a.b.c.d.e` has depth 5. A document at that depth still shows as `object`.
pub const MAX_DEPTH: usize = 5;

/// Counts for one path: how many documents have the path and how many have each BSON type.
#[derive(Default)]
struct FieldStats {
    docs: u32,
    types: HashMap<String, u32>,
}

#[derive(Default)]
pub struct FieldSummary {
    docs: u32,
    /// Paths in the order they were first seen, so `into_columns` keeps document key order.
    order: Vec<String>,
    stats: HashMap<String, FieldStats>,
}

impl FieldSummary {
    /// Merges one sampled document. Keeps no reference to it.
    pub fn add(&mut self, doc: &Document) {
        self.docs += 1;
        // One element can repeat a path and add new types; count each (path, type) at most once
        // per document.
        let mut seen: HashMap<String, HashSet<&'static str>> = HashMap::new();
        self.merge_document(doc, "", 1, &mut seen);
    }

    /// Merges the `$jsonSchema` of a collection validator (the `options.validator` document).
    /// Paths the sample did not have appear with count 0; sample counts never change.
    /// No-op without `$jsonSchema`.
    pub fn add_validator(&mut self, validator: &Document) {
        let Some(Bson::Document(schema)) = validator.get("$jsonSchema") else {
            return;
        };
        if let Some(Bson::Document(properties)) = schema.get("properties") {
            self.merge_properties(properties, "", 1);
        }
    }

    /// Documents merged so far.
    pub fn docs(&self) -> u32 {
        self.docs
    }

    pub fn into_columns(self) -> Vec<ColumnInfo> {
        let FieldSummary { docs, order, stats } = self;
        let mut columns: Vec<ColumnInfo> = order
            .into_iter()
            .filter_map(|name| {
                let field = stats.get(&name)?;
                let mut types: Vec<(&String, &u32)> = field.types.iter().collect();
                types.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                let data_type = types
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                Some(ColumnInfo {
                    is_pk: name == "_id",
                    pk_ordinal: (name == "_id").then_some(1),
                    nullable: field.docs < docs || field.types.contains_key("null"),
                    frequency: Some(field.docs as f64 / f64::from(docs.max(1))),
                    sample_size: Some(docs),
                    name,
                    data_type,
                    default: None,
                })
            })
            .collect();
        // `_id` always leads, also when a later document or the validator saw it first.
        if let Some(i) = columns.iter().position(|c| c.name == "_id") {
            let id = columns.remove(i);
            columns.insert(0, id);
        }
        columns
    }

    /// Records `(path, type)` for the current document and descends into embedded documents and
    /// arrays of documents. `depth` is the number of segments of the keys of `doc` (a top-level
    /// key has depth 1, an array element adds none).
    fn merge_document(
        &mut self,
        doc: &Document,
        prefix: &str,
        depth: usize,
        seen: &mut HashMap<String, HashSet<&'static str>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        for (key, value) in doc {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            let first_for_path = !seen.contains_key(&path);
            if seen
                .entry(path.clone())
                .or_default()
                .insert(type_name(value))
            {
                let field = self.stats_mut(path.clone());
                if first_for_path {
                    field.docs += 1;
                }
                *field.types.entry(type_name(value).to_string()).or_insert(0) += 1;
            }
            match value {
                Bson::Document(embedded) => {
                    self.merge_document(embedded, &path, depth + 1, seen);
                }
                Bson::Array(elements) => {
                    // Scalar element types are ignored; element documents merge under `path[]`.
                    let nested = format!("{path}[]");
                    for element in elements {
                        if let Bson::Document(embedded) = element {
                            self.merge_document(embedded, &nested, depth + 1, seen);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Walks the `properties` of a `$jsonSchema`. `items` with `properties` extends the path with
    /// `[]` without adding depth. Types the sample already counted keep their counts.
    fn merge_properties(&mut self, properties: &Document, prefix: &str, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for (key, schema) in properties {
            let Bson::Document(prop) = schema else {
                continue;
            };
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            for name in schema_bson_types(prop) {
                let field = self.stats_mut(path.clone());
                field.types.entry(name.to_string()).or_insert(0);
            }
            if let Some(Bson::Document(nested)) = prop.get("properties") {
                self.merge_properties(nested, &path, depth + 1);
            }
            if let Some(Bson::Document(items)) = prop.get("items") {
                let nested = format!("{path}[]");
                if let Some(Bson::Document(item_props)) = items.get("properties") {
                    self.merge_properties(item_props, &nested, depth + 1);
                }
            }
        }
    }

    /// Stats of `path`, registering it in first-seen order on first use.
    fn stats_mut(&mut self, path: String) -> &mut FieldStats {
        if !self.stats.contains_key(&path) {
            self.order.push(path.clone());
        }
        self.stats.entry(path).or_default()
    }
}

/// BSON type name as shown in the UI, e.g. "int32", "objectId".
pub fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        // Only the standard UUID subtype (0x04) is a uuid; the legacy subtype stays binData.
        Bson::Binary(binary) if binary.subtype == BinarySubtype::Uuid => "uuid",
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

/// The `bsonType` names of one property schema, as a string or an array. JSON schema names map to
/// the BSON names the sample uses; unknown names pass through unchanged.
fn schema_bson_types(prop: &Document) -> Vec<&str> {
    let mut names = Vec::new();
    match prop.get("bsonType") {
        Some(Bson::String(name)) => names.push(validator_type(name)),
        Some(Bson::Array(list)) => {
            names.extend(list.iter().filter_map(|v| match v {
                Bson::String(name) => Some(validator_type(name)),
                _ => None,
            }));
        }
        _ => {}
    }
    names
}

fn validator_type(name: &str) -> &str {
    match name {
        "int" => "int32",
        "long" => "int64",
        "decimal" => "decimal128",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{Binary, doc};

    fn column<'a>(columns: &'a [ColumnInfo], name: &str) -> &'a ColumnInfo {
        columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    }

    #[test]
    fn nested_document_paths() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "_id": 1, "a": { "b": 1i32, "c": { "d": "x" } } });
        let columns = summary.into_columns();
        assert_eq!(
            columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["_id", "a", "a.b", "a.c", "a.c.d"]
        );
        assert_eq!(column(&columns, "a").data_type, "object");
        assert_eq!(column(&columns, "a.b").data_type, "int32");
        assert_eq!(column(&columns, "a.c.d").data_type, "string");
    }

    #[test]
    fn array_elements_count_once_per_document() {
        let mut summary = FieldSummary::default();
        // Two elements with the same path: still one document that has `items[].sku`.
        summary.add(&doc! { "items": [{ "sku": "a" }, { "sku": "b", "qty": 1i32 }] });
        summary.add(&doc! { "items": [] });
        let columns = summary.into_columns();
        let sku = column(&columns, "items[].sku");
        assert_eq!(sku.data_type, "string");
        assert_eq!(sku.frequency, Some(0.5));
        assert!(sku.nullable);
        assert_eq!(column(&columns, "items").data_type, "array");
        assert_eq!(column(&columns, "items").frequency, Some(1.0));
        assert_eq!(column(&columns, "items[].qty").data_type, "int32");
        assert_eq!(column(&columns, "items[].qty").frequency, Some(0.5));
    }

    #[test]
    fn array_of_scalars_adds_no_element_paths() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "tags": ["a", "b"], "nums": [1i32, 2i64] });
        let columns = summary.into_columns();
        assert_eq!(columns.len(), 2);
        assert_eq!(column(&columns, "tags").data_type, "array");
        assert_eq!(column(&columns, "nums").data_type, "array");
    }

    #[test]
    fn mixed_types_sort_by_count_then_name() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "price": 1i32 });
        summary.add(&doc! { "price": 2i32 });
        summary.add(&doc! { "price": "cheap" });
        let columns = summary.into_columns();
        let price = column(&columns, "price");
        assert_eq!(price.data_type, "int32 | string");
        assert_eq!(price.frequency, Some(1.0));
        assert!(!price.nullable);
        assert_eq!(price.sample_size, Some(3));
    }

    #[test]
    fn missing_field_is_nullable_with_partial_frequency() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "_id": 1, "name": "x" });
        summary.add(&doc! { "_id": 2 });
        let columns = summary.into_columns();
        let name = column(&columns, "name");
        assert!(name.nullable);
        assert_eq!(name.frequency, Some(0.5));
        assert!(!column(&columns, "_id").nullable);
    }

    #[test]
    fn null_value_marks_nullable_and_lists_null() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "note": Bson::Null });
        summary.add(&doc! { "note": "text" });
        let columns = summary.into_columns();
        let note = column(&columns, "note");
        assert_eq!(note.data_type, "null | string");
        assert!(note.nullable);
        assert_eq!(note.frequency, Some(1.0), "present in every document");
    }

    #[test]
    fn paths_stop_at_max_depth() {
        // Keys of `g` would reach depth 7; only `a` to `a.b.c.d.e` (depth 5) are kept.
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "a": { "b": { "c": { "d": { "e": { "f": { "g": 1i32 } } } } } } });
        let columns = summary.into_columns();
        assert_eq!(
            columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "a.b", "a.b.c", "a.b.c.d", "a.b.c.d.e"]
        );
        assert_eq!(column(&columns, "a.b.c.d.e").data_type, "object");
        assert_eq!(MAX_DEPTH, 5);
    }

    #[test]
    fn id_column_leads_and_is_pk_regardless_of_key_order() {
        let mut summary = FieldSummary::default();
        // `_id` is not the first key of the document.
        summary.add(&doc! { "name": "x", "_id": 1, "price": 2i32 });
        let columns = summary.into_columns();
        let id = &columns[0];
        assert_eq!(id.name, "_id");
        assert!(id.is_pk);
        assert_eq!(id.pk_ordinal, Some(1));
        // No other column is a key, not even a nested `_id`.
        assert!(
            columns[1..]
                .iter()
                .all(|c| !c.is_pk && c.pk_ordinal.is_none())
        );
        assert_eq!(columns[1].name, "name");
        assert_eq!(columns[2].name, "price");
    }

    #[test]
    fn nested_id_is_not_pk() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "owner": { "_id": 1i32 } });
        let columns = summary.into_columns();
        assert!(!column(&columns, "owner._id").is_pk);
        assert_eq!(column(&columns, "owner._id").pk_ordinal, None);
    }

    #[test]
    fn validator_adds_unseen_paths_and_maps_int() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "name": "x" });
        summary.add_validator(&doc! {
            "$jsonSchema": {
                "bsonType": "object",
                "properties": {
                    "name": { "bsonType": "string" },
                    "age": { "bsonType": "int" },
                    "tags": { "bsonType": ["array", "null"] },
                    "addr": {
                        "bsonType": "object",
                        "properties": { "zip": { "bsonType": "long" } }
                    }
                }
            }
        });
        let columns = summary.into_columns();
        let age = column(&columns, "age");
        assert_eq!(age.data_type, "int32");
        assert_eq!(age.frequency, Some(0.0));
        assert!(age.nullable);
        assert_eq!(column(&columns, "name").data_type, "string");
        assert_eq!(column(&columns, "name").frequency, Some(1.0));
        assert_eq!(column(&columns, "tags").data_type, "array | null");
        assert_eq!(column(&columns, "addr.zip").data_type, "int64");
        // `addr` itself appears because the validator names its type.
        assert_eq!(column(&columns, "addr").data_type, "object");
    }

    #[test]
    fn validator_without_json_schema_is_noop() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! { "name": "x" });
        summary.add_validator(&doc! { "other": { "bsonType": "int" } });
        summary.add_validator(&Document::new());
        assert_eq!(summary.docs(), 1);
        assert_eq!(summary.into_columns().len(), 1);
    }

    #[test]
    fn uuid_subtype_is_uuid_other_subtypes_are_bindata() {
        let uuid = Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: vec![0; 16],
        });
        let legacy = Bson::Binary(Binary {
            subtype: BinarySubtype::UuidOld,
            bytes: vec![0; 16],
        });
        let generic = Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![0; 4],
        });
        assert_eq!(type_name(&uuid), "uuid");
        assert_eq!(type_name(&legacy), "binData");
        assert_eq!(type_name(&generic), "binData");
    }

    #[test]
    fn type_names_cover_the_bson_kinds() {
        use mongodb::bson::oid::ObjectId;
        use mongodb::bson::{DateTime, Decimal128, JavaScriptCodeWithScope, Regex, Timestamp};
        assert_eq!(type_name(&Bson::Double(1.0)), "double");
        assert_eq!(type_name(&Bson::ObjectId(ObjectId::new())), "objectId");
        assert_eq!(type_name(&Bson::Boolean(true)), "bool");
        assert_eq!(type_name(&Bson::DateTime(DateTime::now())), "date");
        assert_eq!(type_name(&Bson::Undefined), "undefined");
        assert_eq!(
            type_name(&Bson::RegularExpression(Regex {
                pattern: "x".into(),
                options: String::new()
            })),
            "regex"
        );
        assert_eq!(type_name(&Bson::JavaScriptCode("x".into())), "javascript");
        assert_eq!(type_name(&Bson::Symbol("x".into())), "symbol");
        assert_eq!(
            type_name(&Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code: "x".into(),
                scope: Document::new()
            })),
            "javascriptWithScope"
        );
        assert_eq!(type_name(&Bson::Int64(1)), "int64");
        assert_eq!(
            type_name(&Bson::Timestamp(Timestamp {
                time: 0,
                increment: 0
            })),
            "timestamp"
        );
        assert_eq!(
            type_name(&Bson::Decimal128(Decimal128::from_bytes([0; 16]))),
            "decimal128"
        );
        assert_eq!(type_name(&Bson::MinKey), "minKey");
        assert_eq!(type_name(&Bson::MaxKey), "maxKey");
    }
}
