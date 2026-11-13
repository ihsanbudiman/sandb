//! Phase 3b, read-only connections: the second barrier for the user's own editor. The database
//! session itself is the real barrier (PG `default_transaction_read_only`, MySQL `SET SESSION
//! TRANSACTION READ ONLY`, SQLite read-only + `query_only`); this check runs first so a refused
//! statement gets a clear error, and so SQL that would undo the session setting is caught before
//! it runs. It fails closed: text it cannot analyse is refused.

use sqlparser::tokenizer::{Token, Tokenizer};

use crate::core::connections::{Connection, read_only_refusal};
use crate::core::engine::SqlEngine;
use crate::core::error::AppError;
use crate::core::sql::facts::{
    self, ReadProfile, ReadVerdict, StatementFacts, StatementKind,
};
use crate::core::sql::{functions, split};

/// Words of `sql` with comments and whitespace removed, uppercased. String literals become `?`
/// tokens, so `SELECT 'set'` stays a read; quoted identifiers keep their text, so
/// `SET "transaction_read_only" = off` matches like the bare name.
/// `;` is kept as its own token so statement boundaries are visible. MySQL system variables
/// (`@@x`, `@@session.x`, `@@global.x`) lose their `@@` and scope prefix, so they compare
/// equal to the bare `SET x` form.
fn words(engine: SqlEngine, sql: &str) -> Vec<String> {
    let d = split::dialect(engine);
    let Ok(tokens) = Tokenizer::new(&*d, sql).tokenize() else {
        return Vec::new();
    };
    tokens
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(system_variable(&w.value.to_ascii_uppercase()).to_string()),
            Token::SemiColon => Some(";".into()),
            Token::Whitespace(_) | Token::EOF => None,
            _ => Some("?".into()),
        })
        .collect()
}

/// `@@X`, `@@SESSION.X`, `@@GLOBAL.X`, … → `X`; any other word unchanged. Input is uppercased.
fn system_variable(w: &str) -> &str {
    let Some(rest) = w.strip_prefix("@@") else {
        return w;
    };
    ["SESSION.", "GLOBAL.", "LOCAL.", "PERSIST.", "PERSIST_ONLY."]
        .iter()
        .find_map(|p| rest.strip_prefix(p))
        .unwrap_or(rest)
}

/// Settings whose change can make this session or its current transaction writable.
const READ_ONLY_VARS: &[&str] = &[
    "DEFAULT_TRANSACTION_READ_ONLY",
    "TRANSACTION_READ_ONLY",
    "TX_READ_ONLY",
];

fn statements(w: &[String]) -> Vec<&[String]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, t) in w.iter().enumerate() {
        if t == ";" {
            out.push(&w[start..i]);
            start = i + 1;
        }
    }
    out.push(&w[start..]);
    out
}

fn starts(s: &[String], prefix: &[&str]) -> bool {
    s.len() >= prefix.len() && s.iter().zip(prefix).all(|(a, b)| a == b)
}

fn is(s: &[String], i: usize, word: &str) -> bool {
    s.get(i).is_some_and(|a| a == word)
}

/// `READ WRITE` anywhere in the statement (after `BEGIN` / `START TRANSACTION`, where the mode
/// clause can sit among `WITH CONSISTENT SNAPSHOT` etc.).
fn read_write_in(s: &[String]) -> bool {
    s.windows(2).any(|p| p[0] == "READ" && p[1] == "WRITE")
}

/// `SET` / `SET SESSION` (transaction- and session-scoped); `SET LOCAL` and `SET GLOBAL` cannot
/// weaken the barrier of this session.
fn session_set(s: &[String]) -> bool {
    is(s, 0, "SET") && !is(s, 1, "GLOBAL") && !is(s, 1, "LOCAL")
}

fn set_arg(s: &[String]) -> usize {
    if is(s, 1, "SESSION") || is(s, 1, "GLOBAL") {
        2
    } else {
        1
    }
}

/// `READ ONLY` is allowed; every other `SET TRANSACTION` clause is refused. Isolation levels
/// are fine only together with READ ONLY: write-capable isolation modes are refused outright,
/// so the list stays permissive and simple.
fn set_transaction_read_only(s: &[String]) -> bool {
    let i = set_arg(s);
    is(s, i + 1, "READ") && is(s, i + 2, "ONLY") && i + 3 == s.len()
}

fn escapes(engine: SqlEngine, s: &[String]) -> bool {
    if s.is_empty() {
        return false;
    }
    match engine {
        SqlEngine::Pg => {
            starts(s, &["SET", "SESSION", "CHARACTERISTICS"])
                // Any scope, LOCAL included: `SET LOCAL transaction_read_only = off` as the
                // first statement of a transaction makes that transaction writable.
                || (is(s, 0, "SET")
                    && s.iter()
                        .skip(1)
                        .take(2)
                        .any(|w| READ_ONLY_VARS.contains(&w.as_str())))
                || (session_set(s)
                    && is(s, set_arg(s), "TRANSACTION")
                    && !set_transaction_read_only(s))
                || (starts(s, &["BEGIN"]) && read_write_in(s))
                || (starts(s, &["START", "TRANSACTION"]) && read_write_in(s))
                || starts(s, &["RESET", "ALL"])
                || (is(s, 0, "RESET") && s.get(1).is_some_and(|w| READ_ONLY_VARS.contains(&w.as_str())))
                || starts(s, &["DISCARD", "ALL"])
        }
        SqlEngine::Mysql => {
            // Any SET that touches transaction mode: the session-scope forms undo the
            // after_connect barrier, and GLOBAL/transaction_read_only re-enables writes for
            // new sessions, so GLOBAL is refused here too.
            is(s, 0, "SET")
                && (is(s, set_arg(s), "TRANSACTION")
                    || s.iter()
                        .skip(set_arg(s))
                        .any(|w| READ_ONLY_VARS.contains(&w.as_str())))
                || (starts(s, &["START", "TRANSACTION"]) && read_write_in(s))
                || (starts(s, &["BEGIN"]) && read_write_in(s))
        }
        SqlEngine::Sqlite => is(s, 0, "PRAGMA") && s.iter().skip(1).any(|w| w == "QUERY_ONLY"),
    }
}

/// Called functions on the guard's deny side (`set_config`, `nextval`, `setval`, `pg_*` /
/// `lo_*` / `dblink*` writers, …) that are not pure built-ins. Schema qualifiers are dropped,
/// so `pg_catalog.set_config` matches too. User-defined functions are not listed: the editor
/// may call them, and the database barrier refuses any write they attempt.
fn denied_functions(engine: SqlEngine, s: &StatementFacts) -> Vec<String> {
    s.functions
        .iter()
        .filter(|f| {
            let base = f.rsplit('.').next().unwrap_or(f).to_lowercase();
            facts::is_dangerous_field_name(&base) && !functions::is_pure(engine, &base)
        })
        .cloned()
        .collect()
}

/// Refuses editor SQL that must never run on a read-only connection: writes and DDL (the same
/// shared read policy the AI uses), statements the parser cannot verify, and the session-mode
/// changes that could undo the database barrier. The message names the connection.
pub fn check_read_only(
    conn: &Connection,
    engine: SqlEngine,
    stmts: &[String],
) -> Result<(), AppError> {
    debug_assert!(!conn.may_write());
    let err = |m: String| read_only_refusal(conn, Some(&m));
    for sql in stmts {
        let w = words(engine, sql);
        for s in statements(&w) {
            if escapes(engine, s) {
                return Err(err(
                    "this statement would change the session's transaction mode".into(),
                ));
            }
        }
        // Statements consisting only of SET clauses pass the shared policy below: every
        // session-mode change that could weaken the barrier was already caught by `escapes`,
        // and a `SET search_path` / `SET sql_mode` cannot write data.
        let all_set = !w.is_empty()
            && statements(&w)
                .iter()
                .filter(|s| !s.is_empty())
                .all(|s| is(s, 0, "SET"));
        let f = match facts::analyze(engine, sql) {
            Ok(f) => f,
            Err(_) => {
                // Fail closed: the parser cannot prove this is not a write.
                return Err(err("SQL that cannot be analysed is refused".into()));
            }
        };
        let verdicts = facts::read_policy(&f, ReadProfile::ReadOnlyEditor)
            .map_err(err)?;
        for (s, verdict) in f.statements.iter().zip(verdicts) {
            // `SELECT set_config('default_transaction_read_only', 'off', false)` flips the
            // session barrier for the next statement; the guard's deny list catches it.
            if s.kind == StatementKind::Query {
                let denied = denied_functions(engine, s);
                if !denied.is_empty() {
                    return Err(err(format!(
                        "functions that change session state or have side effects are \
                         refused: {}",
                        denied.join(", ")
                    )));
                }
            }
            match verdict {
                ReadVerdict::Read { .. } => {}
                ReadVerdict::Refused(reason) => return Err(err(reason)),
                // Transaction control, PRAGMA and SET stay inside the read-only session; every
                // other Other (USE, CALL, COPY, LOCK, GRANT, VACUUM, ANALYZE, ATTACH, …) is
                // refused.
                ReadVerdict::Mode => {
                    if s.kind == StatementKind::Other && !all_set {
                        return Err(err(format!("{} statements are refused", s.label)));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{Env, SslMode};
    use crate::core::error::ErrorCode;

    fn conn(engine: SqlEngine) -> Connection {
        Connection {
            id: "c".into(),
            name: "prod".into(),
            engine: engine.engine(),
            version: String::new(),
            env: Env::Production,
            host: "h".into(),
            port: 1,
            database: "d".into(),
            user: "u".into(),
            ssl: SslMode::Prefer,
            file: String::new(),
            ai_write: false,
            read_only: true,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    fn ok(engine: SqlEngine, sql: &str) -> Result<(), AppError> {
        let c = conn(engine);
        check_read_only(&c, engine, &split::split(engine, sql))
    }

    fn refused(engine: SqlEngine, sql: &str) -> AppError {
        ok(engine, sql).expect_err(&format!("{engine:?} must refuse `{sql}`"))
    }

    #[test]
    fn writes_and_ddl_are_refused_on_every_engine() {
        for e in [SqlEngine::Pg, SqlEngine::Mysql, SqlEngine::Sqlite] {
            for sql in [
                "INSERT INTO t VALUES (1)",
                "UPDATE t SET a = 1",
                "DELETE FROM t",
                "CREATE TABLE x (a int)",
                "ALTER TABLE t ADD b int",
                "DROP TABLE t",
                "TRUNCATE t",
                "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
                "SELECT * FROM t FOR UPDATE",
            ] {
                let err = refused(e, sql);
                assert_eq!(err.code, ErrorCode::ConnectionReadOnly, "{e:?} `{sql}`");
                assert!(err.message.contains("prod"), "{err}");
            }
        }
        refused(SqlEngine::Pg, "COPY t TO stdout");
        refused(SqlEngine::Pg, "SELECT * INTO newt FROM t");
        refused(SqlEngine::Mysql, "CALL p()");
        refused(SqlEngine::Sqlite, "VACUUM");
        refused(SqlEngine::Sqlite, "ATTACH DATABASE 'x.db' AS x");
    }

    #[test]
    fn reads_and_safe_meta_are_allowed() {
        for e in [SqlEngine::Pg, SqlEngine::Mysql, SqlEngine::Sqlite] {
            for sql in [
                "SELECT * FROM t",
                "WITH x AS (SELECT 1) SELECT * FROM x",
                "VALUES (1, 2)",
                "SELECT 'insert into t' AS note",
                "EXPLAIN SELECT * FROM t",
            ] {
                ok(e, sql).unwrap_or_else(|err| panic!("{e:?} `{sql}`: {err}"));
            }
        }
        ok(SqlEngine::Mysql, "SHOW TABLES").unwrap();
        ok(SqlEngine::Mysql, "DESCRIBE t").unwrap();
        ok(SqlEngine::Sqlite, "PRAGMA table_info(t)").unwrap();
        ok(SqlEngine::Sqlite, "PRAGMA index_list('t')").unwrap();
        // The user's own COMMIT/ROLLBACK/BEGIN stay: they cannot widen the session.
        ok(SqlEngine::Pg, "BEGIN; SELECT 1; COMMIT").unwrap();
        ok(SqlEngine::Mysql, "START TRANSACTION; SELECT 1; COMMIT").unwrap();
        ok(SqlEngine::Sqlite, "BEGIN; SELECT 1; COMMIT").unwrap();
    }

    #[test]
    fn pg_session_mode_escapes_are_refused() {
        for sql in [
            "SET default_transaction_read_only = off",
            "SET SESSION default_transaction_read_only TO off",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            "SET TRANSACTION READ WRITE",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "BEGIN READ WRITE",
            "START TRANSACTION READ WRITE",
            "RESET ALL",
            "RESET default_transaction_read_only",
            "DISCARD ALL",
            // Review item 4: the current transaction's mode, every scope and quoting.
            "SET transaction_read_only = off",
            "SET SESSION transaction_read_only TO off",
            "SET LOCAL transaction_read_only = off",
            "SET \"transaction_read_only\" = off",
            "SET \"default_transaction_read_only\" = off",
            "SET tx_read_only = off",
            "RESET transaction_read_only",
            "BEGIN; SET LOCAL transaction_read_only = off; INSERT INTO t VALUES (1); COMMIT",
            // Function calls that flip the barrier for the next statement.
            "SELECT set_config('default_transaction_read_only', 'off', false)",
            "SELECT set_config('transaction_read_only', 'off', true)",
            "SELECT pg_catalog.set_config('default_transaction_read_only', 'off', false)",
            "SELECT x FROM (SELECT set_config('default_transaction_read_only', 'off', false) AS x) s",
            "SELECT nextval('s')",
            "SELECT setval('s', 1)",
            "SELECT lo_import('/etc/passwd')",
            "SELECT pg_advisory_lock(1)",
            "VALUES (set_config('default_transaction_read_only', 'off', false))",
        ] {
            assert_eq!(
                refused(SqlEngine::Pg, sql).code,
                ErrorCode::ConnectionReadOnly,
                "{sql}"
            );
        }
        ok(SqlEngine::Pg, "SET TRANSACTION READ ONLY").unwrap();
        // Other settings that cannot widen the session still pass.
        ok(SqlEngine::Pg, "SET search_path TO analytics").unwrap();
        ok(SqlEngine::Pg, "SET LOCAL statement_timeout = 1000").unwrap();
        // Pure built-ins (even `pg_*` ones) and user-defined functions still run: the database
        // barrier refuses any write a user function attempts.
        ok(SqlEngine::Pg, "SELECT pg_typeof(1), upper('a')").unwrap();
        ok(SqlEngine::Pg, "SELECT my_report(1)").unwrap();
        ok(SqlEngine::Pg, "SELECT 'set_config' AS note").unwrap();
        let e = refused(
            SqlEngine::Pg,
            "SELECT set_config('default_transaction_read_only', 'off', false)",
        );
        assert!(e.message.contains("set_config"), "{e}");
        let e = refused(SqlEngine::Pg, "SET LOCAL transaction_read_only = off");
        assert!(e.message.contains("transaction mode"), "{e}");
        let e = refused(SqlEngine::Pg, "SET \"transaction_read_only\" = off");
        assert!(e.message.contains("transaction mode"), "{e}");
        refused(SqlEngine::Pg, "SELECT 1; RESET ALL; SELECT 2");
    }

    #[test]
    fn mysql_session_mode_escapes_are_refused() {
        for sql in [
            "SET TRANSACTION READ WRITE",
            "SET TRANSACTION READ ONLY",
            "SET SESSION TRANSACTION READ WRITE",
            "SET SESSION TRANSACTION READ ONLY",
            "SET transaction_read_only = 0",
            "SET SESSION tx_read_only = OFF",
            "SET GLOBAL transaction_read_only = 0",
            "START TRANSACTION READ WRITE",
            "BEGIN READ WRITE",
            // Review item 4: `@@` system-variable forms.
            "SET @@transaction_read_only = 0",
            "SET @@tx_read_only = 0",
            "SET @@session.transaction_read_only = 0",
            "SET @@SESSION.tx_read_only = OFF",
            "SET @@global.transaction_read_only = 0",
            "SET @@local.transaction_read_only = 0",
            "SET @a = 1, @@session.transaction_read_only = 0",
            "SET `transaction_read_only` = 0",
        ] {
            assert_eq!(
                refused(SqlEngine::Mysql, sql).code,
                ErrorCode::ConnectionReadOnly,
                "{sql}"
            );
        }
        ok(SqlEngine::Mysql, "SET SESSION sql_mode = 'STRICT_ALL_TABLES'").unwrap();
        ok(
            SqlEngine::Mysql,
            "SET @@session.sql_mode = 'STRICT_ALL_TABLES'",
        )
        .unwrap();
        assert_eq!(system_variable("@@SESSION.TX_READ_ONLY"), "TX_READ_ONLY");
        assert_eq!(system_variable("@@TX_READ_ONLY"), "TX_READ_ONLY");
        assert_eq!(system_variable("@V"), "@V");
        let e = refused(SqlEngine::Mysql, "SET @@session.transaction_read_only = 0");
        assert!(e.message.contains("transaction mode"), "{e}");
        refused(SqlEngine::Mysql, "SELECT 1; SET transaction_read_only = 0");
    }

    #[test]
    fn sqlite_session_mode_escapes_are_refused() {
        refused(SqlEngine::Sqlite, "PRAGMA query_only = 0");
        refused(SqlEngine::Sqlite, "PRAGMA query_only = OFF");
        refused(SqlEngine::Sqlite, "PRAGMA query_only(0)");
        refused(SqlEngine::Sqlite, "PRAGMA main.query_only = 1");
        // Unrelated pragmas that touch session state stay: the file itself is still read-only.
        ok(SqlEngine::Sqlite, "PRAGMA cache_size = 1000").unwrap();
    }

    #[test]
    fn unparsable_and_tricky_text_is_refused_closed() {
        refused(SqlEngine::Pg, "SELEC 1");
        refused(SqlEngine::Pg, "DO $$ BEGIN RAISE NOTICE 'x'; END $$");
        refused(SqlEngine::Mysql, "SELECT 1 /*!; DELETE FROM t */");
        refused(SqlEngine::Mysql, "SELECT @v := 1");
    }

    #[test]
    fn helper_functions() {
        let w = words(SqlEngine::Pg, "SET a = 1 -- x\n; SELECT 'reset all'");
        assert_eq!(w, ["SET", "A", "?", "?", ";", "SELECT", "?"]);
        let stmts = statements(&w);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], ["SET", "A", "?", "?"]);
        assert_eq!(stmts[1], ["SELECT", "?"]);
    }
}
