//! Phase 3b integration tests: a saved connection marked read-only refuses writes at both
//! barriers — the editor pre-check (`connection_read_only`) and, through the it-only unchecked
//! path, the database session itself (PG startup option, MySQL/MariaDB `SET SESSION TRANSACTION
//! READ ONLY`, SQLite read-only file + `PRAGMA query_only`). Logged in as `sandb_rw`, so the DB
//! user's own permissions never do the blocking. Needs the sandb-test stack (`pnpm db:up`);
//! unreachable databases FAIL, nothing is skipped.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::json;

use sandb_lib::core::connections::{self, Connection, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::db::query::{self, QueryRun, RunRequest};
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

fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    (dir, st)
}

fn net_input(label: &str, engine: Engine, port: u16, read_only: bool) -> ConnectionInput {
    let e = env_test();
    ConnectionInput {
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
        read_only,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    }
}

fn net(label: &str, engine: Engine, port: u16, read_only: bool) -> Ctx {
    let (dir, st) = state();
    let pw = env_test()
        .get("SANDB_RW_PASSWORD")
        .cloned()
        .expect("SANDB_RW_PASSWORD");
    let conn =
        connections::create(&st, net_input(label, engine, port, read_only), Some(pw)).unwrap();
    Ctx {
        _dir: dir,
        st,
        conn,
        label: label.into(),
    }
}

fn sqlite_input(file: &std::path::Path, read_only: bool) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-sqlite{}", if read_only { "-ro" } else { "" }),
        engine: Engine::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        database: String::new(),
        user: String::new(),
        ssl: SslMode::Prefer,
        file: file.to_string_lossy().into_owned(),
        ai_write: false,
        read_only,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    }
}

/// A temp copy of the SQLite fixture; the fixture itself is never opened.
fn sqlite(read_only: bool) -> Ctx {
    let (dir, st) = state();
    let src = root().join("testdb/sqlite/shop.db");
    let file = dir.path().join("shop.db");
    std::fs::copy(&src, &file).unwrap_or_else(|e| {
        panic!(
            "missing fixture {} (run `pnpm db:sqlite`): {e}",
            src.display()
        )
    });
    let conn = connections::create(&st, sqlite_input(&file, read_only), None).unwrap();
    Ctx {
        _dir: dir,
        st,
        conn,
        label: "sqlite".into(),
    }
}

/// A writable and a read-only connection on the same target (same file for SQLite).
fn pair(label: &str, engine: Engine, port: u16) -> (Ctx, Ctx) {
    match engine {
        Engine::Sqlite => {
            let (dir, st) = state();
            let src = root().join("testdb/sqlite/shop.db");
            let file = dir.path().join("shop.db");
            std::fs::copy(&src, &file).unwrap();
            let w = connections::create(&st, sqlite_input(&file, false), None).unwrap();
            let (dir2, st2) = state();
            let c = connections::create(&st2, sqlite_input(&file, true), None).unwrap();
            (
                Ctx {
                    _dir: dir,
                    st,
                    conn: w,
                    label: label.into(),
                },
                Ctx {
                    _dir: dir2,
                    st: st2,
                    conn: c,
                    label: label.into(),
                },
            )
        }
        _ => (
            net(label, engine, port, false),
            net(label, engine, port, true),
        ),
    }
}

fn all(read_only: bool) -> Vec<Ctx> {
    let mut v: Vec<Ctx> = NET
        .iter()
        .map(|(l, e, p)| net(l, *e, *p, read_only))
        .collect();
    v.push(sqlite(read_only));
    v
}

fn scratch() -> String {
    format!(
        "it_cro_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )
}

fn req(c: &Ctx, sql: &str) -> RunRequest {
    RunRequest {
        conn_id: c.conn.id.clone(),
        database: None,
        sql: sql.into(),
        query_id: uuid::Uuid::new_v4().to_string(),
        limit: None,
        tab_id: None,
        sort: None,
    }
}

/// The editor path: read-only connections run the pre-check first.
async fn editor(c: &Ctx, sql: &str) -> Result<QueryRun, AppError> {
    query::run(&c.st, req(c, sql)).await
}

/// The it-only path with the pre-check skipped: only the database barrier can refuse.
async fn unchecked(c: &Ctx, sql: &str) -> Result<QueryRun, AppError> {
    query::run_editor_unchecked(&c.st, req(c, sql)).await
}

/// The error a refused run returns; panics with the result when the run went through.
fn must_fail(label: &str, sql: &str, r: Result<QueryRun, AppError>) -> AppError {
    r.err()
        .unwrap_or_else(|| panic!("{label}: `{sql}` ran on a read-only connection"))
}

/// Any read-only connection: editor INSERT/UPDATE/DDL stop at the pre-check.
#[tokio::test]
async fn pre_check_refuses_writes_on_every_engine() {
    for c in all(true) {
        let l = &c.label;
        for sql in [
            "INSERT INTO products (id, sku, name, price) VALUES (9999, 'X', 'X', 1)",
            "UPDATE products SET price = 0 WHERE id = 1",
            "DELETE FROM products WHERE id = 9999",
            "CREATE TABLE it_cro_t (a int)",
            "SELECT * FROM products FOR UPDATE",
        ] {
            let e = must_fail(l, sql, editor(&c, sql).await);
            assert_eq!(e.code, ErrorCode::ConnectionReadOnly, "{l}: `{sql}`: {e:?}");
            assert!(
                e.message.contains(&c.conn.name),
                "{l}: names the connection: {e}"
            );
        }
        // Reads still work.
        editor(&c, "SELECT count(*) AS n FROM products")
            .await
            .unwrap_or_else(|e| panic!("{l}: SELECT on a read-only connection failed: {e:?}"));
    }
}

/// Statements that would undo the session barrier stop at the pre-check on every engine, with
/// the pre-check's own message (never a database error), and the session stays read-only.
#[tokio::test]
async fn pre_check_refuses_session_mode_changes() {
    for c in all(true) {
        let l = &c.label;
        let escapes: &[&str] = match c.conn.engine {
            Engine::Pg => &[
                "SET default_transaction_read_only = off",
                "SET SESSION default_transaction_read_only TO off",
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
                "BEGIN READ WRITE",
                "RESET ALL",
                "DISCARD ALL",
                // Review item 4 bypass forms.
                "SET transaction_read_only = off",
                "SET LOCAL transaction_read_only = off",
                "SET \"default_transaction_read_only\" = off",
                "SELECT set_config('default_transaction_read_only', 'off', false)",
                "SELECT pg_catalog.set_config('transaction_read_only', 'off', false)",
            ],
            Engine::Mysql => &[
                "SET SESSION TRANSACTION READ WRITE",
                "SET transaction_read_only = 0",
                "START TRANSACTION READ WRITE",
                // Review item 4 bypass forms.
                "SET @@transaction_read_only = 0",
                "SET @@tx_read_only = 0",
                "SET @@session.transaction_read_only = 0",
                "SET @@SESSION.tx_read_only = OFF",
            ],
            Engine::Sqlite => &["PRAGMA query_only = 0", "PRAGMA query_only = OFF"],
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        for sql in escapes {
            let e = must_fail(l, sql, editor(&c, sql).await);
            assert_eq!(e.code, ErrorCode::ConnectionReadOnly, "{l}: `{sql}`: {e:?}");
            assert!(e.message.contains("is read-only"), "{l}: `{sql}`: {e}");
        }
        // None of them reached the session: it still reports read-only. Probed past the
        // pre-check, which refuses even the reading form `PRAGMA query_only`.
        let probe = match (c.conn.engine, l.as_str()) {
            (Engine::Pg, _) => "SHOW transaction_read_only",
            (Engine::Mysql, "mariadb") => "SELECT @@session.tx_read_only AS ro",
            (Engine::Mysql, _) => "SELECT @@session.transaction_read_only AS ro",
            (Engine::Sqlite, _) => "PRAGMA query_only",
            (Engine::Mongo, _) => unreachable!("SQL engines only"),
        };
        let r = unchecked(&c, probe)
            .await
            .unwrap_or_else(|e| panic!("{l}: `{probe}` failed: {e:?}"));
        let v = &r.statements[0].rows[0][0];
        assert!(
            [json!(1), json!("1"), json!("on"), json!(true)].contains(v),
            "{l}: session is no longer read-only: `{probe}` = {v}"
        );
    }
}

/// The pre-check skipped, the database session itself refuses the write. PG's barrier is a
/// startup option, MySQL/MariaDB's an after_connect `SET SESSION TRANSACTION READ ONLY`,
/// SQLite's a read-only file plus `PRAGMA query_only` (a stacked `query_only = 0` does not
/// help: the file itself is read-only).
#[tokio::test]
async fn database_barrier_refuses_writes_on_every_engine() {
    let t = scratch();
    let pairs: Vec<(Ctx, Ctx)> = NET
        .iter()
        .map(|(l, e, p)| pair(l, *e, *p))
        .chain(std::iter::once(pair("sqlite", Engine::Sqlite, 0)))
        .collect();
    for (w, c) in pairs {
        let l = &c.label;
        // Setup through a normal writable connection on the same database.
        query::run(
            &w.st,
            req(&w, &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)")),
        )
        .await
        .unwrap_or_else(|e| panic!("{l}: scratch setup failed (is `pnpm db:up` running?): {e:?}"));

        for sql in [
            format!("INSERT INTO {t} VALUES (1, 1)"),
            format!("UPDATE {t} SET v = 2 WHERE id = 1"),
            format!("DELETE FROM {t}"),
            format!("CREATE TABLE {t}_ddl (a int)"),
            format!("DROP TABLE {t}"),
        ] {
            let e = must_fail(l, &sql, unchecked(&c, &sql).await);
            assert_eq!(e.code, ErrorCode::Database, "{l}: `{sql}`: {e:?}");
        }
        // SQLite extra: turning query_only off cannot help, the file itself is read-only.
        if c.conn.engine == Engine::Sqlite {
            let sql = format!("PRAGMA query_only = 0; INSERT INTO {t} VALUES (9, 9)");
            let e = must_fail(l, &sql, unchecked(&c, &sql).await);
            assert_eq!(e.code, ErrorCode::Database, "{l}: {e:?}");
        }
        let r = unchecked(&c, &format!("SELECT count(*) AS n FROM {t}"))
            .await
            .unwrap_or_else(|e| panic!("{l}: SELECT failed: {e:?}"));
        assert_eq!(
            r.statements[0].rows[0][0],
            json!(0),
            "{l}: a write landed despite the barrier"
        );
        query::run(&w.st, req(&w, &format!("DROP TABLE IF EXISTS {t}")))
            .await
            .unwrap_or_else(|e| panic!("{l}: cleanup failed: {e:?}"));
        let _ = query::run(&w.st, req(&w, &format!("DROP TABLE IF EXISTS {t}_ddl"))).await;
    }
}

/// Switching `readOnly` on a saved connection closes its pools: the very next run uses a new,
/// read-only session instead of a pooled writable one.
#[tokio::test]
async fn toggling_read_only_reopens_the_pools() {
    let t = scratch();
    let w = net("pg16", Engine::Pg, 55416, false);
    let mut input = net_input("pg16-ro", Engine::Pg, 55416, false);
    let (dir, st) = state();
    let pw = env_test().get("SANDB_RW_PASSWORD").cloned().unwrap();
    let c = connections::create(&st, input.clone(), Some(pw)).unwrap();
    query::run(
        &w.st,
        req(&w, &format!("CREATE TABLE {t} (id int PRIMARY KEY)")),
    )
    .await
    .unwrap();

    // A pooled writable session exists now.
    let ctx = Ctx {
        _dir: dir,
        st,
        conn: c.clone(),
        label: "pg16".into(),
    };
    unchecked(&ctx, &format!("INSERT INTO {t} VALUES (1)"))
        .await
        .unwrap_or_else(|e| panic!("write through the pooled session failed: {e:?}"));

    input.read_only = true;
    let updated = connections::update(&ctx.st, &c.id, input.clone(), None)
        .await
        .unwrap();
    assert!(updated.read_only);
    let sql = format!("INSERT INTO {t} VALUES (2)");
    let e = must_fail(&ctx.label, &sql, unchecked(&ctx, &sql).await);
    assert_eq!(
        e.code,
        ErrorCode::Database,
        "a pooled writable session survived the toggle: {e:?}"
    );

    input.read_only = false;
    connections::update(&ctx.st, &c.id, input.clone(), None)
        .await
        .unwrap();
    unchecked(&ctx, &format!("INSERT INTO {t} VALUES (3)"))
        .await
        .unwrap_or_else(|e| panic!("toggling back did not restore writes: {e:?}"));
    query::run(&w.st, req(&w, &format!("DROP TABLE {t}")))
        .await
        .unwrap();
}
