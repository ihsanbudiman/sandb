//! Phase 3b, read-only connections: the second barrier for the user's own editor. The database
//! session itself is the real barrier (PG `default_transaction_read_only`, MySQL `SET SESSION
//! TRANSACTION READ ONLY`, SQLite read-only + `query_only`); this check runs first so a refused
//! statement gets a clear error, and so SQL that would undo the session setting is caught before
//! it runs. It fails closed: text it cannot analyse is refused.

use sqlparser::tokenizer::{Token, Tokenizer};

use crate::core::connections::{Connection, Engine};
use crate::core::error::{AppError, ErrorCode};
use crate::core::sql::facts::{self, StatementFacts, StatementKind};
use crate::core::sql::{functions, split};

/// Words of `sql` with comments and whitespace removed, uppercased. String literals become `?`
/// tokens, so `SELECT 'set'` stays a read; quoted identifiers keep their text, so
/// `SET "transaction_read_only" = off` matches like the bare name.
/// `;` is kept as its own token so statement boundaries are visible. MySQL system variables
/// (`@@x`, `@@session.x`, `@@global.x`) lose their `@@` and scope prefix, so they compare
/// equal to the bare `SET x` form.
fn words(engine: Engine, sql: &str) -> Vec<String> {
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

/// Splits the word list at `;` into per-statement slices.
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

/// Position of the first `SET` keyword's arguments, skipping an optional SESSION / GLOBAL
/// scope word.
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

/// One statement's word list escapes the read-only session, per engine.
fn escapes(engine: Engine, s: &[String]) -> bool {
    if s.is_empty() {
        return false;
    }
    match engine {
        Engine::Pg => {
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
        Engine::Mysql => {
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
        Engine::Sqlite => is(s, 0, "PRAGMA") && s.iter().skip(1).any(|w| w == "QUERY_ONLY"),
        // No SQL runs on a Mongo connection; refuse everything if text ever gets here.
        Engine::Mongo => true,
    }
}

/// Called functions on the guard's deny side (`set_config`, `nextval`, `setval`, `pg_*` /
/// `lo_*` / `dblink*` writers, …) that are not pure built-ins. Schema qualifiers are dropped,
/// so `pg_catalog.set_config` matches too. User-defined functions are not listed: the editor
/// may call them, and the database barrier refuses any write they attempt.
fn denied_functions(engine: Engine, s: &StatementFacts) -> Vec<String> {
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
/// classification `classify_write` reports on), statements the parser cannot verify, and the
/// session-mode changes that could undo the database barrier. The message names the connection.
pub fn check_read_only(conn: &Connection, stmts: &[String]) -> Result<(), AppError> {
    debug_assert!(conn.read_only);
    let name = format!("Connection \"{}\" is read-only", conn.name);
    let err = |m: String| AppError::new(ErrorCode::ConnectionReadOnly, format!("{name}: {m}"));
    for sql in stmts {
        let w = words(conn.engine, sql);
        for s in statements(&w) {
            if escapes(conn.engine, s) {
                return Err(err(
                    "this statement would change the session's transaction mode".into(),
                ));
            }
        }
        // Statements consisting only of SET clauses pass the classify layer below: every
        // session-mode change that could weaken the barrier was already caught by `escapes`,
        // and a `SET search_path` / `SET sql_mode` cannot write data.
        let all_set = !w.is_empty()
            && statements(&w)
                .iter()
                .filter(|s| !s.is_empty())
                .all(|s| is(s, 0, "SET"));
        let f = match facts::analyze(conn.engine, sql) {
            Ok(f) => f,
            Err(_) => {
                // Fail closed: the parser cannot prove this is not a write.
                return Err(err("SQL that cannot be analysed is refused".into()));
            }
        };
        if f.has_executable_comment {
            return Err(err(
                "MySQL executable comments (/*! … */ or /*M! … */) are refused".into(),
            ));
        }
        for s in &f.statements {
            // `SELECT set_config('default_transaction_read_only', 'off', false)` flips the
            // session barrier for the next statement; the guard's deny list catches it.
            if s.kind == StatementKind::Query {
                let denied = denied_functions(conn.engine, s);
                if !denied.is_empty() {
                    return Err(err(format!(
                        "functions that change session state or have side effects are \
                         refused: {}",
                        denied.join(", ")
                    )));
                }
            }
            let blocked = match s.kind {
                // Reads are fine, unless a write hides in them.
                StatementKind::Query => {
                    s.has_into || s.has_locking || s.has_modifying_cte || s.has_variable_assignment
                }
                // SHOW, DESCRIBE.
                StatementKind::Show => false,
                // EXPLAIN only for a read; ANALYZE runs the statement.
                StatementKind::Explain { analyze } => {
                    analyze || s.explained.as_deref() != Some(&StatementKind::Query)
                }
                // BEGIN / COMMIT / ROLLBACK / SAVEPOINT stay inside the session's transaction;
                // a READ WRITE form was caught above.
                StatementKind::Transaction => false,
                // SQLite `PRAGMA`: `query_only` was caught above; other pragmas cannot write
                // through the read-only file the database barrier enforces.
                StatementKind::Pragma => false,
                // SET statements were vetted by `escapes`; every other Other (USE, CALL, COPY,
                // LOCK, GRANT, VACUUM, ANALYZE, ATTACH, …) is refused.
                StatementKind::Other => !all_set,
                // INSERT / UPDATE / DELETE / MERGE / CREATE / ALTER / DROP / TRUNCATE.
                _ => true,
            };
            if blocked {
                return Err(err(format!("{} statements are refused", s.label)));
            }
        }
    }
    Ok(())
}

/// The one place that decides whether a connection may write: phases 5 (AI writes) and 6
/// (edit-mode Save) must call this instead of reading `conn.read_only` directly, so the rule
/// stays in sync with the session barrier and the editor pre-check.
pub fn writes_allowed(conn: &Connection) -> bool {
    !conn.read_only
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{Env, SslMode};

    fn conn(engine: Engine) -> Connection {
        Connection {
            id: "c".into(),
            name: "prod".into(),
            engine,
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

    fn ok(engine: Engine, sql: &str) -> Result<(), AppError> {
        let c = conn(engine);
        check_read_only(&c, &split::split(engine, sql))
    }

    fn refused(engine: Engine, sql: &str) -> AppError {
        ok(engine, sql).expect_err(&format!("{engine:?} must refuse `{sql}`"))
    }

    #[test]
    fn writes_and_ddl_are_refused_on_every_engine() {
        for e in [Engine::Pg, Engine::Mysql, Engine::Sqlite] {
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
        refused(Engine::Pg, "COPY t TO stdout");
        refused(Engine::Pg, "SELECT * INTO newt FROM t");
        refused(Engine::Mysql, "CALL p()");
        refused(Engine::Sqlite, "VACUUM");
        refused(Engine::Sqlite, "ATTACH DATABASE 'x.db' AS x");
    }

    #[test]
    fn reads_and_safe_meta_are_allowed() {
        for e in [Engine::Pg, Engine::Mysql, Engine::Sqlite] {
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
        ok(Engine::Mysql, "SHOW TABLES").unwrap();
        ok(Engine::Mysql, "DESCRIBE t").unwrap();
        ok(Engine::Sqlite, "PRAGMA table_info(t)").unwrap();
        ok(Engine::Sqlite, "PRAGMA index_list('t')").unwrap();
        // The user's own COMMIT/ROLLBACK/BEGIN stay: they cannot widen the session.
        ok(Engine::Pg, "BEGIN; SELECT 1; COMMIT").unwrap();
        ok(Engine::Mysql, "START TRANSACTION; SELECT 1; COMMIT").unwrap();
        ok(Engine::Sqlite, "BEGIN; SELECT 1; COMMIT").unwrap();
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
                refused(Engine::Pg, sql).code,
                ErrorCode::ConnectionReadOnly,
                "{sql}"
            );
        }
        ok(Engine::Pg, "SET TRANSACTION READ ONLY").unwrap();
        // Other settings that cannot widen the session still pass.
        ok(Engine::Pg, "SET search_path TO analytics").unwrap();
        ok(Engine::Pg, "SET LOCAL statement_timeout = 1000").unwrap();
        // Pure built-ins (even `pg_*` ones) and user-defined functions still run: the database
        // barrier refuses any write a user function attempts.
        ok(Engine::Pg, "SELECT pg_typeof(1), upper('a')").unwrap();
        ok(Engine::Pg, "SELECT my_report(1)").unwrap();
        ok(Engine::Pg, "SELECT 'set_config' AS note").unwrap();
        let e = refused(
            Engine::Pg,
            "SELECT set_config('default_transaction_read_only', 'off', false)",
        );
        assert!(e.message.contains("set_config"), "{e}");
        let e = refused(Engine::Pg, "SET LOCAL transaction_read_only = off");
        assert!(e.message.contains("transaction mode"), "{e}");
        let e = refused(Engine::Pg, "SET \"transaction_read_only\" = off");
        assert!(e.message.contains("transaction mode"), "{e}");
        refused(Engine::Pg, "SELECT 1; RESET ALL; SELECT 2");
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
                refused(Engine::Mysql, sql).code,
                ErrorCode::ConnectionReadOnly,
                "{sql}"
            );
        }
        ok(Engine::Mysql, "SET SESSION sql_mode = 'STRICT_ALL_TABLES'").unwrap();
        ok(
            Engine::Mysql,
            "SET @@session.sql_mode = 'STRICT_ALL_TABLES'",
        )
        .unwrap();
        assert_eq!(system_variable("@@SESSION.TX_READ_ONLY"), "TX_READ_ONLY");
        assert_eq!(system_variable("@@TX_READ_ONLY"), "TX_READ_ONLY");
        assert_eq!(system_variable("@V"), "@V");
        let e = refused(Engine::Mysql, "SET @@session.transaction_read_only = 0");
        assert!(e.message.contains("transaction mode"), "{e}");
        refused(Engine::Mysql, "SELECT 1; SET transaction_read_only = 0");
    }

    #[test]
    fn sqlite_session_mode_escapes_are_refused() {
        refused(Engine::Sqlite, "PRAGMA query_only = 0");
        refused(Engine::Sqlite, "PRAGMA query_only = OFF");
        refused(Engine::Sqlite, "PRAGMA query_only(0)");
        refused(Engine::Sqlite, "PRAGMA main.query_only = 1");
        // Unrelated pragmas that touch session state stay: the file itself is still read-only.
        ok(Engine::Sqlite, "PRAGMA cache_size = 1000").unwrap();
    }

    #[test]
    fn unparsable_and_tricky_text_is_refused_closed() {
        refused(Engine::Pg, "SELEC 1");
        refused(Engine::Pg, "DO $$ BEGIN RAISE NOTICE 'x'; END $$");
        refused(Engine::Mysql, "SELECT 1 /*!; DELETE FROM t */");
        refused(Engine::Mysql, "SELECT @v := 1");
    }

    #[test]
    fn helper_functions() {
        let w = words(Engine::Pg, "SET a = 1 -- x\n; SELECT 'reset all'");
        assert_eq!(w, ["SET", "A", "?", "?", ";", "SELECT", "?"]);
        let stmts = statements(&w);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], ["SET", "A", "?", "?"]);
        assert_eq!(stmts[1], ["SELECT", "?"]);
    }
}
