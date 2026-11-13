//! Generates the TypeScript wire contract from the serde types, and fails when the committed
//! files differ. The frontend imports `src/lib/api/generated/wire.ts` and `constants.ts`; this
//! test is the only writer, so a renamed field or a new variant cannot leave the two sides apart.

use std::path::PathBuf;

use ts_rs::TS;

use crate::core::ai::client::Provider;
use crate::core::ai::event::{ChatEvent, ToolKind, ToolResultData};
use crate::core::ai::session::{ChatRequest, ChatRole, ChatSource, ChatTurn, MAX_SOURCES, MAX_STEPS};
use crate::core::ai::settings::{
    AiSettings, AiSettingsView, AiTestResult, DEFAULT_AI_BASE_URL, DEFAULT_AI_MODEL, MAX_ROWS_LIMIT,
    Privacy, TIMEOUT_SEC_LIMIT,
};
use crate::core::ai::tools::{CHART_ROWS, CHART_TYPES, UI_ROWS};
use crate::core::cell::PREVIEW_BYTES;
use crate::core::connections::{
    Connection, ConnectionInput, Env, SslMode, TestResult, Topology,
};
use crate::core::db::export::conflict_text;
use crate::core::db::session::TabStatus;
use crate::core::engine::{Engine, SqlEngine};
use crate::core::error::{AppError, ErrorCode};
use crate::core::mongo::parse::{COLLECTION_METHODS, CURSOR_METHODS, DB_METHODS};
use crate::core::mongo::value::WRITABLE_TYPES;
use crate::core::result::{
    AiReadResult, ApplyEditsRequest, ApplyEditsResult, CellValue, CellValueRequest, ChartSpec,
    ColumnInfo, ColumnMeta, ConstraintInfo, ConstraintKind, DEFAULT_LIMIT, DEFAULT_MAX_ROWS,
    DEFAULT_TIMEOUT, DocumentExportRequest, DocumentFormat, EditFailure, EditKind, EditOperation,
    EditableRequest, EditableResult, EjsonFlavor, ExportRequest, ExportResult, ForeignKeyInfo,
    GuardCheck, IndexInfo, InsertMode, MAX_LIMIT, MAX_ROWS_PER_STATEMENT, MongoCollectionInfo,
    MongoCollectionKind, MongoCollectionStats, MongoIndexInfo, QueryProgress, QueryRun, RunRequest,
    RunSort, SNAPSHOT_MAX_COLUMNS, SNAPSHOT_MAX_TABLES, SchemaSnapshot, SnapshotColumn,
    SnapshotTable, SqlExportRequest, SqlOptions, StatementResult, TableExportRequest, TableInfo,
    TableKind, TableProperties, TypedValue, WarningCode, WriteInfo, WriteStatement, WriteWarning,
};
use crate::commands::MongoUriParts;

fn decl<T: TS + 'static>(cfg: &ts_rs::Config) -> (String, String) {
    (T::ident(cfg), format!("export {}", T::decl(cfg)))
}

fn types(cfg: &ts_rs::Config) -> Vec<(String, String)> {
    let mut all = vec![
        decl::<AiReadResult>(cfg),
        decl::<AiSettings>(cfg),
        decl::<AiSettingsView>(cfg),
        decl::<AiTestResult>(cfg),
        decl::<ApplyEditsRequest>(cfg),
        decl::<ApplyEditsResult>(cfg),
        decl::<AppError>(cfg),
        decl::<CellValueRequest>(cfg),
        decl::<ChartSpec>(cfg),
        decl::<ChatEvent>(cfg),
        decl::<ChatRequest>(cfg),
        decl::<ChatRole>(cfg),
        decl::<ChatSource>(cfg),
        decl::<ChatTurn>(cfg),
        decl::<ColumnInfo>(cfg),
        decl::<ColumnMeta>(cfg),
        decl::<Connection>(cfg),
        decl::<ConnectionInput>(cfg),
        decl::<ConstraintInfo>(cfg),
        decl::<ConstraintKind>(cfg),
        decl::<DocumentExportRequest>(cfg),
        decl::<DocumentFormat>(cfg),
        decl::<EditFailure>(cfg),
        decl::<EditKind>(cfg),
        decl::<EditOperation>(cfg),
        decl::<EditableRequest>(cfg),
        decl::<EditableResult>(cfg),
        decl::<EjsonFlavor>(cfg),
        decl::<Engine>(cfg),
        decl::<Env>(cfg),
        decl::<ErrorCode>(cfg),
        decl::<ExportRequest>(cfg),
        decl::<ExportResult>(cfg),
        decl::<ForeignKeyInfo>(cfg),
        decl::<GuardCheck>(cfg),
        decl::<IndexInfo>(cfg),
        decl::<InsertMode>(cfg),
        decl::<MongoCollectionInfo>(cfg),
        decl::<MongoCollectionKind>(cfg),
        decl::<MongoCollectionStats>(cfg),
        decl::<MongoIndexInfo>(cfg),
        decl::<MongoUriParts>(cfg),
        decl::<Privacy>(cfg),
        decl::<Provider>(cfg),
        decl::<QueryProgress>(cfg),
        decl::<QueryRun>(cfg),
        decl::<RunRequest>(cfg),
        decl::<RunSort>(cfg),
        decl::<SchemaSnapshot>(cfg),
        decl::<SnapshotColumn>(cfg),
        decl::<SnapshotTable>(cfg),
        decl::<SqlExportRequest>(cfg),
        decl::<SqlOptions>(cfg),
        decl::<SslMode>(cfg),
        decl::<StatementResult>(cfg),
        decl::<TabStatus>(cfg),
        decl::<TableExportRequest>(cfg),
        decl::<TableInfo>(cfg),
        decl::<TableKind>(cfg),
        decl::<TableProperties>(cfg),
        decl::<TestResult>(cfg),
        decl::<ToolKind>(cfg),
        decl::<ToolResultData>(cfg),
        decl::<Topology>(cfg),
        decl::<TypedValue>(cfg),
        decl::<WarningCode>(cfg),
        decl::<WriteInfo>(cfg),
        decl::<WriteStatement>(cfg),
        decl::<WriteWarning>(cfg),
    ];
    all.extend(
        crate::commands::arg_declarations(cfg)
            .into_iter()
            .map(|(name, declaration)| (name.to_string(), format!("export {declaration}"))),
    );
    all.extend([decl::<serde_json::Value>(cfg)]);
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

/// A TypeScript array literal of strings, for the generated constants.
fn js_strings(items: &[&str]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("{s:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

/// The export dialog's row-conflict clauses per engine, keyed as `Engine` serializes.
fn export_conflict() -> String {
    let mut clauses = serde_json::Map::new();
    for engine in [SqlEngine::Pg, SqlEngine::Mysql, SqlEngine::Sqlite] {
        let text = conflict_text(engine);
        let key = serde_json::to_value(engine.engine()).expect("an engine serializes to its key");
        clauses.insert(
            key.as_str()
                .expect("an engine serializes to a string")
                .to_string(),
            serde_json::json!({ "skip": text.skip, "upsert": text.upsert }),
        );
    }
    serde_json::Value::Object(clauses).to_string()
}

fn constants() -> String {
    [
        ("PREVIEW_BYTES", PREVIEW_BYTES.to_string()),
        ("DEFAULT_LIMIT", DEFAULT_LIMIT.to_string()),
        ("MAX_LIMIT", MAX_LIMIT.to_string()),
        ("MAX_ROWS_PER_STATEMENT", MAX_ROWS_PER_STATEMENT.to_string()),
        ("SNAPSHOT_MAX_COLUMNS", SNAPSHOT_MAX_COLUMNS.to_string()),
        ("SNAPSHOT_MAX_TABLES", SNAPSHOT_MAX_TABLES.to_string()),
        ("UI_ROWS", UI_ROWS.to_string()),
        ("CHART_ROWS", CHART_ROWS.to_string()),
        ("MAX_SOURCES", MAX_SOURCES.to_string()),
        ("MAX_STEPS", MAX_STEPS.to_string()),
        ("DEFAULT_MAX_ROWS", DEFAULT_MAX_ROWS.to_string()),
        ("DEFAULT_TIMEOUT_SEC", DEFAULT_TIMEOUT.as_secs().to_string()),
        ("MAX_ROWS_LIMIT", MAX_ROWS_LIMIT.to_string()),
        ("TIMEOUT_SEC_LIMIT", TIMEOUT_SEC_LIMIT.to_string()),
        ("CHART_TYPES", js_strings(&CHART_TYPES)),
        ("DEFAULT_AI_BASE_URL", format!("{DEFAULT_AI_BASE_URL:?}")),
        ("DEFAULT_AI_MODEL", format!("{DEFAULT_AI_MODEL:?}")),
        ("MONGOSH_COLLECTION_METHODS", js_strings(COLLECTION_METHODS)),
        ("MONGOSH_CURSOR_METHODS", js_strings(CURSOR_METHODS)),
        ("MONGOSH_DB_METHODS", js_strings(DB_METHODS)),
        ("MONGO_WRITABLE_TYPES", js_strings(WRITABLE_TYPES)),
        ("EXPORT_CONFLICT", export_conflict()),
    ]
    .into_iter()
    .fold(
        String::from("// This file is generated by `cargo test export_bindings`. Do not edit.\n\n"),
        |mut out, (name, value)| {
            out.push_str(&format!("export const {name} = {value};\n"));
            out
        },
    )
}

fn write(dir: &PathBuf, name: &str, generated: String) -> bool {
    let path = dir.join(name);
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&path, &generated).unwrap();
    previous == generated
}

#[test]
fn export_bindings() {
    let cfg = ts_rs::Config::new().with_large_int("number");
    let mut wire =
        String::from("// This file is generated by `cargo test export_bindings`. Do not edit.\n\n");
    wire.push_str(&format!("export type CellValue = {};\n", CellValue::ident(&cfg)));
    wire.push('\n');
    wire.push_str(&crate::commands::command_name_declaration());
    let mut previous = String::new();
    for (name, declaration) in types(&cfg) {
        assert_ne!(previous, name, "two wire types are named {name}");
        previous = name.clone();
        wire.push('\n');
        wire.push_str(&declaration);
        wire.push('\n');
    }

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/lib/api/generated");
    let wire_ok = write(&dir, "wire.ts", wire);
    let constants_ok = write(&dir, "constants.ts", constants());
    assert!(
        wire_ok && constants_ok,
        "the wire contract in {} was out of date and has been regenerated; commit it",
        dir.display()
    );
}
