//! Phase 3b integration tests: a saved connection marked read-only refuses writes at both
//! barriers — the editor pre-check (`connection_read_only`) and, through the it-only unchecked
//! path, the database session itself (PG startup option, MySQL/MariaDB `SET SESSION TRANSACTION
//! READ ONLY`, SQLite read-only file + `PRAGMA query_only`). Logged in as `sandb_rw`, so the DB
//! user's own permissions never do the blocking. Needs the sandb-test stack (`pnpm db:up`);
//! unreachable databases FAIL, nothing is skipped.
#![cfg(feature = "it")]

mod common;

use serde_json::json;

use common::Ctx;
use sandb_lib::core::result::{QueryRun, RunRequest};
use sandb_lib::core::engine::Engine;
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};

fn net(label: &str, engine: Engine, port: u16, read_only: bool) -> Ctx {
    common::ctx(
        label,
        common::sql_input(label, engine, port, read_only),
        Some(common::rw_password()),
    )
}

/// A temp copy of the SQLite fixture; the fixture itself is never opened.
fn sqlite(read_only: bool) -> Ctx {
    common::sqlite_ctx(read_only)
}

/// A writable and a read-only connection on the same target (same file for SQLite).
fn pair(label: &str, engine: Engine, port: u16) -> (Ctx, Ctx) {
    match engine {
        Engine::Sqlite => {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("shop.db");
            std::fs::copy(common::sqlite_fixture(), &file).unwrap();
            let st = common::state_in(dir.path());
            let w = common::ctx_in(
                dir,
                st,
                label,
                common::sqlite_input(&file, false),
                None,
            );
            let dir2 = tempfile::tempdir().unwrap();
            let st2 = common::state_in(dir2.path());
            let c = common::ctx_in(
                dir2,
                st2,
                label,
                common::sqlite_input(&file, true),
                None,
            );
            (w, c)
        }
        _ => (
            net(label, engine, port, false),
            net(label, engine, port, true),
        ),
    }
}

fn all(read_only: bool) -> Vec<Ctx> {
    let mut v: Vec<Ctx> = common::sqlnet()
        .into_iter()
        .map(|n| net(n.label, n.engine, n.port, read_only))
        .collect();
    v.push(sqlite(read_only));
    v
}

fn req(c: &Ctx, sql: &str) -> RunRequest {
    common::req(&c.conn, sql)
}

/// The editor path: read-only connections run the pre-check first.
async fn editor(c: &Ctx, sql: &str) -> Result<QueryRun, AppError> {
    ops::run_query(&c.st, req(c, sql)).await
}

/// The it-only path with the pre-check skipped: only the database barrier can refuse.
async fn unchecked(c: &Ctx, sql: &str) -> Result<QueryRun, AppError> {
    ops::run_editor_unchecked(&c.st, req(c, sql)).await
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
        // One representative escape per engine: the full statement list is the unit test's
        // job (`core::conn_ro`), this proves the saved connection's pre-check uses it.
        let escapes: &[&str] = match c.conn.engine {
            Engine::Pg => &["SET default_transaction_read_only = off"],
            Engine::Mysql => &["SET SESSION TRANSACTION READ WRITE"],
            Engine::Sqlite => &["PRAGMA query_only = 0"],
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
    let t = common::scratch();
    let pairs: Vec<(Ctx, Ctx)> = common::sqlnet()
        .into_iter()
        .map(|n| pair(n.label, n.engine, n.port))
        .chain(std::iter::once(pair("sqlite", Engine::Sqlite, 0)))
        .collect();
    for (w, c) in pairs {
        let l = &c.label;
        // Setup through a normal writable connection on the same database.
        ops::run_query(&w.st, req(&w, &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)")), )
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
        ops::run_query(&w.st, req(&w, &format!("DROP TABLE IF EXISTS {t}")))
            .await
            .unwrap_or_else(|e| panic!("{l}: cleanup failed: {e:?}"));
        let _ = ops::run_query(&w.st, req(&w, &format!("DROP TABLE IF EXISTS {t}_ddl"))).await;
    }
}

/// Switching `readOnly` on a saved connection closes its pools: the very next run uses a new,
/// read-only session instead of a pooled writable one.
#[tokio::test]
async fn toggling_read_only_reopens_the_pools() {
    let t = common::scratch();
    let w = net("pg16", Engine::Pg, common::pg16(), false);
    let mut input = common::sql_input("pg16-ro", Engine::Pg, common::pg16(), false);
    let (dir, st) = common::state();
    let c = st
        .create_connection(input.clone(), Some(common::rw_password()))
        .unwrap();
    ops::run_query(&w.st, req(&w, &format!("CREATE TABLE {t} (id int PRIMARY KEY)")), )
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
    let updated = ctx.st.update_connection(&c.id, input.clone(), None)
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
    ctx.st.update_connection(&c.id, input.clone(), None)
        .await
        .unwrap();
    unchecked(&ctx, &format!("INSERT INTO {t} VALUES (3)"))
        .await
        .unwrap_or_else(|e| panic!("toggling back did not restore writes: {e:?}"));
    ops::run_query(&w.st, req(&w, &format!("DROP TABLE {t}")))
        .await
        .unwrap();
}
