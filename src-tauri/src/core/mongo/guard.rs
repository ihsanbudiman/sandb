//! Classifies parsed MongoDB statements: read, write, schema change or unknown. The guard
//! fails closed, like `conn_ro.rs` does for SQL: a statement it cannot prove harmless — a
//! `runCommand` off the allow-list, JavaScript in any argument — is never a read, so a
//! read-only connection refuses it.

use mongodb::bson::{Bson, Document};

use crate::core::connections::{read_only_refusal, Connection};
use crate::core::error::AppError;
use crate::core::result::{GuardCheck, WarningCode, WriteInfo, WriteStatement, WriteWarning};
use crate::core::mongo::command::{Command, Method, Statement};
use crate::core::mongo::parse;

/// How a statement can touch the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
    Schema,
    /// Everything the guard cannot vouch for, so everything a read-only connection refuses.
    Unknown,
}

/// `runCommand` / `adminCommand` first keys that change nothing. Case-sensitive on purpose:
/// the server matches the command name exactly, so `{ Ping: 1 }` is not `ping`. `explain` is
/// checked apart, by its inner command (`explain_reads`). Only commands that stay read-only
/// whatever their arguments belong here: `profile` sets the profiling level, so it does not.
const READ_COMMANDS: &[&str] = &[
    "ping",
    "buildInfo",
    "hello",
    "isMaster",
    "serverStatus",
    "hostInfo",
    "connectionStatus",
    "dbStats",
    "collStats",
    "listDatabases",
    "listCollections",
    "listIndexes",
    "getParameter",
    "getCmdLineOpts",
    "currentOp",
    "connPoolStats",
    "top",
    "listCommands",
    "whatsmyuri",
];

/// Operators that run JavaScript on the server and so can do anything. They hide in any
/// argument of any command, so every document and array is searched at any depth.
const JAVASCRIPT_KEYS: &[&str] = &["$where", "$function", "$accumulator"];

/// Aggregate stages that write the pipeline result into a collection. They hide like the
/// JavaScript operators: inside `$facet`, `$lookup.pipeline`, `$unionWith.pipeline`, any
/// nesting.
const WRITE_STAGE_KEYS: &[&str] = &["$out", "$merge"];

/// Commands inside `explain` that only read. `explain` runs the winning plan by default, so
/// any other inner command (`mapReduce`, `update`, …) is not a provable read.
const EXPLAIN_READS: &[&str] = &["find", "aggregate", "count", "distinct"];

fn explain_reads(command: &Document) -> bool {
    let Ok(inner) = command.get_document("explain") else {
        return false;
    };
    let first_ok = inner
        .iter()
        .next()
        .is_some_and(|(name, _)| EXPLAIN_READS.contains(&name.as_str()));
    let mut write_stage = false;
    walk_doc(inner, &mut |k, _| {
        write_stage |= WRITE_STAGE_KEYS.contains(&k)
    });
    first_ok && !write_stage
}

/// A `runCommand` / `adminCommand` document that only reads: an allow-listed first key, or an
/// `explain` of a read. `kind` classifies by this, and the AI limits need the same answer.
pub(crate) fn run_command_reads(command: &Document) -> bool {
    match command.iter().next() {
        Some((name, _)) if name == "explain" => explain_reads(command),
        Some((name, _)) => READ_COMMANDS.contains(&name.as_str()),
        None => false,
    }
}

pub fn kind(cmd: &Command) -> Kind {
    // Fail closed: JavaScript can write, so any command that carries it is not provably a
    // read, not even a `find` or a `runCommand` whose first key is on the allow-list.
    if find_key(cmd, JAVASCRIPT_KEYS).is_some() {
        return Kind::Unknown;
    }
    match cmd {
        Command::ShowDbs | Command::ShowCollections | Command::Use(_) => Kind::Read,
        Command::CreateCollection { .. } => Kind::Schema,
        Command::DropDatabase { .. } => Kind::Write,
        Command::RunCommand { command, .. } => {
            if run_command_reads(command) {
                Kind::Read
            } else {
                Kind::Unknown
            }
        }
        Command::Collection { method, .. } => match method {
            Method::Find(_)
            | Method::FindOne { .. }
            | Method::CountDocuments { .. }
            | Method::EstimatedDocumentCount
            | Method::Distinct { .. }
            | Method::GetIndexes
            | Method::Stats => Kind::Read,
            Method::Aggregate { .. } => {
                if find_key_in_method(method, WRITE_STAGE_KEYS).is_some() {
                    Kind::Write
                } else {
                    Kind::Read
                }
            }
            Method::InsertOne { .. }
            | Method::InsertMany { .. }
            | Method::UpdateOne { .. }
            | Method::UpdateMany { .. }
            | Method::ReplaceOne { .. }
            | Method::DeleteOne { .. }
            | Method::DeleteMany { .. }
            | Method::FindOneAndUpdate { .. }
            | Method::FindOneAndReplace { .. }
            | Method::FindOneAndDelete { .. }
            | Method::BulkWrite { .. } => Kind::Write,
            Method::CreateIndex { .. }
            | Method::CreateIndexes { .. }
            | Method::DropIndex { .. }
            | Method::DropIndexes
            | Method::Drop
            | Method::RenameCollection { .. } => Kind::Schema,
        },
    }
}

/// Read-only barrier for the editor, the Mongo side of `conn_ro::check_read_only`: every
/// statement must be a provable read, and unknown statements are refused.
pub fn check_read_only(conn: &Connection, stmts: &[Statement]) -> Result<(), AppError> {
    for s in stmts {
        if kind(&s.command) != Kind::Read {
            return Err(read_only_refusal(conn, Some(&reason(&s.command))));
        }
    }
    Ok(())
}

pub fn exactly_one(stmts: &[Statement]) -> Result<&Statement, String> {
    match stmts {
        [] => Err("The query has no statement".into()),
        [s] => Ok(s),
        many => Err(format!(
            "Exactly one statement is allowed, found {}",
            many.len()
        )),
    }
}

pub fn guard_check(text: &str) -> GuardCheck {
    match parse::parse(text) {
        Err(e) => refused(Some(e.to_string())),
        Ok(stmts) => match exactly_one(&stmts) {
            Err(reason) => refused(Some(reason)),
            Ok(s) => guard_one(s),
        },
    }
}

fn guard_one(s: &Statement) -> GuardCheck {
    match kind(&s.command) {
        Kind::Read => GuardCheck {
            allowed: true,
            reason: None,
            kind: Some(name(&s.command).into()),
        },
        _ => refused(Some(reason(&s.command))),
    }
}

fn refused(reason: Option<String>) -> GuardCheck {
    GuardCheck {
        allowed: false,
        reason,
        kind: None,
    }
}

/// `classify_write` for Mongo: never refuses. Statements the guard cannot vouch for get an
/// `unknown_command` warning and the user decides, like unknown SQL. `database` is the tab's
/// database when the text starts; a `use` changes it for the statements after it, as `run.rs`
/// does, and every statement carries its effective database for the confirmation dialog.
pub fn classify_write(text: &str, database: Option<&str>) -> WriteInfo {
    let stmts = match parse::parse(text) {
        Ok(s) => s,
        Err(e) => {
            return WriteInfo {
                statements: Vec::new(),
                warnings: vec![WriteWarning {
                    code: WarningCode::Unparsable,
                    message: e.to_string(),
                    statement: None,
                }],
            };
        }
    };
    let mut warnings = Vec::new();
    if stmts.len() > 1 {
        warnings.push(WriteWarning {
            code: WarningCode::MultipleStatements,
            message: format!("{} statements will run", stmts.len()),
            statement: None,
        });
    }
    let mut database = database.unwrap_or_default().to_string();
    let mut statements = Vec::with_capacity(stmts.len());
    for (i, s) in stmts.iter().enumerate() {
        if let Command::Use(name) = &s.command {
            database.clone_from(name);
        }
        let (mut statement, mut warned) = classify_statement(s, i);
        statement.database = (!database.is_empty()).then(|| database.clone());
        warnings.append(&mut warned);
        statements.push(statement);
    }
    WriteInfo {
        statements,
        warnings,
    }
}

fn classify_statement(s: &Statement, i: usize) -> (WriteStatement, Vec<WriteWarning>) {
    let mut warnings = Vec::new();
    let k = kind(&s.command);
    if let Command::Collection { collection, method } = &s.command {
        match method {
            Method::UpdateMany { filter, .. } if filter.is_empty() => warnings.push(WriteWarning {
                code: WarningCode::UpdateWithoutWhere,
                message: format!(
                    "updateMany with an empty filter changes every document of {collection}"
                ),
                statement: Some(i),
            }),
            Method::DeleteMany { filter } if filter.is_empty() => warnings.push(WriteWarning {
                code: WarningCode::DeleteWithoutWhere,
                message: format!(
                    "deleteMany with an empty filter removes every document of {collection}"
                ),
                statement: Some(i),
            }),
            Method::BulkWrite { operations, .. } => {
                for op in operations {
                    bulk_write_empty_filter(op, collection, i, &mut warnings);
                }
            }
            Method::Drop => warnings.push(WriteWarning {
                code: WarningCode::Drop,
                message: format!("drop {collection} cannot be undone"),
                statement: Some(i),
            }),
            Method::DropIndexes => warnings.push(WriteWarning {
                code: WarningCode::Drop,
                message: format!("dropIndexes on {collection} cannot be undone"),
                statement: Some(i),
            }),
            Method::RenameCollection { .. } => warnings.push(WriteWarning {
                code: WarningCode::Rename,
                message: format!("renameCollection {collection} cannot be undone"),
                statement: Some(i),
            }),
            _ => {}
        }
    }
    if let Command::DropDatabase { .. } = &s.command {
        warnings.push(WriteWarning {
            code: WarningCode::Drop,
            message: "dropDatabase drops the current database and cannot be undone".into(),
            statement: Some(i),
        });
    }
    if k == Kind::Unknown {
        warnings.push(WriteWarning {
            code: WarningCode::UnknownCommand,
            message: reason(&s.command),
            statement: Some(i),
        });
    }
    let statement = WriteStatement {
        kind: name(&s.command).to_string(),
        table: table(&s.command),
        database: None,
        read: Some(k == Kind::Read),
        structure: Some(is_structure(&s.command)),
    };
    (statement, warnings)
}

/// Whether the statement changes the schema, the collection list or what a `find` sees. The
/// editor's confirmation for a Production connection counts these; a writing aggregate is not
/// one.
fn is_structure(cmd: &Command) -> bool {
    match cmd {
        Command::CreateCollection { .. } | Command::DropDatabase { .. } => true,
        // A command the guard cannot prove read is a schema change, as the confirmation has
        // always treated it.
        Command::RunCommand { .. } => kind(cmd) != Kind::Read,
        Command::Collection { method, .. } => matches!(
            method,
            Method::CreateIndex { .. }
                | Method::CreateIndexes { .. }
                | Method::DropIndex { .. }
                | Method::DropIndexes
                | Method::Drop
                | Method::RenameCollection { .. }
        ),
        Command::ShowDbs | Command::ShowCollections | Command::Use(_) => false,
    }
}

/// `updateMany` / `deleteMany` models of a `bulkWrite` with an empty or missing filter change
/// or remove every document, like the direct methods do.
fn bulk_write_empty_filter(
    op: &Document,
    collection: &str,
    i: usize,
    warnings: &mut Vec<WriteWarning>,
) {
    for (model, code, verb) in [
        ("updateMany", WarningCode::UpdateWithoutWhere, "changes"),
        ("deleteMany", WarningCode::DeleteWithoutWhere, "removes"),
    ] {
        let empty = match op.get(model).and_then(Bson::as_document) {
            Some(m) => m
                .get("filter")
                .is_none_or(|f| f.as_document().is_some_and(Document::is_empty)),
            None => false,
        };
        if empty {
            warnings.push(WriteWarning {
                code,
                message: format!(
                    "{model} with an empty filter {verb} every document of {collection}"
                ),
                statement: Some(i),
            });
        }
    }
}

/// The name the user wrote, for messages, the result header, `classify_write` and the run notice.
pub(crate) fn name(cmd: &Command) -> &'static str {
    match cmd {
        Command::ShowDbs => "show dbs",
        Command::ShowCollections => "show collections",
        Command::Use(_) => "use",
        Command::RunCommand { .. } => "runCommand",
        Command::CreateCollection { .. } => "createCollection",
        Command::DropDatabase { .. } => "dropDatabase",
        Command::Collection { method, .. } => method.name(),
    }
}

fn table(cmd: &Command) -> Option<String> {
    match cmd {
        Command::Collection { collection, .. } => Some(collection.clone()),
        Command::CreateCollection { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Why the statement is not a read, for the read-only refusal and the unknown-command
/// warning. Only called for statements that are not reads; names what the user wrote, as
/// `conn_ro.rs` names the refused SQL.
pub(crate) fn reason(cmd: &Command) -> String {
    if let Some(js) = find_key(cmd, JAVASCRIPT_KEYS) {
        return format!("{js} runs JavaScript on the server");
    }
    match cmd {
        Command::RunCommand { admin, command } => {
            let called = if *admin { "adminCommand" } else { "runCommand" };
            let first = command.iter().next().map_or("?", |(k, _)| k.as_str());
            if first == "explain" {
                return format!(
                    "{called} explain is a read only for {} without $out or $merge",
                    EXPLAIN_READS.join(", ")
                );
            }
            format!("{called} {first} is not on the list of read commands")
        }
        Command::CreateCollection { .. } => "createCollection changes the schema".into(),
        Command::DropDatabase { .. } => "dropDatabase drops the current database".into(),
        Command::Collection { method, .. } => {
            let name = method.name();
            match method {
                Method::Aggregate { .. } => {
                    let stage = find_key_in_method(method, WRITE_STAGE_KEYS).unwrap_or_default();
                    format!("aggregate with {stage} changes data")
                }
                Method::InsertOne { .. }
                | Method::InsertMany { .. }
                | Method::UpdateOne { .. }
                | Method::UpdateMany { .. }
                | Method::ReplaceOne { .. }
                | Method::DeleteOne { .. }
                | Method::DeleteMany { .. }
                | Method::FindOneAndUpdate { .. }
                | Method::FindOneAndReplace { .. }
                | Method::FindOneAndDelete { .. }
                | Method::BulkWrite { .. } => format!("{name} changes data"),
                _ => format!("{name} changes the schema"),
            }
        }
        _ => "this statement is not on the list of read commands".into(),
    }
}

fn find_key(cmd: &Command, keys: &[&str]) -> Option<String> {
    let mut found = None;
    walk_command(cmd, &mut |k, _| {
        if found.is_none() && keys.contains(&k) {
            found = Some(k.to_owned());
        }
    });
    found
}

fn find_key_in_method(method: &Method, keys: &[&str]) -> Option<String> {
    let mut found = None;
    walk_method(method, &mut |k, _| {
        if found.is_none() && keys.contains(&k) {
            found = Some(k.to_owned());
        }
    });
    found
}

fn walk_command(cmd: &Command, f: &mut impl FnMut(&str, &Bson)) {
    match cmd {
        Command::ShowDbs | Command::ShowCollections | Command::Use(_) => {}
        Command::RunCommand { command, .. } => walk_doc(command, f),
        Command::CreateCollection { options, .. } => {
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Command::DropDatabase { options } => {
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Command::Collection { method, .. } => walk_method(method, f),
    }
}

fn walk_method(method: &Method, f: &mut impl FnMut(&str, &Bson)) {
    match method {
        Method::Find(find) => {
            walk_doc(&find.filter, f);
            if let Some(projection) = &find.projection {
                walk_doc(projection, f);
            }
            if let Some(sort) = &find.sort {
                walk_doc(sort, f);
            }
            if let Some(hint) = &find.hint {
                walk_value(hint, f);
            }
            if let Some(collation) = &find.collation {
                walk_doc(collation, f);
            }
        }
        Method::FindOne { filter, projection } => {
            walk_doc(filter, f);
            if let Some(projection) = projection {
                walk_doc(projection, f);
            }
        }
        Method::Aggregate { pipeline, options } => {
            for stage in pipeline {
                walk_doc(stage, f);
            }
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::CountDocuments { filter, options } => {
            walk_doc(filter, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::EstimatedDocumentCount | Method::GetIndexes | Method::Stats => {}
        Method::Distinct { filter, .. } => walk_doc(filter, f),
        Method::InsertOne { document } => walk_doc(document, f),
        Method::InsertMany { documents, options } => {
            for document in documents {
                walk_doc(document, f);
            }
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::UpdateOne {
            filter,
            update,
            options,
        }
        | Method::UpdateMany {
            filter,
            update,
            options,
        } => {
            walk_doc(filter, f);
            walk_value(update, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::ReplaceOne {
            filter,
            replacement,
            options,
        } => {
            walk_doc(filter, f);
            walk_doc(replacement, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::DeleteOne { filter } | Method::DeleteMany { filter } => walk_doc(filter, f),
        Method::FindOneAndUpdate {
            filter,
            update,
            options,
        } => {
            walk_doc(filter, f);
            walk_value(update, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::FindOneAndReplace {
            filter,
            replacement,
            options,
        } => {
            walk_doc(filter, f);
            walk_doc(replacement, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::FindOneAndDelete { filter, options } => {
            walk_doc(filter, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::BulkWrite {
            operations,
            options,
        } => {
            for op in operations {
                walk_doc(op, f);
            }
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::CreateIndex { keys, options } => {
            walk_doc(keys, f);
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::CreateIndexes { keys, options } => {
            for keys in keys {
                walk_doc(keys, f);
            }
            if let Some(options) = options {
                walk_doc(options, f);
            }
        }
        Method::DropIndex { index } => walk_value(index, f),
        Method::DropIndexes | Method::Drop | Method::RenameCollection { .. } => {}
    }
}

fn walk_doc(doc: &Document, f: &mut impl FnMut(&str, &Bson)) {
    for (k, v) in doc {
        f(k, v);
        walk_value(v, f);
    }
}

fn walk_value(v: &Bson, f: &mut impl FnMut(&str, &Bson)) {
    match v {
        Bson::Document(doc) => walk_doc(doc, f),
        Bson::Array(items) => {
            for item in items {
                walk_value(item, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{Env, SslMode};
    use crate::core::engine::Engine;
    use crate::core::error::ErrorCode;
    use crate::core::mongo::command::Find;
    use mongodb::bson::doc;

    fn stmt(command: Command) -> Statement {
        Statement {
            text: String::new(),
            command,
        }
    }

    fn coll(method: Method) -> Command {
        Command::Collection {
            collection: "orders".into(),
            method,
        }
    }

    fn conn() -> Connection {
        Connection {
            id: "c1".into(),
            name: "Prod".into(),
            engine: Engine::Mongo,
            version: String::new(),
            env: Env::Production,
            host: String::new(),
            port: 27017,
            database: String::new(),
            user: String::new(),
            ssl: SslMode::Disable,
            file: String::new(),
            ai_write: false,
            read_only: true,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    /// Every `Method` variant, with the classification the guard owes it. The `match` is
    /// exhaustive, so a new variant does not compile until it is classified here and added to
    /// `cases`.
    #[test]
    fn every_method_is_classified() {
        fn expected(m: &Method) -> Kind {
            match m {
                Method::Find(_)
                | Method::FindOne { .. }
                | Method::CountDocuments { .. }
                | Method::EstimatedDocumentCount
                | Method::Distinct { .. }
                | Method::GetIndexes
                | Method::Stats => Kind::Read,
                // A pipeline without a write stage reads; `$out`/`$merge` are covered by
                // `aggregate_with_out_or_merge_anywhere_is_a_write`.
                Method::Aggregate { .. } => Kind::Read,
                Method::InsertOne { .. }
                | Method::InsertMany { .. }
                | Method::UpdateOne { .. }
                | Method::UpdateMany { .. }
                | Method::ReplaceOne { .. }
                | Method::DeleteOne { .. }
                | Method::DeleteMany { .. }
                | Method::FindOneAndUpdate { .. }
                | Method::FindOneAndReplace { .. }
                | Method::FindOneAndDelete { .. }
                | Method::BulkWrite { .. } => Kind::Write,
                Method::CreateIndex { .. }
                | Method::CreateIndexes { .. }
                | Method::DropIndex { .. }
                | Method::DropIndexes
                | Method::Drop
                | Method::RenameCollection { .. } => Kind::Schema,
            }
        }

        let cases = [
            Method::Find(Find::default()),
            Method::FindOne {
                filter: doc! { "a": 1 },
                projection: None,
            },
            Method::Aggregate {
                pipeline: vec![doc! { "$match": { "a": 1 } }],
                options: None,
            },
            Method::CountDocuments {
                filter: doc! {},
                options: None,
            },
            Method::EstimatedDocumentCount,
            Method::Distinct {
                field: "a".into(),
                filter: doc! {},
            },
            Method::GetIndexes,
            Method::Stats,
            Method::InsertOne {
                document: doc! { "a": 1 },
            },
            Method::InsertMany {
                documents: vec![doc! { "a": 1 }],
                options: None,
            },
            Method::UpdateOne {
                filter: doc! { "a": 1 },
                update: doc! { "$set": { "b": 2 } }.into(),
                options: None,
            },
            Method::UpdateMany {
                filter: doc! { "a": 1 },
                update: doc! { "$set": { "b": 2 } }.into(),
                options: None,
            },
            Method::ReplaceOne {
                filter: doc! { "a": 1 },
                replacement: doc! { "a": 2 },
                options: None,
            },
            Method::DeleteOne {
                filter: doc! { "a": 1 },
            },
            Method::DeleteMany {
                filter: doc! { "a": 1 },
            },
            Method::FindOneAndUpdate {
                filter: doc! { "a": 1 },
                update: doc! { "$set": { "b": 2 } }.into(),
                options: None,
            },
            Method::FindOneAndReplace {
                filter: doc! { "a": 1 },
                replacement: doc! { "a": 2 },
                options: None,
            },
            Method::FindOneAndDelete {
                filter: doc! { "a": 1 },
                options: None,
            },
            Method::BulkWrite {
                operations: vec![doc! { "insertOne": { "document": { "a": 1 } } }],
                options: None,
            },
            Method::CreateIndex {
                keys: doc! { "a": 1 },
                options: None,
            },
            Method::CreateIndexes {
                keys: vec![doc! { "a": 1 }],
                options: None,
            },
            Method::DropIndex {
                index: "a_1".into(),
            },
            Method::DropIndexes,
            Method::Drop,
            Method::RenameCollection {
                to: "orders_old".into(),
                drop_target: false,
            },
        ];
        for m in cases {
            assert_eq!(kind(&coll(m.clone())), expected(&m), "{m:?}");
        }

        // The non-collection commands stay grouped as before.
        for cmd in [
            Command::ShowDbs,
            Command::ShowCollections,
            Command::Use("shop".into()),
            Command::RunCommand {
                admin: false,
                command: doc! { "ping": 1 },
            },
            Command::RunCommand {
                admin: true,
                command: doc! { "buildInfo": 1 },
            },
        ] {
            assert_eq!(kind(&cmd), Kind::Read, "{cmd:?}");
        }
        assert_eq!(
            kind(&Command::CreateCollection {
                name: "new".into(),
                options: None,
            }),
            Kind::Schema
        );
    }

    #[test]
    fn aggregate_with_out_or_merge_anywhere_is_a_write() {
        // Top level of the pipeline.
        let out = coll(Method::Aggregate {
            pipeline: vec![doc! { "$match": { "a": 1 } }, doc! { "$out": "copy" }],
            options: None,
        });
        assert_eq!(kind(&out), Kind::Write);
        let merge = coll(Method::Aggregate {
            pipeline: vec![doc! { "$merge": { "into": "copy" } }],
            options: None,
        });
        assert_eq!(kind(&merge), Kind::Write);
        // Inside `$facet`.
        let facet = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$facet": {
                    "kept": [{ "$match": { "a": 1 } }],
                    "archived": [{ "$out": "archive" }],
                }
            }],
            options: None,
        });
        assert_eq!(kind(&facet), Kind::Write);
        // Inside `$lookup.pipeline`.
        let lookup = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$lookup": {
                    "from": "archive",
                    "pipeline": [{ "$merge": { "into": "log" } }],
                    "as": "x",
                }
            }],
            options: None,
        });
        assert_eq!(kind(&lookup), Kind::Write);
        // Inside `$unionWith.pipeline`.
        let union = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$unionWith": { "coll": "archive", "pipeline": [{ "$out": "copy" }] }
            }],
            options: None,
        });
        assert_eq!(kind(&union), Kind::Write);
        // Nested inside all three at once.
        let deep = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$facet": { "x": [{ "$lookup": { "from": "archive", "pipeline": [
                    { "$unionWith": { "coll": "log", "pipeline": [{ "$merge": { "into": "log" } }] } }
                ], "as": "y" } }] }
            }],
            options: None,
        });
        assert_eq!(kind(&deep), Kind::Write);
        // The same shapes without a write stage stay reads.
        let plain = coll(Method::Aggregate {
            pipeline: vec![doc! { "$facet": { "x": [{ "$match": { "a": 1 } }] } }],
            options: None,
        });
        assert_eq!(kind(&plain), Kind::Read);
        let lookup_plain = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$lookup": { "from": "b", "pipeline": [{ "$match": { "a": 1 } }], "as": "x" }
            }],
            options: None,
        });
        assert_eq!(kind(&lookup_plain), Kind::Read);
    }

    #[test]
    fn drop_database_is_a_write() {
        let cmd = Command::DropDatabase { options: None };
        assert_eq!(kind(&cmd), Kind::Write);
        let (st, warnings) = classify_statement(&stmt(cmd), 0);
        assert_eq!(st.kind, "dropDatabase");
        assert_eq!(st.table, None);
        assert_eq!(st.read, Some(false));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::Drop);

        // The options are walked like every other argument: JavaScript anywhere fails closed.
        let cmd = Command::DropDatabase {
            options: Some(doc! { "writeConcern": { "$where": "1" } }),
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
    }

    #[test]
    fn runcommand_allows_only_the_read_list() {
        for name in READ_COMMANDS {
            let mut command = Document::new();
            command.insert(*name, 1);
            let cmd = Command::RunCommand {
                admin: false,
                command,
            };
            assert_eq!(kind(&cmd), Kind::Read, "{name}");
        }
        // Case-sensitive, like the server.
        let cmd = Command::RunCommand {
            admin: false,
            command: doc! { "Ping": 1 },
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
        // Anything off the list is unknown, admin or not, empty too.
        let cmd = Command::RunCommand {
            admin: false,
            command: doc! { "dropDatabase": 1 },
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
        let cmd = Command::RunCommand {
            admin: true,
            command: doc! { "shutdown": 1 },
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
        let cmd = Command::RunCommand {
            admin: true,
            command: Document::new(),
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
    }

    #[test]
    fn read_command_arguments_still_refuse_javascript() {
        // `listDatabases` takes a filter, which may hold `$expr`; a plain one stays a read.
        let cmd = |filter: Document| Command::RunCommand {
            admin: true,
            command: doc! { "listDatabases": 1, "filter": filter },
        };
        assert_eq!(
            kind(&cmd(
                doc! { "$expr": { "$gt": [{ "$size": "$databases" }, 0] } }
            )),
            Kind::Read
        );
        // JavaScript in that filter fails closed, at any depth.
        assert_eq!(
            kind(&cmd(doc! { "$expr": { "$function": {
                "body": "function() { return true; }", "args": [], "lang": "js"
            } } })),
            Kind::Unknown
        );
        assert_eq!(kind(&cmd(doc! { "$where": "true" })), Kind::Unknown);
    }

    #[test]
    fn javascript_makes_any_command_unknown() {
        // `$where` inside a find filter.
        let cmd = coll(Method::Find(Find {
            filter: doc! { "$where": "this.a > 1" },
            ..Default::default()
        }));
        assert_eq!(kind(&cmd), Kind::Unknown);
        // `$function` inside an aggregate `$addFields`.
        let cmd = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$addFields": {
                    "out": { "$function": { "body": "function() { return 1; }", "args": [], "lang": "js" } }
                }
            }],
            options: None,
        });
        assert_eq!(kind(&cmd), Kind::Unknown);
        // `$accumulator` inside `$group`.
        let cmd = coll(Method::Aggregate {
            pipeline: vec![doc! {
                "$group": { "_id": null, "n": { "$accumulator": { "init": "function() { return 0; }" } } }
            }],
            options: None,
        });
        assert_eq!(kind(&cmd), Kind::Unknown);
        // Deep inside a bulkWrite update pipeline.
        let cmd = coll(Method::BulkWrite {
            operations: vec![doc! {
                "updateMany": {
                    "filter": {},
                    "update": [{ "$set": { "x": { "$function": { "body": "", "args": [], "lang": "js" } } } }],
                }
            }],
            options: None,
        });
        assert_eq!(kind(&cmd), Kind::Unknown);
        // Inside a runCommand argument, the first key being on the allow-list does not save it.
        let cmd = Command::RunCommand {
            admin: false,
            command: doc! { "explain": { "find": "orders", "filter": { "$where": "1" } } },
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
    }

    #[test]
    fn explain_is_a_read_only_for_read_commands() {
        let explain = |inner: Document| Command::RunCommand {
            admin: false,
            command: doc! { "explain": inner, "verbosity": "queryPlanner" },
        };
        assert_eq!(
            kind(&explain(doc! { "find": "orders", "filter": { "a": 1 } })),
            Kind::Read
        );
        assert_eq!(
            kind(&explain(
                doc! { "aggregate": "orders", "pipeline": [{ "$match": {} }], "cursor": {} }
            )),
            Kind::Read
        );
        for inner in [
            doc! { "mapReduce": "orders", "map": "function(){}", "reduce": "function(){}", "out": { "inline": 1 } },
            doc! { "aggregate": "orders", "pipeline": [{ "$out": "copy" }], "cursor": {} },
            doc! { "aggregate": "orders", "pipeline": [{ "$facet": { "a": [{ "$merge": { "into": "x" } }] } }], "cursor": {} },
            doc! { "update": "orders", "updates": [] },
        ] {
            let cmd = explain(inner.clone());
            assert_eq!(kind(&cmd), Kind::Unknown, "{inner}");
            assert!(reason(&cmd).contains("explain"), "{}", reason(&cmd));
        }
        // Not a document: fail closed.
        let cmd = Command::RunCommand {
            admin: false,
            command: doc! { "explain": 1 },
        };
        assert_eq!(kind(&cmd), Kind::Unknown);
    }

    #[test]
    fn read_only_refusal_names_the_method() {
        let conn = conn();
        let stmts = [stmt(coll(Method::UpdateMany {
            filter: doc! {},
            update: doc! { "$set": { "a": 1 } }.into(),
            options: None,
        }))];
        let err = check_read_only(&conn, &stmts).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
        assert_eq!(
            err.message,
            "Connection \"Prod\" is read-only: updateMany changes data"
        );

        let stmts = [stmt(coll(Method::Drop))];
        let err = check_read_only(&conn, &stmts).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
        assert!(err.message.contains("drop"), "{}", err.message);

        let stmts = [stmt(Command::DropDatabase { options: None })];
        let err = check_read_only(&conn, &stmts).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
        assert!(err.message.contains("dropDatabase"), "{}", err.message);

        let stmts = [stmt(Command::RunCommand {
            admin: false,
            command: doc! { "dropDatabase": 1 },
        })];
        let err = check_read_only(&conn, &stmts).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
        assert!(err.message.contains("dropDatabase"), "{}", err.message);

        let stmts = [stmt(coll(Method::Find(Find {
            filter: doc! { "$where": "1" },
            ..Default::default()
        })))];
        let err = check_read_only(&conn, &stmts).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
        assert!(err.message.contains("$where"), "{}", err.message);

        // A genuine read passes, and an empty run has nothing to refuse.
        let stmts = [stmt(coll(Method::Find(Find::default())))];
        assert!(check_read_only(&conn, &stmts).is_ok());
        assert!(check_read_only(&conn, &[]).is_ok());
    }

    #[test]
    fn guard_one_allows_exactly_reads() {
        let ok = guard_one(&stmt(coll(Method::Find(Find::default()))));
        assert!(ok.allowed);
        assert_eq!(ok.kind.as_deref(), Some("find"));
        assert_eq!(ok.reason, None);

        let ok = guard_one(&stmt(Command::ShowDbs));
        assert!(ok.allowed);
        assert_eq!(ok.kind.as_deref(), Some("show dbs"));

        let refused = guard_one(&stmt(coll(Method::Drop)));
        assert!(!refused.allowed);
        assert_eq!(refused.kind, None);
        assert!(refused.reason.is_some());
    }

    #[test]
    fn empty_filter_warnings() {
        let s = stmt(coll(Method::UpdateMany {
            filter: doc! {},
            update: doc! { "$set": { "a": 1 } }.into(),
            options: None,
        }));
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.kind, "updateMany");
        assert_eq!(st.table.as_deref(), Some("orders"));
        assert_eq!(st.read, Some(false));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::UpdateWithoutWhere);
        assert_eq!(warnings[0].statement, Some(0));

        let s = stmt(coll(Method::DeleteMany { filter: doc! {} }));
        let (st, warnings) = classify_statement(&s, 2);
        assert_eq!(st.kind, "deleteMany");
        assert_eq!(st.read, Some(false));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::DeleteWithoutWhere);
        assert_eq!(warnings[0].statement, Some(2));

        // A real filter does not warn.
        let s = stmt(coll(Method::UpdateMany {
            filter: doc! { "status": "open" },
            update: doc! { "$set": { "a": 1 } }.into(),
            options: None,
        }));
        let (_, warnings) = classify_statement(&s, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn bulk_write_models_warn_on_empty_filters() {
        let s = stmt(coll(Method::BulkWrite {
            operations: vec![
                doc! { "updateMany": { "filter": {}, "update": { "$set": { "a": 1 } } } },
                doc! { "deleteMany": { "filter": {} } },
                doc! { "insertOne": { "document": { "a": 1 } } },
                doc! { "updateMany": { "filter": { "status": "open" }, "update": { "$set": { "a": 1 } } } },
            ],
            options: None,
        }));
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.kind, "bulkWrite");
        assert_eq!(st.read, Some(false));
        let codes: Vec<_> = warnings.iter().map(|w| w.code).collect();
        assert_eq!(
            codes,
            [
                WarningCode::UpdateWithoutWhere,
                WarningCode::DeleteWithoutWhere
            ]
        );
        assert_eq!(warnings[0].statement, Some(0));
        assert_eq!(warnings[1].statement, Some(0));
    }

    #[test]
    fn schema_warnings() {
        let s = stmt(coll(Method::Drop));
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.kind, "drop");
        assert_eq!(st.table.as_deref(), Some("orders"));
        assert_eq!(st.read, Some(false));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::Drop);

        let s = stmt(coll(Method::DropIndexes));
        let (_, warnings) = classify_statement(&s, 0);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::Drop);

        let s = stmt(coll(Method::RenameCollection {
            to: "orders_old".into(),
            drop_target: false,
        }));
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.kind, "renameCollection");
        assert_eq!(st.table.as_deref(), Some("orders"));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::Rename);
    }

    #[test]
    fn unknown_command_warns_with_the_reason() {
        let s = stmt(Command::RunCommand {
            admin: false,
            command: doc! { "dropDatabase": 1 },
        });
        let (st, warnings) = classify_statement(&s, 1);
        assert_eq!(st.kind, "runCommand");
        assert_eq!(st.read, Some(false));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::UnknownCommand);
        assert_eq!(warnings[0].statement, Some(1));

        // A read does not warn.
        let s = stmt(coll(Method::Find(Find::default())));
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.read, Some(true));
        assert!(warnings.is_empty());

        // A read `runCommand` from the list is a read too, so Production does not ask.
        let s = stmt(Command::RunCommand {
            admin: true,
            command: doc! { "listDatabases": 1 },
        });
        let (st, warnings) = classify_statement(&s, 0);
        assert_eq!(st.kind, "runCommand");
        assert_eq!(st.read, Some(true));
        assert!(warnings.is_empty());
    }

    #[test]
    fn statement_kinds_and_tables() {
        let (st, warnings) = classify_statement(&stmt(Command::ShowDbs), 0);
        assert_eq!(st.kind, "show dbs");
        assert_eq!(st.read, Some(true));
        assert!(warnings.is_empty());

        let (st, _) = classify_statement(&stmt(Command::ShowCollections), 0);
        assert_eq!(st.kind, "show collections");

        let (st, _) = classify_statement(&stmt(Command::Use("shop".into())), 0);
        assert_eq!(st.kind, "use");
        assert_eq!(st.table, None);

        let (st, _) = classify_statement(&stmt(coll(Method::Stats)), 0);
        assert_eq!(st.kind, "stats");

        let (st, _) = classify_statement(
            &stmt(Command::CreateCollection {
                name: "new".into(),
                options: None,
            }),
            0,
        );
        assert_eq!(st.kind, "createCollection");
        assert_eq!(st.table.as_deref(), Some("new"));
        assert_eq!(st.read, Some(false));
    }

    #[test]
    fn guard_check_text() {
        let ok = guard_check("db.orders.find({ status: 'open' })");
        assert!(ok.allowed);
        assert_eq!(ok.kind.as_deref(), Some("find"));
        assert_eq!(ok.reason, None);

        let refused = guard_check("db.orders.find({}); db.orders.drop()");
        assert!(!refused.allowed);
        assert_eq!(refused.kind, None);
        let reason = refused.reason.unwrap();
        assert!(reason.contains('2'), "{reason}");

        // The parse error text carries its position, like the SQL guard's.
        let refused = guard_check("db.orders.find(");
        assert!(!refused.allowed);
        assert!(refused.reason.unwrap().starts_with("Line "));

        // Only whitespace or comments parses to no statement.
        let refused = guard_check("  // nothing\n");
        assert!(!refused.allowed);
        assert!(
            refused.reason.unwrap().contains("statement"),
            "the refusal names the missing statement"
        );
    }

    #[test]
    fn classify_write_follows_use_for_each_database() {
        let info = classify_write(
            "db.orders.find()\nuse other\ndb.orders.drop()\nuse shop\ndb.dropDatabase()",
            Some("start"),
        );
        let got: Vec<_> = info
            .statements
            .iter()
            .map(|s| (s.kind.as_str(), s.database.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("find", Some("start")),
                ("use", Some("other")),
                ("drop", Some("other")),
                ("use", Some("shop")),
                ("dropDatabase", Some("shop")),
            ]
        );

        // Without a starting database, a run without `use` has none to show.
        let info = classify_write("db.dropDatabase()", None);
        assert_eq!(info.statements[0].database, None);
    }

    #[test]
    fn classify_write_text() {
        let info = classify_write(
            "db.orders.updateMany({}, { $set: { status: 'archived' } })",
            None,
        );
        assert_eq!(info.statements.len(), 1);
        assert_eq!(info.statements[0].kind, "updateMany");
        assert_eq!(info.statements[0].table.as_deref(), Some("orders"));
        assert_eq!(info.statements[0].read, Some(false));
        assert_eq!(info.warnings.len(), 1);
        assert_eq!(info.warnings[0].code, WarningCode::UpdateWithoutWhere);
        assert_eq!(info.warnings[0].statement, Some(0));

        // A read does not warn.
        let info = classify_write("db.orders.find({ status: 'open' })", None);
        assert_eq!(info.statements[0].read, Some(true));
        assert!(info.warnings.is_empty());

        // Several statements: the whole-text warning comes first, without an index.
        let info = classify_write("db.orders.insertOne({ a: 1 }); db.orders.drop()", None);
        assert_eq!(info.statements.len(), 2);
        assert_eq!(info.warnings[0].code, WarningCode::MultipleStatements);
        assert_eq!(info.warnings[0].statement, None);
        assert_eq!(info.warnings[1].code, WarningCode::Drop);
        assert_eq!(info.warnings[1].statement, Some(1));

        // Unparsable: one whole-text warning, no statements.
        let info = classify_write("db.orders.find(", None);
        assert!(info.statements.is_empty());
        assert_eq!(info.warnings.len(), 1);
        assert_eq!(info.warnings[0].code, WarningCode::Unparsable);
        assert_eq!(info.warnings[0].statement, None);
        assert!(info.warnings[0].message.starts_with("Line "));
    }
}
