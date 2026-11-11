//! Table-driven tests for the AI read guard and the write classifier. Every bypass case says
//! why the SQL is dangerous: a regression here lets AI-written SQL change or leak data.

use super::*;
use crate::core::connections::Engine::{self, Mysql, Pg, Sqlite};

const ALL: [Engine; 3] = [Pg, Mysql, Sqlite];

/// (engine, sql, fragment the reason must contain (lowercase), why it is dangerous).
type Bypass = (Engine, &'static str, &'static str, &'static str);

const BYPASSES: &[Bypass] = &[
    // --- stacked statements: the second statement would run with the first one's approval.
    (
        Pg,
        "SELECT 1; DELETE FROM t",
        "exactly one",
        "a write rides along after a read",
    ),
    (
        Mysql,
        "SELECT 1; DELETE FROM t",
        "exactly one",
        "a write rides along after a read",
    ),
    (
        Sqlite,
        "SELECT 1; DELETE FROM t",
        "exactly one",
        "a write rides along after a read",
    ),
    (
        Pg,
        "SELECT 1;\n-- harmless\n/* c */ DELETE FROM t",
        "exactly one",
        "comments and newlines hide the second statement",
    ),
    (
        Mysql,
        "SELECT 1; # note\nDROP TABLE t",
        "exactly one",
        "MySQL # comment hides the separator",
    ),
    (
        Pg,
        "SELECT 1;;DELETE FROM t",
        "exactly one",
        "empty statements between parts",
    ),
    (
        Pg,
        "SELECT 1; SELECT 2",
        "exactly one",
        "two reads are still two statements; the server would run both",
    ),
    (
        Mysql,
        "SELECT 1 --1; DELETE FROM t",
        "exactly one",
        "MySQL needs a space after --, so `--1` is not a comment and the DELETE is real",
    ),
    (
        Sqlite,
        "PRAGMA table_info('t'); DELETE FROM t",
        "exactly one",
        "a write after an allowed PRAGMA",
    ),
    (
        Sqlite,
        "PRAGMA table_info(t); DELETE FROM t",
        "parse",
        "a write after a PRAGMA the parser cannot read",
    ),
    // --- data-modifying CTEs and subqueries: PG runs them even inside a SELECT.
    (
        Pg,
        "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
        "changes data",
        "the CTE deletes rows",
    ),
    (
        Pg,
        "WITH u AS (UPDATE t SET a = 1 RETURNING *) SELECT count(*) FROM u",
        "changes data",
        "the CTE updates rows",
    ),
    (
        Pg,
        "WITH i AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM i",
        "changes data",
        "the CTE inserts rows",
    ),
    (
        Pg,
        "WITH a AS (WITH b AS (DELETE FROM t RETURNING *) SELECT * FROM b) SELECT * FROM a",
        "changes data",
        "nested CTE deletes rows",
    ),
    (
        Pg,
        "SELECT * FROM (DELETE FROM t RETURNING *) d",
        "changes data",
        "a derived table that deletes",
    ),
    (
        Pg,
        "SELECT 1 UNION (DELETE FROM t RETURNING 1)",
        "changes data",
        "a UNION branch that deletes",
    ),
    (
        Pg,
        "WITH x AS (SELECT 1) DELETE FROM t",
        "delete is not a read",
        "WITH that ends in a write",
    ),
    // --- EXPLAIN ANALYZE executes the statement.
    (
        Pg,
        "EXPLAIN ANALYZE DELETE FROM t",
        "explain analyze",
        "runs the DELETE",
    ),
    (
        Pg,
        "EXPLAIN (ANALYZE, BUFFERS) UPDATE t SET a = 1",
        "explain analyze",
        "runs the UPDATE",
    ),
    (
        Pg,
        "EXPLAIN (ANALYZE) SELECT pg_sleep(100)",
        "explain analyze",
        "runs the query",
    ),
    (
        Pg,
        "EXPLAIN (ANALYSE) DELETE FROM t",
        "explain analyze",
        "British spelling of ANALYZE",
    ),
    (
        Pg,
        "EXPLAIN (ANALYZE false) DELETE FROM t",
        "explain analyze",
        "any ANALYZE option is refused, even false",
    ),
    (
        Pg,
        "EXPLAIN ANALYSE DELETE FROM t",
        "parse",
        "the parser does not know this form, so it fails closed",
    ),
    (
        Mysql,
        "EXPLAIN ANALYZE SELECT * FROM t",
        "explain analyze",
        "MySQL runs the query",
    ),
    (
        Mysql,
        "EXPLAIN ANALYZE DELETE FROM t",
        "explain analyze",
        "MySQL runs the DELETE",
    ),
    (
        Pg,
        "EXPLAIN DELETE FROM t",
        "only allowed for a select",
        "plan of a write: no reason for the AI to touch writes",
    ),
    (
        Mysql,
        "DESCRIBE DELETE FROM t",
        "only allowed for a select",
        "DESCRIBE is EXPLAIN on MySQL",
    ),
    (
        Pg,
        "EXPLAIN SELECT nextval('s')",
        "nextval",
        "functions inside EXPLAIN are still checked",
    ),
    // --- INTO: creates tables, writes files or sets variables.
    (
        Pg,
        "SELECT * INTO new_table FROM t",
        "into",
        "creates a table",
    ),
    (Pg, "SELECT 1 INTO TEMP x", "into", "creates a temp table"),
    (
        Mysql,
        "SELECT a INTO @v FROM t",
        "into",
        "sets a session variable",
    ),
    (Mysql, "SELECT 1 INTO @a", "into", "sets a session variable"),
    (
        Mysql,
        "SELECT a FROM t INTO OUTFILE '/tmp/x'",
        "parse",
        "writes a file on the server; unparsable, so refused",
    ),
    (
        Mysql,
        "SELECT a INTO OUTFILE '/tmp/x' FROM t",
        "parse",
        "writes a file on the server",
    ),
    (
        Mysql,
        "SELECT a INTO DUMPFILE '/tmp/x' FROM t",
        "parse",
        "writes a file on the server",
    ),
    (
        Mysql,
        "SELECT a FROM t INTO @v",
        "parse",
        "sets a variable after FROM",
    ),
    // --- executable comments: MySQL runs their body.
    (
        Mysql,
        "SELECT 1 /*!; DELETE FROM t */",
        "executable comment",
        "MySQL runs the DELETE inside the comment",
    ),
    (
        Mysql,
        "SELECT * FROM t /*!50000 FOR UPDATE */",
        "executable comment",
        "version comment adds a lock",
    ),
    (
        Mysql,
        "SELECT 1 /*M! , GET_LOCK('k', 0) */",
        "executable comment",
        "MariaDB comment calls GET_LOCK",
    ),
    (
        Mysql,
        "SELECT 1 /*!*/",
        "executable comment",
        "even an empty one: the parser cannot see what it hides",
    ),
    // --- optimizer hints change limits per statement.
    (
        Mysql,
        "SELECT /*+ MAX_EXECUTION_TIME(0) */ * FROM t",
        "hint",
        "removes the statement time limit",
    ),
    (
        Mysql,
        "SELECT /*+ SET_VAR(sql_mode='') */ 1",
        "hint",
        "changes session variables for the statement",
    ),
    (
        Pg,
        "/*+ Set(statement_timeout 0) */ SELECT 1",
        "hint",
        "pg_hint_plan can lift statement_timeout",
    ),
    // --- locking reads: block other sessions' writes.
    (Pg, "SELECT * FROM t FOR UPDATE", "locking", "row locks"),
    (Pg, "SELECT * FROM t FOR SHARE", "locking", "row locks"),
    (
        Pg,
        "SELECT * FROM t FOR UPDATE OF t SKIP LOCKED",
        "locking",
        "row locks",
    ),
    (
        Pg,
        "SELECT * FROM t FOR NO KEY UPDATE",
        "parse",
        "row locks; unparsable, so refused",
    ),
    (
        Pg,
        "SELECT * FROM t FOR KEY SHARE",
        "parse",
        "row locks; unparsable, so refused",
    ),
    (
        Pg,
        "SELECT * FROM (SELECT * FROM t FOR UPDATE) s",
        "locking",
        "lock in a derived table",
    ),
    (
        Pg,
        "SELECT * FROM t WHERE id IN (SELECT id FROM u FOR UPDATE)",
        "locking",
        "lock in a subquery",
    ),
    (Mysql, "SELECT * FROM t FOR SHARE", "locking", "row locks"),
    (
        Mysql,
        "SELECT * FROM t LOCK IN SHARE MODE",
        "parse",
        "row locks; unparsable, so refused",
    ),
    // --- user variables.
    (Mysql, "SELECT @a := 1", "variable", "changes session state"),
    (
        Mysql,
        "SELECT * FROM t WHERE (@x := id) > 0",
        "variable",
        "assignment hidden in WHERE",
    ),
    // --- statements that are not reads.
    (
        Pg,
        "COPY t TO PROGRAM 'x'",
        "copy is not a read",
        "runs a shell command on the server",
    ),
    (
        Pg,
        "COPY t FROM '/etc/passwd'",
        "copy is not a read",
        "reads a server file into a table",
    ),
    (
        Pg,
        "CALL p()",
        "call is not a read",
        "a procedure can do anything",
    ),
    (
        Mysql,
        "CALL p()",
        "call is not a read",
        "a procedure can do anything",
    ),
    (
        Pg,
        "DO $$ BEGIN DELETE FROM t; END $$",
        "parse",
        "anonymous code block; unparsable, so refused",
    ),
    (
        Pg,
        "SET ROLE admin",
        "set is not a read",
        "privilege change",
    ),
    (
        Pg,
        "SET search_path TO evil",
        "set is not a read",
        "changes name resolution",
    ),
    (
        Pg,
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        "set is not a read",
        "lifts read-only mode",
    ),
    (Mysql, "SET @a = 1", "set is not a read", "session state"),
    (
        Mysql,
        "SET SESSION max_execution_time = 0",
        "set is not a read",
        "lifts the time limit",
    ),
    (
        Pg,
        "LOCK TABLE t",
        "lock is not a read",
        "blocks every other session",
    ),
    (
        Mysql,
        "LOCK TABLES t WRITE",
        "lock",
        "blocks every other session",
    ),
    (
        Pg,
        "PREPARE p AS DELETE FROM t",
        "prepare is not a read",
        "stores a write for later",
    ),
    (
        Pg,
        "EXECUTE p",
        "execute is not a read",
        "runs a stored statement",
    ),
    (
        Pg,
        "VACUUM",
        "vacuum is not a read",
        "maintenance, heavy I/O",
    ),
    (
        Pg,
        "ANALYZE t",
        "analyze is not a read",
        "writes statistics",
    ),
    (
        Pg,
        "REINDEX TABLE t",
        "parse",
        "rebuilds an index; unparsable, so refused",
    ),
    (
        Pg,
        "REFRESH MATERIALIZED VIEW v",
        "parse",
        "rewrites a view; unparsable, so refused",
    ),
    (Pg, "LISTEN c", "listen is not a read", "session state"),
    (
        Pg,
        "NOTIFY c",
        "notify is not a read",
        "sends a message to other sessions",
    ),
    (
        Sqlite,
        "ATTACH DATABASE '/tmp/x' AS y",
        "attach is not a read",
        "opens or creates another file",
    ),
    (
        Sqlite,
        "DETACH DATABASE y",
        "parse",
        "changes attached files; unparsable, so refused",
    ),
    (
        Sqlite,
        "VACUUM INTO '/tmp/x'",
        "parse",
        "writes a copy of the database to a file",
    ),
    (
        Mysql,
        "LOAD DATA INFILE '/x' INTO TABLE t",
        "parse",
        "reads a server file into a table",
    ),
    (Mysql, "HANDLER t OPEN", "parse", "low-level table access"),
    (
        Mysql,
        "USE analytics",
        "use is not a read",
        "changes the session's database",
    ),
    (
        Pg,
        "BEGIN",
        "begin is not a read",
        "transaction control escapes the read-only transaction",
    ),
    (
        Pg,
        "COMMIT",
        "commit is not a read",
        "ends the read-only transaction",
    ),
    (
        Pg,
        "INSERT INTO t VALUES (1)",
        "insert is not a read",
        "DML",
    ),
    (Pg, "UPDATE t SET a = 1", "update is not a read", "DML"),
    (Pg, "DELETE FROM t", "delete is not a read", "DML"),
    (
        Pg,
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN DELETE",
        "merge is not a read",
        "DML",
    ),
    (
        Sqlite,
        "REPLACE INTO t VALUES (1)",
        "insert is not a read",
        "DML",
    ),
    (
        Pg,
        "TRUNCATE t",
        "truncate is not a read",
        "DDL-like delete",
    ),
    (
        Pg,
        "CREATE TABLE x AS SELECT 1",
        "create table is not a read",
        "DDL",
    ),
    (Pg, "DROP TABLE t", "drop table is not a read", "DDL"),
    (
        Mysql,
        "ALTER TABLE t DROP COLUMN a",
        "alter table is not a read",
        "DDL",
    ),
    (Pg, "GRANT SELECT ON t TO x", "grant is not a read", "DCL"),
    // --- SQLite PRAGMA writes and non-allow-listed pragmas.
    (
        Sqlite,
        "PRAGMA writable_schema=1",
        "writable_schema is not allowed",
        "lets the schema be edited directly",
    ),
    (
        Sqlite,
        "PRAGMA journal_mode=delete",
        "journal_mode is not allowed",
        "changes the database file mode",
    ),
    (
        Sqlite,
        "PRAGMA query_only = 0",
        "query_only is not allowed",
        "turns off layer 2 on the connection",
    ),
    (
        Sqlite,
        "PRAGMA query_only(0)",
        "query_only is not allowed",
        "turns off layer 2, call form",
    ),
    (
        Sqlite,
        "PRAGMA table_info = t",
        "read form",
        "assignment form of an allowed name",
    ),
    (
        Sqlite,
        "PRAGMA table_info",
        "read form",
        "no table argument",
    ),
    (
        Sqlite,
        "PRAGMA user_version",
        "user_version is not allowed",
        "not on the allow-list",
    ),
    (
        Pg,
        "PRAGMA table_info('t')",
        "pragma is not a read",
        "PRAGMA is SQLite only",
    ),
    // --- functions with side effects (every name on the deny list).
    (
        Pg,
        "SELECT pg_terminate_backend(123)",
        "pg_terminate_backend",
        "kills another session",
    ),
    (
        Pg,
        "SELECT pg_cancel_backend(123)",
        "pg_cancel_backend",
        "cancels another session's query",
    ),
    (
        Pg,
        "SELECT pg_reload_conf()",
        "pg_reload_conf",
        "reloads server config",
    ),
    (Pg, "SELECT pg_sleep(100)", "pg_sleep", "holds a connection"),
    (
        Pg,
        "SELECT set_config('statement_timeout', '0', false)",
        "set_config",
        "lifts the time limit",
    ),
    (
        Pg,
        "SELECT pg_advisory_lock(1)",
        "pg_advisory_lock",
        "takes a lock",
    ),
    (
        Pg,
        "SELECT pg_try_advisory_lock(1)",
        "pg_try_advisory_lock",
        "takes a lock",
    ),
    (
        Pg,
        "SELECT nextval('s')",
        "nextval",
        "advances a sequence (not rolled back)",
    ),
    (Pg, "SELECT setval('s', 1)", "setval", "resets a sequence"),
    (
        Pg,
        "SELECT lo_import('/etc/passwd')",
        "lo_import",
        "reads a server file",
    ),
    (
        Pg,
        "SELECT lo_export(1, '/tmp/x')",
        "lo_export",
        "writes a server file",
    ),
    (
        Pg,
        "SELECT pg_read_file('/etc/passwd')",
        "pg_read_file",
        "reads a server file",
    ),
    (
        Pg,
        "SELECT pg_read_binary_file('/etc/passwd')",
        "pg_read_binary_file",
        "reads a server file",
    ),
    (
        Pg,
        "SELECT * FROM pg_ls_dir('.')",
        "pg_ls_dir",
        "lists server files",
    ),
    (
        Pg,
        "SELECT * FROM dblink('host=x', 'DELETE FROM t') AS r(a int)",
        "dblink",
        "runs SQL on another connection, outside read-only",
    ),
    (
        Pg,
        "SELECT pg_notify('c', 'x')",
        "pg_notify",
        "sends a message",
    ),
    (
        Pg,
        "SELECT txid_current()",
        "txid_current",
        "assigns a transaction id (a write)",
    ),
    (Mysql, "SELECT SLEEP(100)", "sleep", "holds a connection"),
    (
        Mysql,
        "SELECT BENCHMARK(100000000, MD5('x'))",
        "benchmark",
        "burns CPU",
    ),
    (Mysql, "SELECT GET_LOCK('k', 0)", "get_lock", "takes a lock"),
    (
        Mysql,
        "SELECT RELEASE_LOCK('k')",
        "release_lock",
        "releases someone's lock",
    ),
    (
        Mysql,
        "SELECT LOAD_FILE('/etc/passwd')",
        "load_file",
        "reads a server file",
    ),
    (
        Sqlite,
        "SELECT load_extension('/tmp/x.so')",
        "load_extension",
        "loads native code",
    ),
    (
        Sqlite,
        "SELECT readfile('/etc/passwd')",
        "readfile",
        "reads a file",
    ),
    (
        Sqlite,
        "SELECT writefile('/tmp/x', 'y')",
        "writefile",
        "writes a file",
    ),
    // Name tricks: case, quoting, schema qualification, and nesting do not hide a call.
    (Pg, "SELECT PG_SLEEP(1)", "pg_sleep", "upper case"),
    (Pg, "SELECT \"pg_sleep\"(1)", "pg_sleep", "quoted name"),
    (
        Pg,
        "SELECT pg_catalog.pg_sleep(1)",
        "pg_catalog.pg_sleep",
        "schema-qualified",
    ),
    (
        Pg,
        "SELECT public.lower(a) FROM t",
        "public.lower",
        "a user function shadowing a built-in name",
    ),
    (Pg, "SELECT my_func()", "my_func", "user-defined function"),
    (
        Pg,
        "SELECT * FROM my_set_returning_func()",
        "my_set_returning_func",
        "user-defined table function",
    ),
    (
        Pg,
        "SELECT a FROM t WHERE a = (SELECT pg_sleep(1))",
        "pg_sleep",
        "in a subquery",
    ),
    (
        Pg,
        "SELECT count(*) OVER (ORDER BY nextval('s')) FROM t",
        "nextval",
        "in a window",
    ),
    (
        Pg,
        "SELECT 1 FROM t GROUP BY nextval('s')",
        "nextval",
        "in GROUP BY",
    ),
    (Pg, "VALUES (nextval('s'))", "nextval", "in VALUES"),
    (
        Pg,
        "WITH x AS (SELECT setval('s', 1)) SELECT * FROM x",
        "setval",
        "in a CTE",
    ),
    (
        Pg,
        "SELECT * FROM generate_series(1, nextval('s'))",
        "nextval",
        "in a table function argument",
    ),
    (Mysql, "SELECT `sleep`(1)", "sleep", "backtick-quoted"),
    (Mysql, "SELECT sLeEp(1)", "sleep", "mixed case"),
    // --- attribute notation: PG reads `x.f` as `f(x)` when `x` has no column `f`; the guard
    // must see through it or the allowed functions list is meaningless.
    (
        Pg,
        "SELECT x.pg_terminate_backend FROM pg_stat_activity a, unnest(ARRAY[a.pid]) x",
        "pg_terminate_backend",
        "kills a backend through an unnest alias",
    ),
    (
        Pg,
        "SELECT g.pg_terminate_backend FROM generate_series(1, 100000) g",
        "pg_terminate_backend",
        "kills a backend through a generate_series alias",
    ),
    (
        Pg,
        "SELECT g.pg_sleep FROM unnest(ARRAY[10::float8]) g",
        "pg_sleep",
        "sleeps through an unnest alias",
    ),
    (
        Pg,
        "SELECT f.pg_read_file FROM unnest(ARRAY['/etc/passwd']) f",
        "pg_read_file",
        "reads a server file through an unnest alias",
    ),
    (
        Pg,
        "SELECT (g).pg_cancel_backend FROM generate_series(1, 10) g",
        "pg_cancel_backend",
        "cancels a backend through a parenthesized access",
    ),
    (
        Pg,
        "SELECT d.pg_sleep FROM (SELECT pid FROM pg_stat_activity) d",
        "pg_sleep",
        "derived table alias calls pg_sleep(d)",
    ),
    (
        Pg,
        "SELECT c.pg_sleep FROM (VALUES (1)) c(x)",
        "pg_sleep",
        "VALUES alias calls pg_sleep(c)",
    ),
    (
        Pg,
        "SELECT ('SELECT to_tsvector(pg_cancel_backend(0)::text)'::text).ts_stat",
        "attribute",
        "ts_stat runs the string as SQL: a dot step on a non-column root",
    ),
    (
        Pg,
        "SELECT (x.c).ts_stat FROM unnest(ARRAY[$$SELECT to_tsvector(pg_cancel_backend(0)::text)$$]) AS x(c)",
        "attribute",
        "ts_stat on a compound root runs the string as SQL",
    ),
    (
        Pg,
        "SELECT (CAST('q' AS text)).ts_stat",
        "attribute",
        "any dot step on a non-column root can call a function",
    ),
    (
        Pg,
        "SELECT ts_stat('SELECT 1')",
        "ts_stat",
        "runs its argument as SQL; also refused as a direct call",
    ),
    (
        Pg,
        "WITH x AS (SELECT 1) SELECT x.gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) x",
        "gin_clean_pending_list",
        "CTE name shadowed by a row-function alias: attribute call on the scalar",
    ),
    (
        Pg,
        "WITH x AS (SELECT 1) SELECT x.http_get FROM unnest(ARRAY['http://e']) x",
        "http_get",
        "extension call through a shadowed CTE name",
    ),
    (
        Pg,
        "SELECT unnest.http_get FROM unnest(ARRAY['http://e'])",
        "http_get",
        "unnest's implicit alias calls http_get on the row",
    ),
    (
        Pg,
        "WITH x AS (SELECT 1) SELECT x.upper FROM unnest(ARRAY['abc']) x",
        "upper",
        "a shadowed row-function alias calls any one-argument function",
    ),
    (
        Pg,
        "SELECT \"É\".upper FROM unnest(ARRAY['abc']) É",
        "upper",
        "PG folds the unquoted alias: a quoted qualifier is the same name",
    ),
    (
        Pg,
        "SELECT É.upper FROM unnest(ARRAY['abc']) \"É\"",
        "upper",
        "quoted alias, unquoted qualifier: the same name either way",
    ),
    (
        Pg,
        "SELECT \"É\".gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) É",
        "gin_clean_pending_list",
        "case-variant alias calls an index-maintenance function",
    ),
    (
        Pg,
        "SELECT \"X\".upper FROM unnest(ARRAY['abc']) x",
        "upper",
        "mixed ASCII case: quoting does not decide which alias applies",
    ),
    (
        Pg,
        "SELECT e.path FROM jsonb_each('{}') AS e(k, v)",
        "attribute",
        "PG's jsonb_each has no path column; e.path is path(e)",
    ),
    (
        Pg,
        "SELECT x.upper FROM unnest(ARRAY['abc']) AS x(\"UPPER\")",
        "upper",
        "quoted column UPPER does not answer to x.upper; PG runs upper(x)",
    ),
    (
        Pg,
        "SELECT x.gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) AS x(\"GIN_CLEAN_PENDING_LIST\")",
        "gin_clean_pending_list",
        "quoted column name does not hide an index-maintenance call",
    ),
    (
        Pg,
        "SELECT x.upper FROM generate_series(1,2) AS x(\"Upper\")",
        "upper",
        "generate_series alias column quoted differently than the reference",
    ),
    // --- privacy: the AI must not see other sessions' SQL text or other users' credentials.
    (
        Mysql,
        "SHOW PROCESSLIST",
        "privacy",
        "shows what other sessions run",
    ),
    (
        Mysql,
        "SHOW FULL PROCESSLIST",
        "privacy",
        "shows full query text of other sessions",
    ),
    (
        Mysql,
        "SELECT * FROM information_schema.processlist",
        "privacy",
        "processlist table shows other sessions' SQL",
    ),
    (
        Mysql,
        "SELECT * FROM performance_schema.threads",
        "privacy",
        "performance schema exposes session internals",
    ),
    (
        Mysql,
        "SELECT * FROM sys.processlist",
        "privacy",
        "sys schema wraps the processlist",
    ),
    (
        Mysql,
        "SELECT user, authentication_string FROM mysql.user",
        "privacy",
        "password hashes",
    ),
    (
        Mysql,
        "SELECT * FROM information_schema.`PROCESSLIST`",
        "privacy",
        "quoted uppercase does not hide the processlist",
    ),
    (
        Mysql,
        "SELECT * FROM `INFORMATION_SCHEMA`.`PROCESSLIST`",
        "privacy",
        "quoted schema and table names",
    ),
    (
        Mysql,
        "SELECT * FROM `PERFORMANCE_SCHEMA`.threads",
        "privacy",
        "quoted performance_schema name",
    ),
    (
        Mysql,
        "SELECT * FROM `SYS`.`session`",
        "privacy",
        "quoted sys schema name",
    ),
    (
        Mysql,
        "SELECT * FROM information_schema.innodb_trx",
        "privacy",
        "other sessions' open transactions",
    ),
    (
        Mysql,
        "SELECT * FROM information_schema.innodb_lock_waits",
        "privacy",
        "other sessions' lock waits",
    ),
    (
        Mysql,
        "SHOW ENGINE INNODB STATUS",
        "privacy",
        "engine status exposes other sessions' internals",
    ),
    (
        Mysql,
        "SELECT * FROM mysql.general_log",
        "privacy",
        "the general log holds other sessions' SQL",
    ),
    (
        Mysql,
        "SELECT * FROM mysql.slow_log",
        "privacy",
        "the slow log holds other sessions' SQL",
    ),
    (
        Mysql,
        "SELECT * FROM mysql.global_priv",
        "privacy",
        "MariaDB privilege table with password data",
    ),
    (
        Mysql,
        "SELECT * FROM `MySQL`.`db`",
        "privacy",
        "any table in the system schema, quoted or not",
    ),
    (
        Mysql,
        "SHOW BINLOG EVENTS",
        "privacy",
        "binlog events hold other sessions' statements",
    ),
    (
        Mysql,
        "SHOW RELAYLOG EVENTS",
        "privacy",
        "relay log events hold other sessions' statements",
    ),
    (
        Mysql,
        "SHOW GRANTS FOR root",
        "privacy",
        "lists another user's privileges",
    ),
    (
        Mysql,
        "SHOW GRANTS",
        "privacy",
        "lists the current user's privileges",
    ),
    (
        Pg,
        "SHOW ALL",
        "privacy",
        "some settings hold connection strings",
    ),
    (
        Pg,
        "SHOW primary_conninfo",
        "privacy",
        "the setting can hold a password",
    ),
    (
        Pg,
        "SELECT current_setting('primary_conninfo')",
        "current_setting",
        "reads a setting that can hold a password; also denied as a call",
    ),
    (
        Pg,
        "SELECT * FROM pg_settings",
        "privacy",
        "setting rows can hold connection strings",
    ),
    (
        Pg,
        "SELECT * FROM pg_stat_activity",
        "privacy",
        "shows other sessions' queries",
    ),
    (
        Pg,
        "SELECT query FROM pg_stat_statements",
        "privacy",
        "shows other sessions' query text",
    ),
    (
        Pg,
        "SELECT * FROM pg_catalog.pg_authid",
        "privacy",
        "password hashes",
    ),
    (Pg, "SELECT * FROM pg_shadow", "privacy", "password hashes"),
    (
        Pg,
        "SELECT * FROM pg_user_mappings",
        "privacy",
        "foreign server credentials",
    ),
    // --- value amplification: a single call can exhaust memory before the row cap matters.
    (
        Pg,
        "SELECT repeat('x', 500000000)",
        "large values",
        "builds a 500 MB string",
    ),
    (
        Pg,
        "SELECT lpad('x', 100000000, 'y')",
        "large values",
        "builds a 100 MB string",
    ),
    (
        Mysql,
        "SELECT REPEAT('x', 500000000)",
        "large values",
        "builds a 500 MB string",
    ),
    (
        Mysql,
        "SELECT SPACE(500000000)",
        "large values",
        "builds a 500 MB string",
    ),
    (
        Sqlite,
        "SELECT zeroblob(1000000000)",
        "large values",
        "builds a 1 GB blob",
    ),
    (
        Sqlite,
        "SELECT randomblob(1000000000)",
        "large values",
        "builds a 1 GB blob",
    ),
    (
        Pg,
        "SELECT format('%500000000s','x')",
        "large values",
        "width specifier builds a huge string",
    ),
    (
        Sqlite,
        "SELECT printf('%.*c', 900000000, 'x')",
        "large values",
        "precision specifier builds a huge string",
    ),
    // --- parser tricks.
    (Pg, "", "empty", "nothing to check"),
    (Pg, "   \n\t ", "empty", "nothing to check"),
    (Pg, "-- only a comment", "no statement", "nothing to run"),
    (Pg, "SELEC 1", "parse", "unknown text fails closed"),
    (Pg, "SELECT 'abc", "parse", "unterminated literal"),
    (
        Pg,
        "SELECT\u{00a0}1; DELETE FROM t",
        "parse",
        "non-breaking space between words: fail closed",
    ),
    (
        Pg,
        "SELECT 1\u{2028}; DELETE FROM t",
        "exactly one",
        "unicode line separator does not hide the second statement",
    ),
    (
        Mysql,
        "SELECT 1 /* /* */ ; DELETE FROM t; */",
        "parse",
        "MySQL comments do not nest; the parser must agree",
    ),
    (
        Mysql,
        "SELECT 'x\\', ';DELETE FROM t;--'",
        "parse",
        "backslash-escaped quote: parser and server must agree on the string end",
    ),
];

const READS: &[(Engine, &str, &str)] = &[
    (
        Pg,
        "SELECT c.name, count(o.id) AS n, sum(o.total) FROM customers c LEFT JOIN orders o ON o.customer_id = c.id GROUP BY c.name HAVING count(*) > 1 ORDER BY 2 DESC LIMIT 10",
        "SELECT",
    ),
    (
        Pg,
        "SELECT id, row_number() OVER (PARTITION BY customer_id ORDER BY created_at), lag(total) OVER w FROM orders WINDOW w AS (ORDER BY id)",
        "SELECT",
    ),
    (
        Pg,
        "WITH recent AS (SELECT * FROM orders WHERE created_at > now() - interval '7 days') SELECT count(*) FROM recent",
        "SELECT",
    ),
    (
        Pg,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10) SELECT sum(i) FROM n",
        "SELECT",
    ),
    (
        Pg,
        "SELECT date_trunc('day', created_at) AS d, count(*) FILTER (WHERE status = 'paid') FROM orders GROUP BY 1",
        "SELECT",
    ),
    (
        Pg,
        "SELECT data->>'a', data #> '{b,c}', jsonb_build_object('k', id), jsonb_array_length(data->'x') FROM t",
        "SELECT",
    ),
    (
        Pg,
        "SELECT g, g * 2 FROM generate_series(1, 10) g",
        "SELECT",
    ),
    (
        Pg,
        "SELECT x::int, CAST(y AS text), EXTRACT(year FROM d), coalesce(a, 'n/a'), CASE WHEN a > 1 THEN 'x' END FROM t",
        "SELECT",
    ),
    (
        Pg,
        "SELECT * FROM t WHERE name ~* '^a' AND regexp_replace(name, 'x', 'y') <> '' ",
        "SELECT",
    ),
    (
        Pg,
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY total) FROM orders",
        "SELECT",
    ),
    (
        Pg,
        "SELECT * FROM analytics.events WHERE id IN (SELECT id FROM analytics.events LIMIT 5)",
        "SELECT",
    ),
    (Pg, "SELECT 1 UNION ALL SELECT 2 EXCEPT SELECT 3", "SELECT"),
    (Pg, "VALUES (1, 'a'), (2, 'b')", "SELECT"),
    (Pg, "SELECT ';DELETE FROM t'", "SELECT"),
    (Pg, "SELECT $$;DELETE FROM t$$", "SELECT"),
    (Pg, "SELECT $tag$; DROP TABLE t; $tag$ AS x", "SELECT"),
    (Pg, "SELECT 'a'';DELETE FROM t;--' AS x", "SELECT"),
    (Pg, "SELECT 1 AS \"x;DELETE\"", "SELECT"),
    (Pg, "SELECT 1 /* ; DELETE FROM t */", "SELECT"),
    (Pg, "SELECT 1 /* a /* nested ; DELETE */ b */", "SELECT"),
    (Pg, "SELECT 1 -- ; DELETE FROM t", "SELECT"),
    (Pg, "SELECT 'non\u{00a0}breaking'", "SELECT"),
    (Pg, "SELECT 1;", "SELECT"),
    (Pg, "  SELECT 1 ;  -- trailing", "SELECT"),
    (Pg, "SELECT '/*+ not a hint */'", "SELECT"),
    (Pg, "SHOW search_path", "SHOW"),
    (Pg, "SHOW server_version", "SHOW"),
    (Pg, "EXPLAIN SELECT * FROM orders", "EXPLAIN"),
    (
        Pg,
        "EXPLAIN (FORMAT JSON, COSTS false) SELECT * FROM orders",
        "EXPLAIN",
    ),
    (Pg, "EXPLAIN VERBOSE SELECT 1", "EXPLAIN"),
    (
        Pg,
        "SELECT pg_size_pretty(pg_total_relation_size('orders'))",
        "SELECT",
    ),
    (
        Pg,
        "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.total > 10 ORDER BY o.id LIMIT 5",
        "SELECT",
    ),
    (Pg, "SELECT a.b.c FROM analytics.events a", "SELECT"),
    (Pg, "SELECT x.unnest FROM unnest(ARRAY[1, 2]) x", "SELECT"),
    (Pg, "SELECT unnest.unnest FROM unnest(ARRAY[1])", "SELECT"),
    (Pg, "SELECT e.key FROM jsonb_each('{}') e", "SELECT"),
    (
        Pg,
        "SELECT x.upper FROM unnest(ARRAY['abc']) AS x(\"upper\")",
        "SELECT",
    ),
    (
        Pg,
        "SELECT x.\"UPPER\" FROM unnest(ARRAY['abc']) AS x(\"UPPER\")",
        "SELECT",
    ),
    (
        Pg,
        "SELECT u.generate_series FROM generate_series(1, 5) u",
        "SELECT",
    ),
    (
        Pg,
        "SELECT d.a FROM (SELECT id AS a, name AS b FROM customers) d",
        "SELECT",
    ),
    (Pg, "SELECT v.x FROM (VALUES (1, 'a')) v(x, y)", "SELECT"),
    (
        Pg,
        "WITH x AS (SELECT * FROM orders) SELECT x.id FROM x",
        "SELECT",
    ),
    (
        Pg,
        "SELECT s.customer_id FROM (SELECT customer_id, sum(total) FROM orders GROUP BY 1) s",
        "SELECT",
    ),
    (
        Pg,
        "SELECT o.id, c.customer_id FROM orders o JOIN (SELECT customer_id, count(*) FROM orders GROUP BY 1) c ON o.customer_id = c.customer_id",
        "SELECT",
    ),
    (
        Pg,
        "SELECT o.id FROM (SELECT o.*, 1 AS k FROM orders o) o",
        "SELECT",
    ),
    (Pg, "SELECT t.column1 FROM (VALUES (1)) t", "SELECT"),
    (
        Pg,
        "SELECT g, g * 2 FROM generate_series(1, 10) g",
        "SELECT",
    ),
    (
        Mysql,
        "SELECT DATE_FORMAT(created_at, '%Y-%m') AS m, COUNT(*) FROM orders GROUP BY m",
        "SELECT",
    ),
    (
        Mysql,
        "SELECT JSON_EXTRACT(c_json, '$.a'), c_json->>'$.b' FROM type_zoo",
        "SELECT",
    ),
    (
        Mysql,
        "SELECT IF(a > 1, 'x', 'y'), IFNULL(b, 0), GROUP_CONCAT(c SEPARATOR ',') FROM t GROUP BY a, b",
        "SELECT",
    ),
    (
        Mysql,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM t",
        "SELECT",
    ),
    (
        Mysql,
        "SELECT * FROM t WHERE name LIKE '%;DELETE%' # trailing; comment",
        "SELECT",
    ),
    (Mysql, "SELECT \"a;b\", `c;d` FROM t", "SELECT"),
    (Mysql, "SELECT 'a\\';DELETE FROM t;#'", "SELECT"),
    (Mysql, "SELECT 1 /* plain ; comment */", "SELECT"),
    (Mysql, "SHOW TABLES", "SHOW"),
    (Mysql, "SHOW ENGINES", "SHOW"),
    (Mysql, "SHOW CREATE TABLE orders", "SHOW"),
    (Mysql, "SHOW COLUMNS FROM orders", "SHOW"),
    (Mysql, "DESCRIBE orders", "DESCRIBE"),
    (Mysql, "DESC orders", "DESCRIBE"),
    (Mysql, "EXPLAIN SELECT * FROM orders", "EXPLAIN"),
    (Mysql, "EXPLAIN FORMAT=TREE SELECT * FROM orders", "EXPLAIN"),
    (
        Sqlite,
        "SELECT strftime('%Y', created_at), count(*) FROM orders GROUP BY 1",
        "SELECT",
    ),
    (
        Sqlite,
        "SELECT json_extract(c_json, '$.a'), c_json -> '$.b', iif(1, 'a', 'b'), typeof(x) FROM type_zoo",
        "SELECT",
    ),
    (
        Sqlite,
        "SELECT * FROM pragma_table_info('orders')",
        "SELECT",
    ),
    (Sqlite, "SELECT 'a;b' || char(10)", "SELECT"),
    (Sqlite, "SELECT [a;b] FROM t", "SELECT"),
    (Sqlite, "EXPLAIN QUERY PLAN SELECT * FROM orders", "EXPLAIN"),
    (Sqlite, "PRAGMA table_info(orders)", "PRAGMA"),
    (Sqlite, "PRAGMA table_xinfo('orders')", "PRAGMA"),
    (Sqlite, "pragma main.index_list(orders);", "PRAGMA"),
    (Sqlite, "PRAGMA index_info(idx_orders_customer)", "PRAGMA"),
    (Sqlite, "PRAGMA foreign_key_list(orders)", "PRAGMA"),
    (
        Sqlite,
        "/* lead */ PRAGMA table_info(orders) -- trail",
        "PRAGMA",
    ),
];

#[test]
fn bypasses_are_rejected_with_a_specific_reason() {
    let mut failures = Vec::new();
    for (engine, sql, fragment, why) in BYPASSES {
        match check_ai_read(sql, *engine) {
            Ok(ok) => failures.push(format!(
                "ALLOWED [{engine:?}] {sql:?} as {} ({why})",
                ok.kind
            )),
            Err(r) if !r.reason.to_lowercase().contains(fragment) => failures.push(format!(
                "WRONG REASON [{engine:?}] {sql:?}: {:?} (expected {fragment:?}; {why})",
                r.reason
            )),
            Err(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn plain_reads_are_allowed_with_their_kind() {
    let mut failures = Vec::new();
    for (engine, sql, kind) in READS {
        match check_ai_read(sql, *engine) {
            Ok(ok) if ok.kind == *kind => {}
            Ok(ok) => failures.push(format!("[{engine:?}] {sql:?}: kind {} != {kind}", ok.kind)),
            Err(r) => failures.push(format!("REJECTED [{engine:?}] {sql:?}: {r:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The simple reads the AI uses most must pass on every engine.
#[test]
fn common_reads_pass_on_every_engine() {
    for e in ALL {
        for sql in [
            "SELECT * FROM orders",
            "SELECT count(*), max(total), min(total), avg(total) FROM orders",
            "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.total > 10 ORDER BY o.id LIMIT 5",
            "WITH x AS (SELECT customer_id, sum(total) AS s FROM orders GROUP BY customer_id) SELECT * FROM x ORDER BY s DESC",
            "SELECT lower(name), upper(name), length(name), substr(name, 1, 3), round(total, 2) FROM orders",
        ] {
            check_ai_read(sql, e).unwrap_or_else(|r| panic!("[{e:?}] {sql}: {r:?}"));
        }
    }
}

#[test]
fn rejection_names_the_function_and_the_kind() {
    let r = check_ai_read("SELECT a, pg_sleep(1), my_fn(b) FROM t", Pg).unwrap_err();
    assert!(
        r.reason.contains("my_fn") && r.reason.contains("pg_sleep"),
        "{r:?}"
    );
    assert_eq!(r.kind.as_deref(), Some("SELECT"));
    let r = check_ai_read("DROP TABLE orders", Pg).unwrap_err();
    assert_eq!(r.kind.as_deref(), Some("DROP TABLE"));
    let r = check_ai_read("SELEC 1", Pg).unwrap_err();
    assert!(r.detail.is_some() && r.kind.is_none(), "{r:?}");
}

#[test]
fn guard_check_json_shape() {
    assert_eq!(
        serde_json::to_value(guard_check("SELECT 1", Pg)).unwrap(),
        serde_json::json!({"allowed": true, "kind": "SELECT"})
    );
    let v = serde_json::to_value(guard_check("DELETE FROM t", Sqlite)).unwrap();
    assert_eq!(v["allowed"], false);
    assert_eq!(v["kind"], "DELETE");
    assert!(v["reason"].as_str().unwrap().contains("not a read"));
}

#[test]
fn rejection_becomes_guard_rejected_error() {
    let r = check_ai_read("DELETE FROM t", Pg).unwrap_err();
    let e = crate::core::error::AppError::from(r);
    assert_eq!(e.code, crate::core::error::ErrorCode::GuardRejected);
    assert!(e.message.contains("DELETE is not a read"), "{}", e.message);
}

// ---- write classification ----

fn codes(info: &WriteInfo) -> Vec<WarningCode> {
    info.warnings.iter().map(|w| w.code).collect()
}

#[test]
fn write_warnings() {
    use WarningCode::*;
    let cases: &[(Engine, &str, &[WarningCode])] = &[
        (Pg, "UPDATE orders SET status = 'x' WHERE id = 1", &[]),
        (Pg, "UPDATE orders SET status = 'x'", &[UpdateWithoutWhere]),
        (Mysql, "DELETE FROM orders", &[DeleteWithoutWhere]),
        (Sqlite, "DELETE FROM orders WHERE id = 1", &[]),
        (Pg, "INSERT INTO orders (id) VALUES (1)", &[]),
        (Pg, "DROP TABLE orders", &[Drop]),
        (Pg, "DROP VIEW v", &[Drop]),
        (Pg, "DROP SCHEMA s CASCADE", &[Drop]),
        (Mysql, "DROP DATABASE shop", &[Drop]),
        (Pg, "DROP INDEX i", &[Drop]),
        (Pg, "TRUNCATE orders", &[Truncate]),
        (Mysql, "TRUNCATE TABLE orders", &[Truncate]),
        (Pg, "ALTER TABLE orders DROP COLUMN note", &[AlterDrop]),
        (Pg, "ALTER TABLE orders DROP CONSTRAINT c", &[AlterDrop]),
        (Mysql, "ALTER TABLE orders DROP INDEX i", &[AlterDrop]),
        (Pg, "ALTER TABLE orders ADD COLUMN note text", &[]),
        (Pg, "CREATE TABLE x (id int)", &[]),
        (
            Pg,
            "UPDATE a SET x = 1; DELETE FROM b",
            &[MultipleStatements, UpdateWithoutWhere, DeleteWithoutWhere],
        ),
        (Pg, "UPDAT orders", &[Unparsable]),
        (
            Mysql,
            "DELETE FROM t WHERE id = 1 /*! OR 1 = 1 */",
            &[ExecutableComment],
        ),
    ];
    for (e, sql, want) in cases {
        let info = classify_write(sql, *e, None);
        assert_eq!(codes(&info), *want, "[{e:?}] {sql}: {info:?}");
    }
}

#[test]
fn write_statements_have_kind_and_table() {
    let info = classify_write(
        "INSERT INTO shop.orders (id) VALUES (1); UPDATE customers SET a = 1 WHERE id = 2; DROP TABLE IF EXISTS old_a, old_b",
        Pg,
        None,
    );
    let got: Vec<(&str, Option<&str>)> = info
        .statements
        .iter()
        .map(|s| (s.kind.as_str(), s.table.as_deref()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("INSERT", Some("shop.orders")),
            ("UPDATE", Some("customers")),
            ("DROP TABLE", Some("old_a, old_b")),
        ]
    );
    let drop = info
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::Drop)
        .unwrap();
    assert_eq!(drop.statement, Some(2));
    assert!(drop.message.contains("old_a, old_b"), "{}", drop.message);
}

#[test]
fn write_info_json_shape() {
    let v = serde_json::to_value(classify_write("DELETE FROM t", Sqlite, None)).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "statements": [{"kind": "DELETE", "table": "t"}],
            "warnings": [{"code": "delete_without_where", "message": "DELETE without WHERE removes every row of t", "statement": 0}]
        })
    );
    let v = serde_json::to_value(classify_write("x", Pg, None)).unwrap();
    assert_eq!(v["statements"], serde_json::json!([]));
    assert_eq!(v["warnings"][0]["code"], "unparsable");
    assert!(v["warnings"][0].get("statement").is_none());
}
