//! Server reply shapes shared by the browse, read, write and export paths: the cursor batch, the
//! one-row replies of a count and a distinct, and the collection kind.

use mongodb::bson::{Bson, Document, doc};
use mongodb::results::CollectionType;

use crate::core::result::{MongoCollectionKind, TableKind};

pub fn cursor_batch(reply: &Document) -> Option<Vec<Document>> {
    let batch = reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .ok()?;
    Some(
        batch
            .iter()
            .filter_map(|value| value.as_document().cloned())
            .collect(),
    )
}

pub fn reply_docs(reply: &Document) -> Vec<Document> {
    cursor_batch(reply).unwrap_or_else(|| vec![reply.clone()])
}

pub fn count_row(n: u64) -> Document {
    doc! { "count": i64::try_from(n).unwrap_or(i64::MAX) }
}

pub fn value_row(value: Bson) -> Document {
    doc! { "value": value }
}

pub fn driver_kind(collection_type: CollectionType) -> MongoCollectionKind {
    match collection_type {
        CollectionType::View => MongoCollectionKind::View,
        CollectionType::Timeseries => MongoCollectionKind::Timeseries,
        // `Collection`, and any kind a newer driver adds: both list as a table.
        _ => MongoCollectionKind::Collection,
    }
}

pub fn named_kind(ty: Option<&str>) -> MongoCollectionKind {
    match ty {
        Some("view") => MongoCollectionKind::View,
        Some("timeseries") => MongoCollectionKind::Timeseries,
        _ => MongoCollectionKind::Collection,
    }
}

pub fn kind_label(kind: MongoCollectionKind) -> &'static str {
    match kind {
        MongoCollectionKind::Collection => "collection",
        MongoCollectionKind::View => "view",
        MongoCollectionKind::Timeseries => "timeseries",
    }
}

/// A timeseries collection is a table.
pub fn table_kind(kind: MongoCollectionKind) -> TableKind {
    match kind {
        MongoCollectionKind::View => TableKind::View,
        _ => TableKind::Table,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_reply_keeps_its_first_batch_and_a_plain_reply_is_one_document() {
        let reply = doc! {
            "cursor": { "firstBatch": [ { "name": "a" }, { "name": "b" } ], "id": 0i64 },
            "ok": 1.0,
        };
        assert_eq!(cursor_batch(&reply).unwrap().len(), 2);
        assert_eq!(reply_docs(&reply).len(), 2);
        let plain = doc! { "count": 3i64, "ok": 1.0 };
        assert_eq!(cursor_batch(&plain), None);
        assert_eq!(reply_docs(&plain), vec![plain.clone()]);
    }

    #[test]
    fn one_kind_map_covers_the_driver_field_and_the_raw_type_field() {
        assert_eq!(
            driver_kind(CollectionType::View),
            MongoCollectionKind::View
        );
        assert_eq!(
            driver_kind(CollectionType::Collection),
            MongoCollectionKind::Collection
        );
        assert_eq!(
            driver_kind(CollectionType::Timeseries),
            MongoCollectionKind::Timeseries
        );
        for (ty, kind) in [
            (Some("view"), MongoCollectionKind::View),
            (Some("timeseries"), MongoCollectionKind::Timeseries),
            (Some("collection"), MongoCollectionKind::Collection),
            (None, MongoCollectionKind::Collection),
        ] {
            assert_eq!(named_kind(ty), kind);
        }
        assert_eq!(kind_label(MongoCollectionKind::View), "view");
        assert_eq!(kind_label(MongoCollectionKind::Timeseries), "timeseries");
        assert_eq!(kind_label(MongoCollectionKind::Collection), "collection");
        assert_eq!(table_kind(MongoCollectionKind::View), TableKind::View);
        assert_eq!(
            table_kind(MongoCollectionKind::Timeseries),
            TableKind::Table
        );
    }
}
