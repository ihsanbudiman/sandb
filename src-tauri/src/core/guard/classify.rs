use serde::Serialize;

use crate::core::engine::SqlEngine;
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{GuardCheck, WarningCode, WriteInfo, WriteStatement, WriteWarning};
use crate::core::sql::facts::{
    self, PragmaForm, ReadProfile, ReadVerdict, SqlFacts, StatementFacts, StatementKind,
};

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

/// MySQL/MariaDB tables that show other sessions' work, regardless of their schema.
const MYSQL_PRIVATE_TABLES: &[&str] = &["processlist", "innodb_trx", "innodb_lock_waits"];

/// Tables a statement reads that must not be shown to the AI: they carry other sessions' query
/// text or other users' credentials.
fn private_reads(engine: SqlEngine, s: &StatementFacts) -> Vec<String> {
    let mut out = Vec::new();
    for t in &s.tables {
        // Lowercase on both engines: MySQL is case-insensitive for these names, and a quoted
        // PG name that does not match exactly is still safer to block.
        let t = t.to_lowercase();
        let last = t.rsplit('.').next().unwrap_or(&t);
        match engine {
            SqlEngine::Pg => {
                if PG_PRIVATE_TABLES.contains(&last) {
                    out.push(t.clone());
                }
            }
            SqlEngine::Mysql => {
                let schema = t.split('.').next().unwrap_or("");
                // The whole `mysql`/`performance_schema`/`sys` schema holds credentials and
                // query logs; the data dictionary (`information_schema`) is public metadata.
                if MYSQL_PRIVATE_TABLES.contains(&last) || engine.is_private_schema(schema) {
                    out.push(t.clone());
                }
            }
            SqlEngine::Sqlite => {}
        }
    }
    out.sort();
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadOk {
    /// `SELECT`, `SHOW`, `DESCRIBE`, `EXPLAIN` or `PRAGMA`.
    pub kind: String,
}

/// Why AI-written SQL was refused. `reason` is written for the model, so it can rewrite the query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuardRejection {
    pub reason: String,
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

pub fn guard_check(sql: &str, engine: SqlEngine) -> GuardCheck {
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

/// The AI read policy: exactly one statement that only reads, parsed with the connection's
/// dialect (MariaDB uses MySQL's), plus the AI-only rules.
pub fn check_ai_read(sql: &str, engine: SqlEngine) -> Result<ReadOk, GuardRejection> {
    if sql.trim().is_empty() {
        return Err(GuardRejection::new("The query is empty"));
    }
    let facts = facts::analyze(engine, sql).map_err(|e| GuardRejection {
        detail: Some(e.to_string()),
        ..GuardRejection::new(format!(
            "Could not parse the SQL as {}; write one plain read query",
            engine.label()
        ))
    })?;
    if let Some(reason) = facts::executable_comment_error(&facts, ReadProfile::AiRead) {
        return Err(GuardRejection::new(reason));
    }
    if facts.has_optimizer_hint {
        return Err(GuardRejection::new(
            "Optimizer hint comments (/*+ … */) are not allowed",
        ));
    }
    let verdict = match facts::read_policy(&facts, ReadProfile::AiRead) {
        Ok(mut v) => v.remove(0),
        Err(reason) => return Err(GuardRejection::new(reason)),
    };
    let s = &facts.statements[0];
    let kind = match verdict {
        ReadVerdict::Read { kind } => kind,
        ReadVerdict::Refused(reason) => return Err(GuardRejection::of(s, reason)),
        // The AI profile never allows a mode statement.
        ReadVerdict::Mode => return Err(GuardRejection::of(s, "Not a read")),
    };
    if s.kind == StatementKind::Pragma {
        check_pragma(engine, s)?;
    }
    check_privacy(engine, &facts, s)?;
    Ok(ReadOk {
        kind: kind.to_string(),
    })
}

fn check_pragma(engine: SqlEngine, s: &StatementFacts) -> Result<(), GuardRejection> {
    let Some(p) = &s.pragma else {
        return Err(GuardRejection::of(s, "Unsupported PRAGMA"));
    };
    if !engine.is_read_pragma(&p.name) {
        return Err(GuardRejection::of(
            s,
            format!(
                "PRAGMA {} is not allowed; only {} are",
                p.name,
                engine.read_pragmas().join(", ")
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

/// What the AI may not see: other sessions' SQL and other users' credentials.
fn check_privacy(
    engine: SqlEngine,
    facts: &SqlFacts,
    s: &StatementFacts,
) -> Result<(), GuardRejection> {
    if s.show_processlist {
        return Err(GuardRejection::of(
            s,
            "SHOW PROCESSLIST is blocked for privacy: it shows the SQL other sessions run",
        ));
    }
    // `SHOW x` statements the parser reports as ShowVariable: the first word decides.
    match (engine, s.show_variable.as_deref()) {
        (SqlEngine::Mysql, Some("engine")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW ENGINE … is blocked for privacy: engine status exposes other sessions' internals",
            ));
        }
        (SqlEngine::Mysql, Some("binlog" | "relaylog")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW BINLOG/RELAYLOG EVENTS is blocked for privacy: it shows the statements other sessions ran",
            ));
        }
        (SqlEngine::Mysql, Some("grants")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW GRANTS is blocked for privacy: it lists user privileges",
            ));
        }
        (SqlEngine::Pg, Some("all")) => {
            return Err(GuardRejection::of(
                s,
                "SHOW ALL is blocked for privacy: some settings hold connection strings",
            ));
        }
        (SqlEngine::Pg, Some(v)) if v.contains("conninfo") => {
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
    Ok(())
}

/// What a proposed write would do, for the approval preview. Never refuses: unknown SQL gets a
/// warning instead, and the user decides.
pub fn classify_write(sql: &str, engine: SqlEngine) -> WriteInfo {
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
                        engine.label()
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
    // The write path promises one transaction; on MySQL a DDL or other implicit commit breaks
    // that promise, so the approval says so before the user decides. The fact is parsed, not the
    // conservative probe rule (`session::may_end_tx`), which is true for every `Other` statement
    // (`SET FOREIGN_KEY_CHECKS`, `USE`, …) where MySQL commits nothing.
    if facts.statements.iter().any(|s| s.implicit_commit) {
        warnings.push(WriteWarning {
            code: WarningCode::ImplicitCommit,
            message: "MySQL may commit a statement here immediately (DDL, LOCK TABLES, SET \
                      autocommit, CALL, GRANT); a failure after it cannot undo the statements \
                      before it"
                .into(),
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
            structure: Some(s.kind.is_ddl()),
        });
    }
    WriteInfo {
        statements,
        warnings,
    }
}
