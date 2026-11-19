//! Editor, production-preview and AI checks share the same Redis command classification.

use crate::core::{
    connections::{self, Connection},
    error::{AppError, ErrorCode},
    result::{GuardCheck, WarningCode, WriteInfo, WriteStatement, WriteWarning},
};

use super::{
    command::{self, Kind},
    key::quote_arg,
    parse::{self, Statement},
};

pub fn check_supported(statements: &[Statement]) -> Result<(), AppError> {
    for stmt in statements {
        if let Kind::Unsupported(reason) = command::classify(stmt) {
            return Err(AppError::new(ErrorCode::Unsupported, reason));
        }
    }
    Ok(())
}

pub fn check_read_only(conn: &Connection, statements: &[Statement]) -> Result<(), AppError> {
    let mut non_read = None;
    for stmt in statements {
        match command::classify(stmt) {
            Kind::Read | Kind::Select => {}
            Kind::Unsupported(reason) => return Err(AppError::new(ErrorCode::Unsupported, reason)),
            _ => {
                non_read.get_or_insert(stmt);
            }
        }
    }
    if let Some(stmt) = non_read {
        return Err(connections::read_only_refusal(conn, Some(&not_read(stmt))));
    }
    Ok(())
}

pub fn guard_check(text: &str) -> GuardCheck {
    let reason = match ai_read_statement(text) {
        Ok(stmt) => {
            return GuardCheck {
                allowed: true,
                reason: None,
                kind: Some(command::name(&stmt)),
            };
        }
        Err(reason) => reason,
    };
    GuardCheck {
        allowed: false,
        reason: Some(reason),
        kind: None,
    }
}

pub(super) fn ai_read_statement(text: &str) -> Result<Statement, String> {
    let stmt = read_statement(text)?;
    if let Some(name @ ("CONFIG GET" | "ACL LIST" | "CLIENT LIST" | "CLIENT INFO")) =
        command::known_name(&stmt)
    {
        return Err(format!(
            "{name} can expose private server details. Use the editor instead."
        ));
    }
    Ok(stmt)
}

pub(super) fn read_statement(text: &str) -> Result<Statement, String> {
    let mut statements = parse::parse(text).map_err(|err| err.to_string())?;
    let [stmt] = statements.as_slice() else {
        return Err(if statements.is_empty() {
            "The query has no statement".into()
        } else {
            format!(
                "Exactly one statement is allowed, found {}",
                statements.len()
            )
        });
    };
    match command::classify(stmt) {
        Kind::Read if stmt.args[0].eq_ignore_ascii_case(b"KEYS") => {
            Err("Use SCAN with MATCH and COUNT instead of KEYS.".into())
        }
        Kind::Read => Ok(statements.pop().unwrap()),
        Kind::Unsupported(reason) => Err(reason.into()),
        _ => Err(not_read(stmt)),
    }
}

fn not_read(stmt: &Statement) -> String {
    format!("{} is not a read", command::name(stmt))
}

pub fn classify_write(text: &str, database: Option<&str>) -> WriteInfo {
    match parse::parse(text) {
        Ok(statements) => classify_statements(&statements, database),
        Err(err) => WriteInfo {
            statements: Vec::new(),
            warnings: vec![WriteWarning {
                code: WarningCode::Unparsable,
                message: err.to_string(),
                statement: None,
            }],
        },
    }
}

fn classify_statements(statements: &[Statement], database: Option<&str>) -> WriteInfo {
    let mut info = WriteInfo {
        statements: Vec::new(),
        warnings: Vec::new(),
    };
    let mut database = database.filter(|db| !db.is_empty()).unwrap_or("0");
    for (index, stmt) in statements.iter().enumerate() {
        let kind = command::classify(stmt);
        match kind {
            Kind::Read => continue,
            Kind::Select => {
                match command::select_database(stmt) {
                    Ok(db) => {
                        let normalized = db.trim_start_matches('0');
                        database = if normalized.is_empty() {
                            "0"
                        } else {
                            normalized
                        };
                    }
                    Err(err) => info.warnings.push(WriteWarning {
                        code: WarningCode::Unparsable,
                        message: err.message,
                        statement: Some(index),
                    }),
                }
                continue;
            }
            _ => {}
        }
        let name = command::name(stmt);
        if let Some((code, message)) = warning(kind, &name, database) {
            info.warnings.push(WriteWarning {
                code,
                message,
                statement: Some(index),
            });
        }
        info.statements.push(WriteStatement {
            kind: name,
            table: command::first_key(stmt).map(quote_arg),
            database: Some(database.into()),
            read: Some(false),
            structure: Some(matches!(kind, Kind::Admin | Kind::Flush)),
        });
    }
    info
}

fn warning(kind: Kind, name: &str, database: &str) -> Option<(WarningCode, String)> {
    match kind {
        Kind::Flush => Some((
            WarningCode::Flush,
            if name == "FLUSHDB" {
                format!("FLUSHDB deletes every key of database {database}")
            } else {
                "FLUSHALL deletes every key of every database".into()
            },
        )),
        Kind::Admin => Some((
            WarningCode::ServerAdmin,
            format!("{name} changes the server, not only data"),
        )),
        Kind::Script => Some((
            WarningCode::UnknownCommand,
            format!("{name} runs a script; sandb cannot tell which keys it changes"),
        )),
        Kind::Unknown => Some((WarningCode::UnknownCommand, unknown(name))),
        _ => None,
    }
}

fn unknown(name: &str) -> String {
    format!("sandb does not know what {name} does")
}

pub fn write_preview(text: &str, database: &str) -> Result<WriteInfo, AppError> {
    let stmt = write_statement(text)?;
    Ok(classify_statements(
        std::slice::from_ref(&stmt),
        Some(database),
    ))
}

pub(super) fn write_statement(text: &str) -> Result<Statement, AppError> {
    let mut statements = parse::parse(text)?;
    let [stmt] = statements.as_slice() else {
        return Err(AppError::invalid(
            "Exactly one Redis write command is allowed",
        ));
    };
    match command::classify(stmt) {
        Kind::Read => Err(AppError::invalid("This is a read. Use run_redis_read.")),
        Kind::Select => Err(AppError::invalid("SELECT is not a write")),
        Kind::Unknown => Err(AppError::invalid(unknown(&command::name(stmt)))),
        Kind::Unsupported(reason) => Err(AppError::new(ErrorCode::Unsupported, reason)),
        Kind::Write | Kind::Admin | Kind::Flush | Kind::Script => Ok(statements.pop().unwrap()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        serde_json::from_value(serde_json::json!({
            "id": "redis", "name": "Cache", "engine": "redis", "readOnly": true
        }))
        .unwrap()
    }

    #[test]
    fn read_only_allows_reads_and_local_select_but_fails_closed() {
        let conn = connection();
        let read = parse::parse("SELECT 1\nEVAL_RO 'return 1' 0\nCONFIG GET databases").unwrap();
        check_read_only(&conn, &read).unwrap();
        for (text, name) in [
            ("EVAL 'return 1' 0", "EVAL"),
            ("FUTURE key", "FUTURE"),
            ("CONFIG FUTURE key", "CONFIG FUTURE"),
            ("CONFIG SET key value", "CONFIG SET"),
            ("SORT key STORE dest", "SORT"),
            ("XREADGROUP GROUP g c STREAMS key >", "XREADGROUP"),
        ] {
            let err = check_read_only(&conn, &parse::parse(text).unwrap()).unwrap_err();
            assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
            assert_eq!(
                err.message,
                format!("Connection \"Cache\" is read-only: {name} is not a read")
            );
        }
    }

    #[test]
    fn unsupported_is_found_before_any_command_runs() {
        let conn = connection();
        for text in [
            "MULTI",
            "SUBSCRIBE key",
            "XREAD BLOCK 0 STREAMS key 0",
            "CLIENT REPLY OFF",
            "CLIENT TRACKING ON",
            "RESET",
            "HELLO 2",
            "AUTH something",
            "ASKING",
        ] {
            let statements = parse::parse(&format!("SET k v\n{text}")).unwrap();
            let err = check_supported(&statements).unwrap_err();
            assert_eq!(err.code, ErrorCode::Unsupported);
            assert_eq!(
                check_read_only(&conn, &statements).unwrap_err().message,
                err.message
            );
        }
    }

    #[test]
    fn ai_read_guard_requires_exactly_one_safe_command() {
        assert_eq!(
            guard_check("# comment\n EVAL_RO 'return 1' 0")
                .kind
                .as_deref(),
            Some("EVAL_RO")
        );
        assert!(guard_check("SCAN 0 MATCH user:* COUNT 100").allowed);
        for (text, reason) in [
            ("KEYS *", "Use SCAN with MATCH and COUNT instead of KEYS."),
            (
                "cOnFiG \"gEt\" databases",
                "CONFIG GET can expose private server details. Use the editor instead.",
            ),
            (
                "ACL LIST",
                "ACL LIST can expose private server details. Use the editor instead.",
            ),
            (
                "CLIENT LIST",
                "CLIENT LIST can expose private server details. Use the editor instead.",
            ),
            (
                "CLIENT INFO",
                "CLIENT INFO can expose private server details. Use the editor instead.",
            ),
            ("SELECT 0", "SELECT is not a read"),
            ("CONFIG FUTURE", "CONFIG FUTURE is not a read"),
            (
                "XREAD BLOCK 0 STREAMS k 0",
                "Blocking commands are not supported.",
            ),
            ("PING\nPING", "Exactly one statement is allowed, found 2"),
            ("# comment", "The query has no statement"),
            ("GET '\n", "Line 1, column 5: Unclosed quote"),
        ] {
            let check = guard_check(text);
            assert!(!check.allowed);
            assert_eq!(check.reason.as_deref(), Some(reason));
            assert_eq!(check.kind, None);
        }
    }

    #[test]
    fn classifies_dangerous_writes_with_effective_database_and_keys() {
        let info = classify_write(
            "GET k\nSELECT 003\nFLUSHDB\nFLUSHALL\nCONFIG SET key value\nEVAL 'return 1' 1 '\\key'\nCONFIG FUTURE\nSET \"\\xff\" val",
            Some("0"),
        );
        assert_eq!(info.statements.len(), 6);
        assert!(
            info.statements
                .iter()
                .all(|s| s.database.as_deref() == Some("3") && s.read == Some(false))
        );
        assert_eq!(info.statements[2].kind, "CONFIG SET");
        assert_eq!(info.statements[2].structure, Some(true));
        assert_eq!(info.statements[3].table.as_deref(), Some(r#""\\key""#));
        assert_eq!(info.statements[3].structure, Some(false));
        assert_eq!(info.statements[5].table.as_deref(), Some(r#""\xff""#));
        let expected = [
            (
                WarningCode::Flush,
                "FLUSHDB deletes every key of database 3",
            ),
            (
                WarningCode::Flush,
                "FLUSHALL deletes every key of every database",
            ),
            (
                WarningCode::ServerAdmin,
                "CONFIG SET changes the server, not only data",
            ),
            (
                WarningCode::UnknownCommand,
                "EVAL runs a script; sandb cannot tell which keys it changes",
            ),
            (
                WarningCode::UnknownCommand,
                "sandb does not know what CONFIG FUTURE does",
            ),
        ];
        assert_eq!(info.warnings.len(), expected.len());
        for (index, (actual, (code, message))) in info.warnings.iter().zip(expected).enumerate() {
            assert_eq!(
                (actual.code, actual.message.as_str(), actual.statement),
                (code, message, Some(index + 2))
            );
        }
    }

    #[test]
    fn reports_parse_errors_in_write_classification() {
        let bad_select = classify_write("SELECT nope\nFLUSHDB", None);
        assert_eq!(bad_select.warnings[0].code, WarningCode::Unparsable);
        assert_eq!(bad_select.warnings[0].statement, Some(0));
        assert_eq!(bad_select.statements[0].database.as_deref(), Some("0"));
        let parse_error = classify_write("SET k 'value", None);
        assert!(parse_error.statements.is_empty());
        assert_eq!(parse_error.warnings.len(), 1);
        assert_eq!(parse_error.warnings[0].code, WarningCode::Unparsable);
        assert_eq!(parse_error.warnings[0].statement, None);
    }

    #[test]
    fn preview_requires_one_known_write() {
        assert_eq!(
            write_preview("SET key value", "1")
                .unwrap()
                .statements
                .len(),
            1
        );
        assert_eq!(
            write_preview("FLUSHDB", "4").unwrap().warnings[0].code,
            WarningCode::Flush
        );
        assert!(write_preview("EVAL 'return 1' 0", "1").is_ok());
        for text in ["", "GET key", "SELECT 1", "CONFIG FUTURE", "PING\nSET k v"] {
            assert_eq!(
                write_preview(text, "0").unwrap_err().code,
                ErrorCode::InvalidInput,
                "{text}"
            );
        }
        assert_eq!(
            write_preview("MULTI", "0").unwrap_err().code,
            ErrorCode::Unsupported
        );
    }
}
