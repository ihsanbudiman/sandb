//! Phase 3 integration tests: the read-only AI executor (layers 2 and 3) on every engine.
//! Logged in as `sandb_rw`, so the DB user's own permissions never block a write: only the
//! read-only session does. Needs the sandb-test docker stack (`pnpm db:up`). Unreachable
//! databases FAIL; nothing is skipped.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use sandb_lib::core::connections::{self, Connection, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::db::pool::DbPool;
use sandb_lib::core::db::query::{self, Kill, QueryRun, RunRequest};
use sandb_lib::core::db::readonly::{self, ReadLimits};
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

const NET: [(&str, Engine, u16); 4] = [
    ("pg16", Engine::Pg, 55416),
    ("pg13", Engine::Pg, 55413),
    ("mysql", Engine::Mysql, 53384),
    ("mariadb", Engine::Mysql, 53311),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn env_test() -> HashMap<String, String> {
    let p = root().join("testdb/.env.test");
    let raw =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    raw.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

struct Ctx {
    _dir: tempfile::TempDir,
    st: AppState,
    conn: Connection,
    label: String,
}

fn net(label: &str, engine: Engine, port: u16) -> Ctx {
    let e = env_test();
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    let input = ConnectionInput {
        name: format!("it-{label}"),
        engine,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: "shop".into(),
        user: e.get("SANDB_RW_USER").cloned().unwrap_or("sandb_rw".into()),
        ssl: SslMode::Prefer,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    };
    let pw = e
        .get("SANDB_RW_PASSWORD")
        .cloned()
        .expect("SANDB_RW_PASSWORD");
    let conn = connections::create(&st, input, Some(pw)).unwrap();
    Ctx {
        _dir: dir,
        st,
        conn,
        label: label.into(),
    }
}

/// A temp copy of the SQLite fixture; the fixture itself is never opened.
fn sqlite() -> Ctx {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    let src = root().join("testdb/sqlite/shop.db");
    let file = dir.path().join("shop.db");
    std::fs::copy(&src, &file).unwrap_or_else(|e| {
        panic!(
            "missing fixture {} (run `pnpm db:sqlite`): {e}",
            src.display()
        )
    });
    let input = ConnectionInput {
        name: "it-sqlite".into(),
        engine: Engine::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        database: String::new(),
        user: String::new(),
        ssl: SslMode::Prefer,
        file: file.to_string_lossy().into_owned(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    };
    let conn = connections::create(&st, input, None).unwrap();
    Ctx {
        _dir: dir,
        st,
        conn,
        label: "sqlite".into(),
    }
}

fn all() -> Vec<Ctx> {
    let mut v: Vec<Ctx> = NET.iter().map(|(l, e, p)| net(l, *e, *p)).collect();
    v.push(sqlite());
    v
}

/// The user's own (unrestricted) editor path, for setup, checks and cleanup.
async fn editor(c: &Ctx, sql: &str) -> QueryRun {
    query::run(
        &c.st,
        RunRequest {
            conn_id: c.conn.id.clone(),
            database: None,
            sql: sql.into(),
            query_id: uuid::Uuid::new_v4().to_string(),
            limit: Some(10_000),
            tab_id: None,
            sort: None,
        },
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "{} on port {}: editor `{sql}` failed (is `pnpm db:up` running?): {e:?}",
            c.label, c.conn.port
        )
    })
}

fn cell(r: &QueryRun, row: usize, col: usize) -> String {
    let s = r.statements.last().unwrap();
    match &s.rows[row][col] {
        Value::String(v) => v.clone(),
        v => v.to_string(),
    }
}

async fn unchecked(c: &Ctx, sql: &str) -> Result<readonly::AiReadResult, AppError> {
    readonly::run_read_only_unchecked(&c.st, &c.conn.id, None, sql, ReadLimits::default()).await
}

async fn ai(c: &Ctx, sql: &str, limits: ReadLimits) -> Result<readonly::AiReadResult, AppError> {
    readonly::run_ai_read(&c.st, &c.conn.id, None, sql, limits).await
}

fn scratch() -> String {
    format!("it_ro_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// Each write runs with layer 1 skipped; the database must refuse it, and nothing may change.
async fn writes_are_refused(c: &Ctx, writes: &[String], check: &str) -> Vec<String> {
    let before = editor(c, check).await;
    let mut failures = Vec::new();
    for sql in writes {
        match unchecked(c, sql).await {
            // Read-only transaction refusal, or the server rejecting the statement itself
            // (e.g. a stacked statement through the extended protocol).
            Err(e) if matches!(e.code, ErrorCode::ReadOnlyViolation | ErrorCode::Database) => {}
            other => failures.push(format!("{}: `{sql}` -> {other:?}", c.label)),
        }
    }
    let after = editor(c, check).await;
    if before.statements.last().unwrap().rows != after.statements.last().unwrap().rows {
        failures.push(format!(
            "{}: data changed: {:?} -> {:?}",
            c.label,
            before.statements.last().unwrap().rows,
            after.statements.last().unwrap().rows
        ));
    }
    failures
}

#[tokio::test]
async fn layer2_refuses_writes_on_pg() {
    let mut failures = Vec::new();
    for (label, engine, port) in NET.iter().filter(|n| n.1 == Engine::Pg) {
        let c = net(label, *engine, *port);
        let s = scratch();
        editor(
            &c,
            &format!(
                "CREATE SCHEMA {s}; CREATE TABLE {s}.t (id int PRIMARY KEY, v text); \
                 INSERT INTO {s}.t VALUES (1, 'a'), (2, 'b'); CREATE SEQUENCE {s}.seq"
            ),
        )
        .await;
        let writes = [
            format!("INSERT INTO {s}.t VALUES (3, 'c')"),
            format!("UPDATE {s}.t SET v = 'x'"),
            format!("DELETE FROM {s}.t"),
            format!("CREATE TABLE {s}.t2 (a int)"),
            format!("SELECT nextval('{s}.seq')"),
            format!("WITH d AS (DELETE FROM {s}.t RETURNING *) SELECT * FROM d"),
            // A stacked COMMIT must not open a writable transaction (session default is read-only).
            format!("COMMIT; INSERT INTO {s}.t VALUES (4, 'd')"),
        ];
        let check = format!(
            "SELECT (SELECT string_agg(id || ':' || v, ',' ORDER BY id) FROM {s}.t) AS rows, \
             (SELECT last_value || ':' || is_called FROM {s}.seq) AS seq, \
             (SELECT count(*) FROM information_schema.tables WHERE table_schema = '{s}') AS tables"
        );
        let f = writes_are_refused(&c, &writes, &check).await;
        let after = editor(&c, &check).await;
        editor(&c, &format!("DROP SCHEMA {s} CASCADE")).await;
        failures.extend(f);
        assert_eq!(cell(&after, 0, 0), "1:a,2:b", "{label}");
        assert_eq!(cell(&after, 0, 1), "1:false", "{label}: sequence advanced");
        assert_eq!(cell(&after, 0, 2), "1", "{label}: a table was created");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn layer2_refuses_writes_on_mysql_and_mariadb() {
    let mut failures = Vec::new();
    for (label, engine, port) in NET.iter().filter(|n| n.1 == Engine::Mysql) {
        let c = net(label, *engine, *port);
        let t = scratch();
        editor(
            &c,
            &format!(
                "CREATE TABLE {t} (id int PRIMARY KEY, v varchar(10)); INSERT INTO {t} VALUES (1, 'a'), (2, 'b')"
            ),
        )
        .await;
        let writes = [
            format!("INSERT INTO {t} VALUES (3, 'c')"),
            format!("UPDATE {t} SET v = 'x'"),
            format!("DELETE FROM {t}"),
            // DDL commits the open transaction implicitly; the session access mode still applies.
            format!("CREATE TABLE {t}_2 (a int)"),
        ];
        let check = format!(
            "SELECT (SELECT GROUP_CONCAT(CONCAT(id, ':', v) ORDER BY id) FROM {t}) AS r, \
             (SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = '{t}_2') AS n"
        );
        let f = writes_are_refused(&c, &writes, &check).await;
        let after = editor(&c, &check).await;
        editor(&c, &format!("DROP TABLE IF EXISTS {t}, {t}_2")).await;
        failures.extend(f);
        assert_eq!(cell(&after, 0, 0), "1:a,2:b", "{label}");
        assert_eq!(cell(&after, 0, 1), "0", "{label}: a table was created");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn layer2_refuses_writes_on_sqlite() {
    let c = sqlite();
    let writes = [
        "INSERT INTO customers (id, name, email) VALUES (99999, 'x', 'x@x')".to_string(),
        "UPDATE customers SET name = 'x'".to_string(),
        "DELETE FROM customers".to_string(),
        "CREATE TABLE it_ro_t (a int)".to_string(),
        "PRAGMA query_only = 0; DELETE FROM customers".to_string(),
    ];
    let check = "SELECT (SELECT group_concat(id || ':' || name, ',') FROM (SELECT id, name FROM customers ORDER BY id)) AS r, \
                 (SELECT count(*) FROM sqlite_master WHERE name = 'it_ro_t') AS n";
    let f = writes_are_refused(&c, &writes, check).await;
    assert!(f.is_empty(), "{}", f.join("\n"));
    let after = editor(&c, check).await;
    assert_eq!(cell(&after, 0, 1), "0");
    // The editor's own connection still writes: only the AI path is read-only.
    editor(&c, "CREATE TABLE it_rw (a int); DROP TABLE it_rw").await;
}

#[tokio::test]
async fn guard_rejects_before_the_database() {
    for c in all() {
        let e = ai(&c, "DELETE FROM customers", ReadLimits::default())
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::GuardRejected, "{}: {e:?}", c.label);
        assert!(
            !c.st.pools.contains(&c.conn.id),
            "{}: the guard rejection opened a pool",
            c.label
        );
    }
}

#[tokio::test]
async fn slow_reads_time_out_and_the_next_call_works() {
    for c in all() {
        let slow = match (c.conn.engine, c.label.as_str()) {
            (Engine::Pg, _) => {
                "SELECT count(*) FROM generate_series(1, 100000) a CROSS JOIN generate_series(1, 100000) b"
            }
            (Engine::Mysql, _) => {
                "SELECT COUNT(*) FROM analytics.events a CROSS JOIN analytics.events b CROSS JOIN analytics.events c WHERE a.id + b.id + c.id < 0"
            }
            (Engine::Sqlite, _) => {
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000000000) SELECT count(*) FROM n"
            }
            (Engine::Mongo, _) => unreachable!("SQL engines only"),
        };
        let limits = ReadLimits {
            max_rows: 500,
            timeout: Duration::from_secs(1),
        };
        let started = Instant::now();
        let e = ai(&c, slow, limits).await.unwrap_err();
        let took = started.elapsed();
        assert_eq!(e.code, ErrorCode::Timeout, "{}: {e:?}", c.label);
        assert!(took < Duration::from_secs(3), "{}: took {took:?}", c.label);
        let r = ai(&c, "SELECT 1 AS one", ReadLimits::default())
            .await
            .unwrap_or_else(|e| panic!("{}: next call failed: {e:?}", c.label));
        assert_eq!(r.rows, vec![vec![Value::from(1)]], "{}", c.label);
    }
}

#[tokio::test]
async fn rows_are_capped_at_max_rows() {
    for c in all() {
        let events = if c.conn.engine == Engine::Sqlite {
            "events"
        } else {
            "analytics.events"
        };
        let r = ai(
            &c,
            &format!("SELECT * FROM {events} ORDER BY id"),
            ReadLimits::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("{}: {e:?}", c.label));
        assert_eq!((r.rows.len(), r.truncated), (500, true), "{}", c.label);
        assert!(!r.columns.is_empty(), "{}", c.label);
        let r = ai(
            &c,
            &format!("SELECT id FROM {events} ORDER BY id LIMIT 10"),
            ReadLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!((r.rows.len(), r.truncated), (10, false), "{}", c.label);
        let r = ai(
            &c,
            &format!("SELECT id, name FROM {events} WHERE id < 0"),
            ReadLimits::default(),
        )
        .await
        .unwrap();
        assert!(r.rows.is_empty(), "{}", c.label);
        let names: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["id", "name"],
            "{}: empty result keeps columns",
            c.label
        );
    }
}

/// The read-only session settings die with the AI read's connection: the editor still writes.
#[tokio::test]
async fn read_only_state_never_reaches_the_pool() {
    for c in all() {
        for _ in 0..6 {
            ai(&c, "SELECT count(*) FROM customers", ReadLimits::default())
                .await
                .unwrap_or_else(|e| panic!("{}: {e:?}", c.label));
        }
        let t = scratch();
        let r = editor(
            &c,
            &format!("CREATE TABLE {t} (a int); INSERT INTO {t} VALUES (1); SELECT count(*) AS n FROM {t}"),
        )
        .await;
        editor(&c, &format!("DROP TABLE {t}")).await;
        assert_eq!(cell(&r, 0, 0), "1", "{}", c.label);
    }
}

/// A stacked statement must not lift the read-only session: the AI statement runs through the
/// extended/prepared protocol (PG, MySQL/MariaDB) or the authorizer (SQLite), so a second
/// statement never executes. Layer 1 is skipped on purpose to prove layer 2 alone refuses it.
#[tokio::test]
async fn stacked_statements_cannot_lift_read_only() {
    let mut failures = Vec::new();
    for (label, engine, port) in NET {
        let c = net(label, engine, port);
        let s = scratch();
        let (setup, table, check) = match engine {
            Engine::Pg => (
                format!(
                    "CREATE SCHEMA {s}; CREATE TABLE {s}.t (id int PRIMARY KEY, v text); INSERT INTO {s}.t VALUES (1, 'a')"
                ),
                format!("{s}.t"),
                format!("SELECT count(*) FROM {s}.t"),
            ),
            Engine::Mysql => (
                format!(
                    "CREATE TABLE {s}_t (id int PRIMARY KEY, v varchar(10)); INSERT INTO {s}_t VALUES (1, 'a')"
                ),
                format!("{s}_t"),
                format!("SELECT count(*) FROM {s}_t"),
            ),
            _ => unreachable!(),
        };
        editor(&c, &setup).await;
        let writes = match engine {
            Engine::Pg => vec![
                format!("SET TRANSACTION READ WRITE; INSERT INTO {table} VALUES (2, 'b')"),
                format!("COMMIT; BEGIN READ WRITE; INSERT INTO {table} VALUES (3, 'c')"),
                format!(
                    "COMMIT; SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE; COMMIT; INSERT INTO {table} VALUES (4, 'd')"
                ),
                format!(
                    "COMMIT; SET default_transaction_read_only = off; INSERT INTO {table} VALUES (5, 'e')"
                ),
            ],
            Engine::Mysql => vec![
                format!(
                    "COMMIT; SET SESSION TRANSACTION READ WRITE; INSERT INTO {table} VALUES (2, 'b')"
                ),
                format!(
                    "COMMIT; SET SESSION max_execution_time = 0; SET SESSION max_statement_time = 0; INSERT INTO {table} VALUES (3, 'c')"
                ),
            ],
            _ => unreachable!(),
        };
        for sql in &writes {
            match unchecked(&c, sql).await {
                Err(e)
                    if matches!(
                        e.code,
                        ErrorCode::ReadOnlyViolation | ErrorCode::Database | ErrorCode::Timeout
                    ) => {}
                other => failures.push(format!("{label}: `{sql}` -> {other:?}")),
            }
        }
        let n = editor(&c, &check).await;
        assert_eq!(cell(&n, 0, 0), "1", "{label}: a stacked write got through");
        match engine {
            Engine::Pg => {
                editor(&c, &format!("DROP SCHEMA {s} CASCADE")).await;
            }
            Engine::Mysql => {
                editor(&c, &format!("DROP TABLE {s}_t")).await;
            }
            _ => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `PRAGMA query_only = 0` would lift the session; the authorizer must stop it before it runs,
/// and VACUUM INTO must never create its file.
#[tokio::test]
async fn sqlite_stacked_pragma_and_vacuum_into_are_blocked() {
    let c = sqlite();
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("vacuumed.db");
    let mut failures = Vec::new();
    let before = editor(&c, "SELECT count(*) FROM customers").await;
    for sql in [
        format!("PRAGMA query_only = 0; VACUUM INTO '{}'", dump.display()),
        "PRAGMA query_only = 0; INSERT INTO customers (id, name, email) VALUES (99998, 'x', 'x@x')"
            .to_string(),
        "VACUUM".to_string(),
        "ATTACH DATABASE '/tmp/x.db' AS x".to_string(),
        "INSERT INTO customers (id, name, email) VALUES (99997, 'x', 'x@x')".to_string(),
    ] {
        match unchecked(&c, &sql).await {
            Err(e)
                if matches!(
                    e.code,
                    ErrorCode::ReadOnlyViolation | ErrorCode::Database | ErrorCode::GuardRejected
                ) => {}
            other => failures.push(format!("sqlite: `{sql}` -> {other:?}")),
        }
    }
    assert!(!dump.exists(), "VACUUM INTO wrote {}", dump.display());
    let after = editor(&c, "SELECT count(*) FROM customers").await;
    assert_eq!(
        before.statements.last().unwrap().rows,
        after.statements.last().unwrap().rows,
        "data changed"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The statement kinds the guard allows must still run through the prepared-statement path on
/// every engine.
#[tokio::test]
async fn allowed_kinds_run_prepared() {
    for c in all() {
        let sqls: &[&str] = match c.conn.engine {
            Engine::Pg => &[
                "SELECT 1 AS one",
                "SHOW search_path",
                "EXPLAIN SELECT * FROM orders",
            ],
            Engine::Mysql => &[
                "SELECT 1 AS one",
                "SHOW TABLES",
                "DESCRIBE orders",
                "DESC orders",
                "EXPLAIN SELECT * FROM orders",
            ],
            Engine::Sqlite => &["SELECT 1 AS one", "PRAGMA table_info(orders)"],
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        for sql in sqls {
            let r = ai(&c, sql, ReadLimits::default())
                .await
                .unwrap_or_else(|e| panic!("{}: `{sql}`: {e:?}", c.label));
            assert!(!r.rows.is_empty(), "{}: `{sql}` returned no rows", c.label);
        }
    }
}
#[tokio::test]
async fn allowed_statement_kinds_run() {
    for c in all() {
        let sqls: &[&str] = match c.conn.engine {
            Engine::Pg => &[
                "SHOW search_path",
                "EXPLAIN SELECT * FROM orders",
                "WITH x AS (SELECT customer_id, sum(total) s FROM orders GROUP BY 1) SELECT * FROM x ORDER BY s DESC",
                "SELECT ';DELETE FROM orders' AS a, $$;DELETE$$ AS b",
            ],
            Engine::Mysql => &[
                "SHOW TABLES",
                "DESCRIBE orders",
                "EXPLAIN SELECT * FROM orders",
                "SELECT ';DELETE FROM orders' AS a",
            ],
            Engine::Sqlite => &[
                "PRAGMA table_info(orders)",
                "EXPLAIN QUERY PLAN SELECT * FROM orders",
                "SELECT * FROM pragma_table_info('orders')",
                "SELECT ';DELETE FROM orders' AS a",
            ],
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        for sql in sqls {
            let r = ai(&c, sql, ReadLimits::default())
                .await
                .unwrap_or_else(|e| panic!("{}: `{sql}`: {e:?}", c.label));
            assert!(!r.rows.is_empty(), "{}: `{sql}` returned no rows", c.label);
        }
        let n = editor(&c, "SELECT count(*) FROM orders").await;
        assert_ne!(cell(&n, 0, 0), "0", "{}: orders were deleted", c.label);
    }
}

/// PG numeric/date/array values come back as text through the simple protocol; the
/// extended-protocol Parse step is only the single-statement gate.
#[tokio::test]
async fn pg_values_stay_readable() {
    for (label, engine, port) in NET.iter().filter(|n| n.1 == Engine::Pg) {
        let c = net(label, *engine, *port);
        let r = ai(
            &c,
            "SELECT avg(x) AS a, 12.50::numeric AS p, DATE '2026-11-02' AS d, ARRAY[1,2] AS arr \
             FROM generate_series(1,3) x",
            ReadLimits::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(r.rows.len(), 1, "{label}");
        let row = &r.rows[0];
        for (i, v) in row.iter().enumerate() {
            assert!(
                !v.to_string().contains("$bytes"),
                "{label}: col {i} is binary: {v:?}"
            );
        }
        assert_eq!(
            row[0],
            Value::String("2.0000000000000000".into()),
            "{label}"
        );
        assert_eq!(row[1], Value::String("12.50".into()), "{label}");
        assert_eq!(row[2], Value::String("2026-11-02".into()), "{label}");
        // PG array text `{1,2}` is decoded to a JSON array by pg_text.
        assert_eq!(row[3], serde_json::json!([1, 2]), "{label}");
    }
}

/// The safety net's server-side kill on a fresh connection: `KILL QUERY` on MySQL and MariaDB,
/// `pg_cancel_backend` on PG. Drives `server_kill` directly against a sleeping pooled session.
#[tokio::test]
async fn server_kill_stops_a_sleeping_session() {
    for (label, engine, port) in NET {
        let c = net(label, engine, port);
        match connections::pool_for(&c.st, &c.conn.id, Some("shop")).unwrap() {
            DbPool::MySql(p) => {
                let mut conn = p.acquire().await.unwrap();
                let (id,): (i64,) = sqlx::query_as("SELECT CAST(CONNECTION_ID() AS SIGNED)")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
                let sleeper = tokio::spawn(async move {
                    sqlx::query("SELECT SLEEP(30)").execute(&mut *conn).await
                });
                // Let SLEEP actually start before the kill lands.
                tokio::time::sleep(Duration::from_millis(400)).await;
                readonly::server_kill(&c.st, &c.conn, "shop", Kill::MySql(id))
                    .await
                    .unwrap_or_else(|e| panic!("{label}: server_kill: {e:?}"));
                let r = tokio::time::timeout(Duration::from_secs(5), sleeper)
                    .await
                    .unwrap_or_else(|_| panic!("{label}: sleep was not killed"))
                    .unwrap();
                // KILL QUERY makes SLEEP return early (it does not raise an error); the
                // 5 s timeout above is the proof it did not sleep the full 30.
                let _ = r;
            }
            DbPool::Pg(p) => {
                let mut conn = p.acquire().await.unwrap();
                let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
                let sleeper = tokio::spawn(async move {
                    sqlx::query("SELECT pg_sleep(30)").execute(&mut *conn).await
                });
                tokio::time::sleep(Duration::from_millis(400)).await;
                readonly::server_kill(&c.st, &c.conn, "shop", Kill::Pg(pid))
                    .await
                    .unwrap_or_else(|e| panic!("{label}: server_kill: {e:?}"));
                let r = tokio::time::timeout(Duration::from_secs(5), sleeper)
                    .await
                    .unwrap_or_else(|_| panic!("{label}: sleep was not killed"))
                    .unwrap();
                assert!(
                    r.is_err(),
                    "{label}: pg_sleep(30) returned without being killed"
                );
            }
            _ => panic!("{label}: unexpected pool"),
        }
    }
}
