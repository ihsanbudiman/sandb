//! A parsed mongosh-style statement. `parse.rs` builds it from editor text, `guard.rs` classifies
//! it and `run.rs` runs it. The parser never runs JavaScript: every argument is a BSON value.

use mongodb::bson::{Bson, Document};

/// One statement of the editor text.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// The statement text as written, without the trailing `;`. Shown in the result header and
    /// used for a source rerun ("Load more", sort).
    pub text: String,
    /// Byte offset of `text` in the full editor text.
    pub start: usize,
    pub command: Command,
}

// One value per statement of a run, built once and read in place: boxing the large variants
// would only add allocations.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `show dbs` / `show databases`
    ShowDbs,
    /// `show collections` / `show tables`
    ShowCollections,
    /// `use <db>`: changes the database for the next statements of the same run only.
    Use(String),
    /// `db.runCommand({...})` (`admin: false`) or `db.adminCommand({...})` (`admin: true`).
    RunCommand { admin: bool, command: Document },
    /// `db.createCollection("name", {options})`
    CreateCollection {
        name: String,
        options: Option<Document>,
    },
    /// `db.dropDatabase({writeConcern})`: drops the database the statement runs on.
    DropDatabase { options: Option<Document> },
    /// `db.<coll>.<method>(...)` or `db.getCollection("<coll>").<method>(...)`
    Collection { collection: String, method: Method },
}

/// `find` with its chained cursor methods. A chain method given twice keeps the last value, as
/// mongosh does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Find {
    pub filter: Document,
    /// Second argument of `find`, or `.project()`.
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub limit: Option<i64>,
    pub skip: Option<u64>,
    /// Index name (string) or key document.
    pub hint: Option<Bson>,
    pub collation: Option<Document>,
    /// `.maxTimeMS(n)` at the end: the server stops the query after `n` milliseconds.
    pub max_time_ms: Option<u64>,
    /// `.count()` at the end: the result is one number, not documents.
    pub count: bool,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    // ---- reads
    Find(Find),
    FindOne {
        filter: Document,
        projection: Option<Document>,
    },
    Aggregate {
        pipeline: Vec<Document>,
        options: Option<Document>,
    },
    CountDocuments {
        filter: Document,
        options: Option<Document>,
    },
    EstimatedDocumentCount,
    Distinct {
        field: String,
        filter: Document,
    },
    GetIndexes,
    Stats,
    // ---- writes
    InsertOne {
        document: Document,
    },
    InsertMany {
        documents: Vec<Document>,
        options: Option<Document>,
    },
    /// `update` is a document (`{$set: …}`) or an array of pipeline stages.
    UpdateOne {
        filter: Document,
        update: Bson,
        options: Option<Document>,
    },
    UpdateMany {
        filter: Document,
        update: Bson,
        options: Option<Document>,
    },
    ReplaceOne {
        filter: Document,
        replacement: Document,
        options: Option<Document>,
    },
    DeleteOne {
        filter: Document,
    },
    DeleteMany {
        filter: Document,
    },
    FindOneAndUpdate {
        filter: Document,
        update: Bson,
        options: Option<Document>,
    },
    FindOneAndReplace {
        filter: Document,
        replacement: Document,
        options: Option<Document>,
    },
    FindOneAndDelete {
        filter: Document,
        options: Option<Document>,
    },
    /// Each operation is one mongosh model, e.g. `{ insertOne: { document: {...} } }`.
    BulkWrite {
        operations: Vec<Document>,
        options: Option<Document>,
    },
    // ---- schema changes
    CreateIndex {
        keys: Document,
        options: Option<Document>,
    },
    /// Each entry is a key document, as mongosh `createIndexes([keys, …], options)` takes.
    CreateIndexes {
        keys: Vec<Document>,
        options: Option<Document>,
    },
    /// Index name (string) or key document.
    DropIndex {
        index: Bson,
    },
    DropIndexes,
    Drop,
    RenameCollection {
        to: String,
        drop_target: bool,
    },
}

impl Method {
    /// The mongosh name, for messages and the result header.
    pub fn name(&self) -> &'static str {
        match self {
            Method::Find(_) => "find",
            Method::FindOne { .. } => "findOne",
            Method::Aggregate { .. } => "aggregate",
            Method::CountDocuments { .. } => "countDocuments",
            Method::EstimatedDocumentCount => "estimatedDocumentCount",
            Method::Distinct { .. } => "distinct",
            Method::GetIndexes => "getIndexes",
            Method::Stats => "stats",
            Method::InsertOne { .. } => "insertOne",
            Method::InsertMany { .. } => "insertMany",
            Method::UpdateOne { .. } => "updateOne",
            Method::UpdateMany { .. } => "updateMany",
            Method::ReplaceOne { .. } => "replaceOne",
            Method::DeleteOne { .. } => "deleteOne",
            Method::DeleteMany { .. } => "deleteMany",
            Method::FindOneAndUpdate { .. } => "findOneAndUpdate",
            Method::FindOneAndReplace { .. } => "findOneAndReplace",
            Method::FindOneAndDelete { .. } => "findOneAndDelete",
            Method::BulkWrite { .. } => "bulkWrite",
            Method::CreateIndex { .. } => "createIndex",
            Method::CreateIndexes { .. } => "createIndexes",
            Method::DropIndex { .. } => "dropIndex",
            Method::DropIndexes => "dropIndexes",
            Method::Drop => "drop",
            Method::RenameCollection { .. } => "renameCollection",
        }
    }
}
