use serde::Serialize;

use crate::core::connections::Engine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::sql::facts::{self, PragmaForm, SqlFacts, StatementFacts, StatementKind};
use crate::core::sql::functions;

/// Functions that produce arbitrarily large values: the row cap and per-cell preview cap are
/// applied after the value is in memory, so `repeat('x', 500000000)` can exhaust it. The AI
/// policy refuses them; the editor's `is_plain_read` keeps the shared allow-list.
const AMPLIFYING: &[&str] = &[
    "repeat",
    "lpad",
    "rpad",
    "space",
    "zeroblob",
    "randomblob",
    "format",
    "printf",
];

/// PG catalog objects that expose other sessions' SQL or other users' credentials, matched on
/// the last name part so `public.pg_authid` and `pg_catalog.pg_authid` are both caught.
const PG_PRIVATE_TABLES: &[&str] = &[
    "pg_stat_activity",
    "pg_stat_statements",
    "pg_authid",
    "pg_shadow",
    "pg_user_mappings",
    // Every setting row, not only `*conninfo*`: simpler and fail closed, and the AI can use
    // `SHOW <name>` for the one setting it needs.
    "pg_settings",
];

/// Tables a statement reads that must not be shown to the AI: they carry other sessions' query
/// text or other users' credentials. `SHOW PROCESSLIST` is checked separately in
/// `check_statement`.
fn private_reads(engine: Engine, s: &StatementFacts) -> Vec<String> {
    let mut out = Vec::new();
    for t in &s.tables {
        // Lowercase on both engines: MySQL is case-insensitive for these names, and a quoted
        // PG name that does not match exactly is still safer to block.
        let t = t.to_lowercase();
        let last = t.rsplit('.').next().unwrap_or(&t);
        match engine {
            Engine::Pg => {
                if PG_PRIVATE_TABLES.contains(&last) {
                    out.push(t.clone());
                }
            }
            Engine::Mysql => {
                let schema = t.split('.').next().unwrap_or("");
                // `processlist`, `innodb_trx` and `innodb_lock_waits` show other sessions; the
                // whole `mysql` system schema holds credentials and query logs (`user`,
                // `global_priv`, `general_log`, `slow_log`, `db`, …).
                if last == "processlist"
                    || last == "innodb_trx"
                    || last == "innodb_lock_waits"
                    || schema == "performance_schema"
                    || schema == "sys"
                    || schema == "mysql"
                {
                    out.push(t.clone());
                }
            }
            Engine::Sqlite | Engine::Mongo => {}
        }
    }
    out.sort();
    out
}

/// SQLite pragmas the AI may run: they only describe the schema.
const READ_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "foreign_key_list",
];

/// The SQL passed the AI read policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadOk {
    /// `SELECT`, `SHOW`, `DESCRIBE`, `EXPLAIN` or `PRAGMA`.
    pub kind: String,
}

/// Why AI-written SQL was refused. `reason` is written for the model, so it can rewrite the query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuardRejection {
    pub reason: String,
    /// Parser output or other technical detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The statement kind, when the SQL parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl GuardRejection {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            detail: None,
            kind: None,
        }
    }

    fn of(s: &StatementFacts, reason: impl Into<String>) -> Self {
        Self {
            kind: Some(s.label.clone()),
            ..Self::new(reason)
        }
    }
}

impl From<GuardRejection> for AppError {
    fn from(r: GuardRejection) -> Self {
        let mut e = AppError::new(
            ErrorCode::GuardRejected,
            format!("Blocked by the read-only guard: {}", r.reason),
        );
        e.detail = r.detail;
        e
    }
}

/// [`check_ai_read`] as one value for the UI and the bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuardCheck {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

pub fn guard_check(sql: &str, engine: Engine) -> GuardCheck {
    if engine == Engine::Mongo {
        return crate::core::mongo::guard::guard_check(sql);
    }
    match check_ai_read(sql, engine) {
        Ok(ok) => GuardCheck {
            allowed: true,
            reason: None,
            kind: Some(ok.kind),
        },
        Err(r) => GuardCheck {
            allowed: false,
            reason: Some(r.reason),
            kind: r.kind,
        },
    }
}

const READ_KINDS: &str = "Only one SELECT, WITH … SELECT, VALUES, SHOW, DESCRIBE or EXPLAIN \
                          (without ANALYZE) statement is allowed";

/// The AI read policy. Allows exactly one statement that only reads, parsed with the dialect of
/// the connection (MariaDB uses MySQL's). Anything the parser cannot read is refused.
pub fn check_ai_read(sql: &str, engine: Engine) -> Result<ReadOk, GuardRejection> {
    if sql.trim().is_empty() {
        return Err(GuardRejection::new("The query is empty"));
    }
    if engine == Engine::Mongo {
        return Err(GuardRejection::new(
            "MongoDB queries are not available in the assistant yet",
        ));
    }
    let facts = facts::analyze(engine, sql).map_err(|e| GuardRejection {
        detail: Some(e.to_string()),
        ..GuardRejection::new(format!(
            "Could not parse the SQL as {}; write one plain read query",
            dialect_name(engine)
        ))
    })?;
    if facts.has_executable_comment {
        return Err(GuardRejection::new(
            "MySQL executable comments (/*! … */ or /*M! … */) are not allowed",
        ));
    }
    if facts.has_optimizer_hint {
        return Err(GuardRejection::new(
            "Optimizer hint comments (/*+ … */) are not allowed",
        ));
    }
    let s = match facts.statements.as_slice() {
        [] => return Err(GuardRejection::new("The query has no statement")),
        [s] => s,
        many => {
            return Err(GuardRejection::new(format!(
                "Exactly one statement is allowed, found {}",
                many.len()
            )));
        }
    };
    let kind = check_statement(&facts, s)?;
    Ok(ReadOk { kind })
}

fn dialect_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Pg => "PostgreSQL",
        Engine::Mysql => "MySQL/MariaDB",
        Engine::Sqlite => "SQLite",
        Engine::Mongo => "MongoDB",
    }
}

fn check_statement(facts: &SqlFacts, s: &StatementFacts) -> Result<String, GuardRejection> {
    let kind = match s.kind {
        StatementKind::Query => "SELECT",
        StatementKind::Show => {
            if s.label == "DESCRIBE" {
                "DESCRIBE"
            } else {
                "SHOW"
            }
        }
        StatementKind::Explain { analyze: true } => {
            return Err(GuardRejection::of(
                s,
                "EXPLAIN ANALYZE runs the statement; use EXPLAIN without ANALYZE",
            ));
        }
        StatementKind::Explain { analyze: false } => {
            if s.explained.as_deref() != Some(&StatementKind::Query) {
                return Err(GuardRejection::of(
                    s,
                    "EXPLAIN is only allowed for a SELECT query",
                ));
            }
            "EXPLAIN"
        }
        StatementKind::Pragma if facts.engine == Engine::Sqlite => {
            return check_pragma(s).map(|()| "PRAGMA".to_string());
        }
        _ => {
            return Err(GuardRejection::of(
                s,
                format!("{} is not a read. {READ_KINDS}", s.label),
            ));
        }
    };
    if s.has_into {
        return Err(GuardRejection::of(
            s,
            "SELECT … INTO (a table, file or variable) is not allowed",
        ));
    }
    if s.has_locking {
        return Err(GuardRejection::of(
            s,
            "Locking reads (FOR UPDATE / FOR SHARE / LOCK IN SHARE MODE) are not allowed",
        ));
    }
    if s.has_modifying_cte {
        return Err(GuardRejection::of(
            s,
            "A CTE or subquery that changes data (INSERT / UPDATE / DELETE / MERGE) is not allowed",
        ));
    }
    if s.has_variable_assignment {
        return Err(GuardRejection::of(
            s,
            "Assigning user variables (@v := …) is not allowed",
        ));
    }
    // `x.f` can be the attribute notation call `f(x)`, which `s.functions` never records.
    let attr = facts::attribute_calls(facts, s);
    if !attr.is_empty() {
        return Err(GuardRejection::of(
            s,
            format!(
                "Possible attribute-notation function call{}: {}. PostgreSQL reads x.f as f(x); \
                 write the column or an allowed function call instead",
                if attr.len() == 1 { "" } else { "s" },
                attr.join(", ")
            ),
        ));
    }
    let amplifying: Vec<&str> = s
        .functions
        .iter()
        .filter(|f| AMPLIFYING.contains(&f.as_str()))
        .map(String::as_str)
        .collect();
    if !amplifying.is_empty() {
        return Err(GuardRejection::of(
            s,
            format!(
                "Function{} can build arbitrarily large values and is not allowed: {}",
                if amplifying.len() == 1 { "" } else { "s" },
                amplifying.join(", ")
            ),
        ));
    }
    let blocked: Vec<&str> = s
        .functions
        .iter()
        .filter(|f| !functions::is_pure(facts.engine, f))
        .map(String::as_str)
        .collect();
    if !blocked.is_empty() {
        return Err(GuardRejection::of(
            s,
            format!(
                "Function{} not on the read-only allow-list: {}. Use built-in string, math, \
                 date, JSON, aggregate or window functions instead",
                if blocked.len() == 1 { "" } else { "s" },
                blocked.join(", ")
            ),
        ));
    }
    if s.show_processlist {
        return Err(GuardRejection::of(
            s,
            "SHOW PROCESSLIST is blocked for privacy: it shows the SQL other sessions run",
        ));
    }
    // `SHOW x` statements the parser reports as ShowVariable: the first word decides.
    match (facts.engine, s.show_variable.as_deref()) {
        (Engine::Mysql, Some("engine")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW ENGINE … is blocked for privacy: engine status exposes other sessions' internals",
            ));
        }
        (Engine::Mysql, Some("binlog" | "relaylog")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW BINLOG/RELAYLOG EVENTS is blocked for privacy: it shows the statements other sessions ran",
            ));
        }
        (Engine::Mysql, Some("grants")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW GRANTS is blocked for privacy: it lists user privileges",
            ));
        }
        (Engine::Pg, Some("all")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW ALL is blocked for privacy: some settings hold connection strings",
            ));
        }
        (Engine::Pg, Some(v)) if v.contains("conninfo") => {
            return Err(GuardRejection::of(
                s,
                "This SHOW is blocked for privacy: the setting can hold a password",
            ));
        }
        _ => {}
    }
    let private = private_reads(facts.engine, s);
    if !private.is_empty() {
        return Err(GuardRejection::of(
            s,
            format!(
                "{} is blocked for privacy: it exposes data about other sessions or users",
                private.join(", ")
            ),
        ));
    }
    Ok(kind.to_string())
}
fn check_pragma(s: &StatementFacts) -> Result<(), GuardRejection> {
    let Some(p) = &s.pragma else {
        return Err(GuardRejection::of(s, "Unsupported PRAGMA"));
    };
    if !READ_PRAGMAS.contains(&p.name.as_str()) {
        return Err(GuardRejection::of(
            s,
            format!(
                "PRAGMA {} is not allowed; only {} are",
                p.name,
                READ_PRAGMAS.join(", ")
            ),
        ));
    }
    if p.form != PragmaForm::Call {
        return Err(GuardRejection::of(
            s,
            format!("Only the read form PRAGMA {}(table) is allowed", p.name),
        ));
    }
    Ok(())
}

// ---- write preview ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteStatement {
    /// `INSERT`, `UPDATE`, `DROP TABLE`, `SELECT`, …
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// MongoDB only: the database the statement runs on, from the tab's database and following
    /// `use`, so the confirmation names it even when `table` is absent (`db.dropDatabase()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// MongoDB only: the statement only reads. The editor asks for a confirmation before a run on
    /// a Production connection when any statement has `read: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    UpdateWithoutWhere,
    DeleteWithoutWhere,
    Drop,
    Truncate,
    AlterDrop,
    MultipleStatements,
    Unparsable,
    ExecutableComment,
    /// MongoDB: `renameCollection` cannot be undone.
    Rename,
    /// MongoDB: the guard cannot say what the statement does.
    UnknownCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteWarning {
    pub code: WarningCode,
    pub message: String,
    /// 0-based statement the warning is about; absent for the whole text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteInfo {
    pub statements: Vec<WriteStatement>,
    pub warnings: Vec<WriteWarning>,
}

/// What a proposed write would do, for the approval preview. Never refuses: unknown SQL gets a
/// warning instead, and the user decides.
pub fn classify_write(sql: &str, engine: Engine, database: Option<&str>) -> WriteInfo {
    if engine == Engine::Mongo {
        return crate::core::mongo::guard::classify_write(sql, database);
    }
    let mut warnings = Vec::new();
    let facts = match facts::analyze(engine, sql) {
        Ok(f) => f,
        Err(e) => {
            return WriteInfo {
                statements: Vec::new(),
                warnings: vec![WriteWarning {
                    code: WarningCode::Unparsable,
                    message: format!(
                        "Could not parse the SQL as {}, so its effect is unknown ({e})",
                        dialect_name(engine)
                    ),
                    statement: None,
                }],
            };
        }
    };
    if facts.has_executable_comment {
        warnings.push(WriteWarning {
            code: WarningCode::ExecutableComment,
            message: "Contains a MySQL executable comment (/*! … */); the server runs SQL that \
                      this preview may not show"
                .into(),
            statement: None,
        });
    }
    if facts.statement_count() > 1 {
        warnings.push(WriteWarning {
            code: WarningCode::MultipleStatements,
            message: format!("{} statements will run", facts.statement_count()),
            statement: None,
        });
    }
    let mut statements = Vec::with_capacity(facts.statement_count());
    for (i, s) in facts.statements.iter().enumerate() {
        let table = s.target.clone();
        let on = table.as_deref().unwrap_or("the table");
        let mut warn = |code, message: String| {
            warnings.push(WriteWarning {
                code,
                message,
                statement: Some(i),
            })
        };
        match (s.kind, s.has_where) {
            (StatementKind::Update, Some(false)) => warn(
                WarningCode::UpdateWithoutWhere,
                format!("UPDATE without WHERE changes every row of {on}"),
            ),
            (StatementKind::Delete, Some(false)) => warn(
                WarningCode::DeleteWithoutWhere,
                format!("DELETE without WHERE removes every row of {on}"),
            ),
            (StatementKind::Drop, _) => warn(
                WarningCode::Drop,
                format!("{} {on} cannot be undone", s.label),
            ),
            (StatementKind::Truncate, _) => warn(
                WarningCode::Truncate,
                format!("TRUNCATE removes every row of {on}"),
            ),
            (StatementKind::Alter, _) if s.alter_drops => warn(
                WarningCode::AlterDrop,
                format!("ALTER … DROP removes part of {on}"),
            ),
            _ => {}
        }
        statements.push(WriteStatement {
            kind: s.label.clone(),
            table,
            database: None,
            read: None,
        });
    }
    WriteInfo {
        statements,
        warnings,
    }
}
