//! Phase 3b integration tests: manual commit on Production. Each editor tab gets its own
//! dedicated session with an open transaction; writes stay invisible to every other session
//! until Commit, Rollback discards them, and the session survives several runs while its
//! transaction is open (it closes with the transaction). Needs the
//! sandb-test stack (`pnpm db:up`); unreachable databases FAIL, nothing is skipped.
#![cfg(feature = "it")]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use sqlx::Connection as _;

use common::Ctx;
use sandb_lib::core::connections::{ConnectionInput, Env};
use sandb_lib::core::result::{ApplyEditsRequest, EditKind, EditOperation, TypedValue};
use sandb_lib::core::result::ExportRequest;
use sandb_lib::core::result::{QueryRun, RunRequest};
use sandb_lib::core::db::session::{self, TabStatus};
use sandb_lib::core::engine::Engine;
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};

fn input(engine: Engine, port: u16, env: Env, file: &Path) -> ConnectionInput {
    let mut i = if engine == Engine::Sqlite {
        common::sqlite_input(file, false)
    } else {
        common::sql_input("session", engine, port, false)
    };
    i.name = format!("it-{engine:?}-{port}-{env:?}");
    i.env = env;
    i
}

/// A Production connection on `engine`; for SQLite a temp copy of the fixture.
fn prod_ctx(label: &str, engine: Engine, port: u16) -> Ctx {
    if engine == Engine::Sqlite {
        let (dir, st) = common::state();
        let file = dir.path().join("shop.db");
        std::fs::copy(common::sqlite_fixture(), &file).unwrap();
        common::ctx_in(
            dir,
            st,
            label,
            input(engine, port, Env::Production, &file),
            None,
        )
    } else {
        common::ctx(
            label,
            input(engine, port, Env::Production, Path::new("")),
            Some(common::rw_password()),
        )
    }
}

fn prod(engine: Engine, port: u16) -> Vec<Ctx> {
    common::sqlnet()
        .into_iter()
        .filter(|n| n.engine == engine && (port == 0 || n.port == port))
        .map(|n| prod_ctx(n.label, n.engine, n.port))
        .collect()
}

fn prod_all() -> Vec<Ctx> {
    let mut v: Vec<Ctx> = common::sqlnet()
        .into_iter()
        .map(|n| prod_ctx(n.label, n.engine, n.port))
        .collect();
    v.push(prod_ctx("sqlite", Engine::Sqlite, 0));
    v
}

fn req(c: &Ctx, sql: &str, tab_id: Option<&str>) -> RunRequest {
    let mut r = common::req(&c.conn, sql);
    r.tab_id = tab_id.map(str::to_string);
    r
}

/// A run inside the tab's production session (manual commit).
async fn run_tab(c: &Ctx, sql: &str, tab: &str) -> Result<QueryRun, AppError> {
    ops::run_query(&c.st, req(c, sql, Some(tab))).await
}

async fn run_tab_ok(c: &Ctx, sql: &str, tab: &str) -> QueryRun {
    run_tab(c, sql, tab).await.unwrap_or_else(|e| {
        panic!(
            "{}: `{sql}` failed (is `pnpm db:up` running?): {e:?}",
            c.label
        )
    })
}

/// A run outside any tab session: another session of the same user.
async fn run_other(c: &Ctx, sql: &str) -> QueryRun {
    ops::run_query(&c.st, req(c, sql, None))
        .await
        .unwrap_or_else(|e| panic!("{}: other-session `{sql}` failed: {e:?}", c.label))
}

async fn status(c: &Ctx, tab: &str) -> Option<TabStatus> {
    session::status(&c.st, tab)
        .await
        .unwrap_or_else(|e| panic!("{}: tab_status failed: {e:?}", c.label))
}

async fn commit(c: &Ctx, tab: &str) -> Result<(), AppError> {
    session::end_tx_for(&c.st, tab, true).await
}

async fn rollback(c: &Ctx, tab: &str) -> Result<(), AppError> {
    session::end_tx_for(&c.st, tab, false).await
}

fn cell(r: &QueryRun, row: usize, col: usize) -> serde_json::Value {
    common::cell(r, row, col)
}

/// Waits until `tab` is a live session with work, instead of sleeping a guessed interval.
async fn wait_pending(c: &Ctx, tab: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !c.st.sessions().pending_tab_ids().iter().any(|t| t == tab) {
        assert!(
            std::time::Instant::now() < deadline,
            "{}: tab {tab} never became pending",
            c.label
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Waits until the run/save with `query_id` is registered, then cancels it.
async fn cancel_started(c: &Ctx, query_id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while ops::query_progress(&c.st, query_id).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "{}: {query_id} never started",
            c.label
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    ops::cancel_query(&c.st, query_id)
        .await
        .expect("cancel the save");
}

/// Creates the scratch table outside a session (autocommit), and drops it at the end of `body`.
async fn with_scratch(c: &Ctx, body: impl AsyncFnOnce(&Ctx, &str)) {
    let t = common::scratch();
    run_other(c, &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)")).await;
    body(c, &t).await;
    // A failed drop is a test failure; during unwinding it would abort the process.
    if !std::thread::panicking() {
        run_other(c, &format!("DROP TABLE IF EXISTS {t}")).await;
    }
}

/// The core guarantee of manual commit: a write in tab A is invisible to another session
/// until Commit, and Rollback discards it.
#[tokio::test]
async fn write_is_invisible_until_commit_and_rollback_discards() {
    for c in prod_all() {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 10)"), "tab-a").await;

            // The tab's own session sees its uncommitted write; no other session does.
            let own = run_tab_ok(c, &format!("SELECT count(*) AS n FROM {t}"), "tab-a").await;
            assert_eq!(
                cell(&own, 0, 0),
                json!(1),
                "{l}: the tab must see its own write"
            );
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(
                cell(&other, 0, 0),
                json!(0),
                "{l}: uncommitted write leaked"
            );
            let s = status(c, "tab-a").await.unwrap();
            assert!(s.tx_open && s.pending, "{l}: {s:?}");

            rollback(c, "tab-a").await.unwrap();
            assert_eq!(
                status(c, "tab-a").await,
                None,
                "{l}: session kept after rollback"
            );

            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 20)"), "tab-a").await;
            commit(c, "tab-a").await.unwrap();
            let other = run_other(c, &format!("SELECT v FROM {t}")).await;
            assert_eq!(
                cell(&other, 0, 0),
                json!(20),
                "{l}: committed write invisible"
            );
            // Without a transaction the session has nothing to hold: it closes.
            assert_eq!(
                status(c, "tab-a").await,
                None,
                "{l}: session kept after commit"
            );
        })
        .await;
    }
}

/// Several runs reuse one session: the transaction opened on the first run is still the one
/// the user commits after run 3+.
#[tokio::test]
async fn session_survives_several_runs() {
    for c in prod(Engine::Pg, common::pg16())
        .into_iter()
        .chain(prod(Engine::Mysql, common::mysql()))
        .chain(std::iter::once(prod_ctx("sqlite", Engine::Sqlite, 0)))
    {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            let backend = match c.conn.engine {
                Engine::Pg => "SELECT pg_backend_pid() AS b",
                Engine::Mysql => "SELECT CAST(CONNECTION_ID() AS SIGNED) AS b",
                // SQLite has no backend id; a temp table lives on the connection itself.
                Engine::Sqlite => "CREATE TEMP TABLE it_keep (a int)",
                Engine::Mongo => unreachable!("SQL engines only"),
            };
            // The write opens the transaction (and the session); a plain read alone would not.
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            let first = run_tab_ok(c, backend, "tab").await;
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;
            match c.conn.engine {
                Engine::Sqlite => {
                    // Same connection: the temp table from run 1 is still there.
                    let r = run_tab_ok(
                        c,
                        "SELECT count(*) AS n FROM sqlite_temp_master WHERE name = 'it_keep'",
                        "tab",
                    )
                    .await;
                    assert_eq!(cell(&r, 0, 0), json!(1), "{l}: the session was replaced");
                }
                _ => {
                    let third = run_tab_ok(c, backend, "tab").await;
                    assert_eq!(
                        cell(&first, 0, 0),
                        cell(&third, 0, 0),
                        "{l}: runs 2 and 4 used different server sessions"
                    );
                }
            }
            commit(c, "tab").await.unwrap();
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(cell(&other, 0, 0), json!(2), "{l}: not all runs committed");
        })
        .await;
    }
}

/// MySQL commits DDL implicitly even inside a transaction: status must report it and the
/// result must carry the notice.
#[tokio::test]
async fn mysql_ddl_commits_implicitly() {
    for c in prod(Engine::Mysql, common::mysql())
        .into_iter()
        .chain(prod(Engine::Mysql, common::mariadb()))
    {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            let s = status(c, "tab").await.unwrap();
            assert!(s.pending, "{l}: insert not pending before DDL");

            let t2 = format!("{t}_ddl");
            let r = run_tab_ok(c, &format!("CREATE TABLE {t2} (a int)"), "tab").await;
            assert!(
                r.notices
                    .iter()
                    .any(|n| n.contains("MySQL committed this DDL immediately")
                        && n.contains("including the uncommitted changes")),
                "{l}: notices {:?}",
                r.notices
            );
            assert_eq!(
                status(c, "tab").await,
                None,
                "{l}: the implicit commit still looks open"
            );
            // The insert before the DDL was committed with it.
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(cell(&other, 0, 0), json!(1), "{l}");
            run_other(c, &format!("DROP TABLE {t2}")).await;
        })
        .await;
    }
}

/// A failed statement aborts the PG transaction: the error says to roll back, the session
/// stays open (Rollback available), and Rolling back restores it.
#[tokio::test]
async fn pg_aborted_transaction_keeps_rollback_available() {
    for c in prod(Engine::Pg, common::pg16()).into_iter() {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            let e = run_tab(c, "SELECT * FROM no_such_table_it", "tab")
                .await
                .unwrap_err();
            assert!(
                e.message
                    .contains("Transaction aborted, roll back to continue"),
                "{l}: {e:?}"
            );
            let s = status(c, "tab").await.unwrap();
            assert!(
                s.tx_open && s.aborted && s.pending,
                "{l}: aborted transaction must stay open and pending: {s:?}"
            );

            rollback(c, "tab").await.unwrap();
            // The session is usable again; the failed INSERT was discarded.
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;
            commit(c, "tab").await.unwrap();
            let other = run_other(c, &format!("SELECT id FROM {t} ORDER BY id")).await;
            assert_eq!(cell(&other, 0, 0), json!(2), "{l}");
        })
        .await;
    }
}

/// A user-typed COMMIT inside a run must end the transaction the status reports — otherwise
/// the next write would autocommit silently.
#[tokio::test]
async fn user_typed_commit_inside_a_run_is_seen() {
    for c in prod(Engine::Pg, common::pg16()).into_iter() {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1); COMMIT"), "tab").await;
            assert_eq!(status(c, "tab").await, None, "{l}");
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(
                cell(&other, 0, 0),
                json!(1),
                "{l}: user COMMIT did not commit"
            );
        })
        .await;
    }
}

/// A run without pending changes leaves no transaction behind: a plain read never opens one,
/// and a transaction that wrote nothing ends with the run — otherwise the tab would sit idle
/// in a transaction the user cannot end (Commit / Rollback need pending work).
#[tokio::test]
async fn clean_runs_leave_no_transaction() {
    for c in prod_all() {
        let l = &c.label;
        let not_plain = match c.conn.engine {
            Engine::Pg => "SET LOCAL statement_timeout = 5000; SELECT 1",
            // MySQL: `innodb_trx` needs PROCESS, which the test user lacks, so any
            // write-capable statement (even `SET @x`) counts as pending there.
            Engine::Mysql => "START TRANSACTION; SELECT 1",
            Engine::Sqlite => "PRAGMA user_version; SELECT 1",
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        for sql in ["SELECT 1", not_plain] {
            run_tab_ok(&c, sql, "tab").await;
            assert_eq!(status(&c, "tab").await, None, "{l}: `{sql}` left a session");
        }
        // A read after a pending write stays inside the transaction.
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            run_tab_ok(c, "SELECT 1", "tab").await;
            let s = status(c, "tab").await.unwrap();
            assert!(s.tx_open && s.pending, "{l}: {s:?}");
            rollback(c, "tab").await.unwrap();
        })
        .await;
    }
}

/// `close_tab_session` discards the pending work (rollback on close).
#[tokio::test]
async fn close_session_discards_pending_work() {
    for c in prod(Engine::Pg, common::pg16()).into_iter() {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            c.st.sessions().close("tab").await;
            assert_eq!(status(c, "tab").await, None, "{l}: session was not dropped");
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(
                cell(&other, 0, 0),
                json!(0),
                "{l}: close committed instead of rollback"
            );
            // The next run opens a fresh session.
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;
            commit(c, "tab").await.unwrap();
        })
        .await;
    }
}

/// Commit / rollback on a tab with no open transaction is a clear error, not a silent no-op.
#[tokio::test]
async fn commit_and_rollback_need_an_open_transaction() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    for r in [commit(&c, "none").await, rollback(&c, "none").await] {
        let e = r.unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    }
    // A read-only run leaves nothing to commit either.
    run_tab_ok(&c, "SELECT 1", "tab").await;
    let e = commit(&c, "tab").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}

/// Switching the tab's database closes the old session (its pending work rolls back).
#[tokio::test]
async fn switching_database_rolls_back_the_old_session() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
        // The frontend only switches the target after the user's decision; here the pending
        // insert is rolled back when the old session dies.
        let mut r = req(c, "SELECT 1", Some("tab"));
        r.database = Some("shop_staging".into());
        ops::run_query(&c.st, r)
            .await
            .unwrap_or_else(|e| panic!("{l}: run on the new target failed: {e:?}"));
        let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
        assert_eq!(
            cell(&other, 0, 0),
            json!(0),
            "{l}: the switch committed the old work"
        );
    })
    .await;
}

/// A write in tab A is invisible to tab B on the same connection: separate sessions.
#[tokio::test]
async fn tabs_on_one_connection_have_separate_sessions() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab-a").await;
        let b = run_tab_ok(c, &format!("SELECT count(*) AS n FROM {t}"), "tab-b").await;
        assert_eq!(
            cell(&b, 0, 0),
            json!(0),
            "{l}: tab B saw tab A's uncommitted write"
        );
        commit(c, "tab-a").await.unwrap();
        let b = run_tab_ok(c, &format!("SELECT count(*) AS n FROM {t}"), "tab-b").await;
        assert_eq!(cell(&b, 0, 0), json!(1), "{l}");
        assert_eq!(
            status(c, "tab-b").await,
            None,
            "{l}: a read opened a session"
        );
    })
    .await;
}

/// Non-Production connections keep phase-2 autocommit behaviour even when a run names a tab.
#[tokio::test]
async fn non_production_runs_stay_autocommit() {
    let (dir, st) = common::state();
    let pw = common::rw_password();
    let conn = st.create_connection(input(Engine::Pg, common::pg16(), Env::Local, Path::new("")), Some(pw), )
    .unwrap();
    let c = Ctx {
        _dir: dir,
        st,
        conn,
        label: "pg16-local".into(),
    };
    let t = common::scratch();
    run_other(&c, &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)")).await;
    ops::run_query(&c.st, req(&c, &format!("INSERT INTO {t} VALUES (1, 1)"), Some("tab")), )
    .await
    .unwrap();
    let other = run_other(&c, &format!("SELECT count(*) AS n FROM {t}")).await;
    assert_eq!(
        cell(&other, 0, 0),
        json!(1),
        "writes must autocommit off Production"
    );
    assert_eq!(status(&c, "tab").await, None, "no session may exist");
    run_other(&c, &format!("DROP TABLE {t}")).await;
}

/// Production + read-only: no session is ever opened (the UI hides Commit / Rollback), reads
/// work, writes are refused by the read-only pre-check.
#[tokio::test]
async fn production_read_only_never_opens_a_session() {
    let (dir, st) = common::state();
    let pw = common::rw_password();
    let mut i = input(Engine::Pg, common::pg16(), Env::Production, Path::new(""));
    i.read_only = true;
    let conn = st.create_connection(i, Some(pw)).unwrap();
    let c = Ctx {
        _dir: dir,
        st,
        conn,
        label: "pg16-prod-ro".into(),
    };
    let r = run_tab(&c, "SELECT 1", "tab").await.unwrap();
    assert_eq!(cell(&r, 0, 0), json!(1));
    assert_eq!(
        status(&c, "tab").await,
        None,
        "a read-only tab must not get a session"
    );
    let e = run_tab(
        &c,
        "INSERT INTO products (id, sku, name, price) VALUES (1, 'x', 'x', 1)",
        "tab",
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::ConnectionReadOnly, "{e:?}");
}

/// "Load more" re-runs the read in the same tab session, so paging sees the tab's own
/// uncommitted rows.
#[tokio::test]
async fn load_more_reruns_inside_the_tab_session() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_other(
            c,
            &format!("INSERT INTO {t} SELECT g, g FROM generate_series(1, 5) g"),
        )
        .await;
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (6, 6)"), "tab").await;
        let mut r = req(c, &format!("SELECT id FROM {t} ORDER BY id"), Some("tab"));
        r.limit = Some(3);
        let first = ops::run_query(&c.st, r).await.unwrap();
        assert!(first.statements[0].truncated, "{l}");
        // "Load more" is the same read at a higher limit, in the same session.
        let mut r = req(c, &format!("SELECT id FROM {t} ORDER BY id"), Some("tab"));
        r.limit = Some(100);
        let more = ops::run_query(&c.st, r).await.unwrap();
        assert_eq!(
            more.statements[0].row_count, 6,
            "{l}: the uncommitted row is missing"
        );
        // Another session pages the same table but never sees row 6.
        let mut r = req(c, &format!("SELECT id FROM {t} ORDER BY id"), None);
        r.limit = Some(100);
        let outside = ops::run_query(&c.st, r).await.unwrap();
        assert_eq!(outside.statements[0].row_count, 5, "{l}");
        rollback(c, "tab").await.unwrap();
    })
    .await;
}

/// A read over the row limit inside a session drains the rest of the result instead of
/// cutting the connection off: the pending work survives and Commit still lands it.
#[tokio::test]
async fn limited_read_keeps_pending_work_committable() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_other(
            c,
            &format!("INSERT INTO {t} SELECT g, g FROM generate_series(1, 5) g"),
        )
        .await;
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (6, 6)"), "tab").await;
        let mut r = req(c, &format!("SELECT id FROM {t} ORDER BY id"), Some("tab"));
        r.limit = Some(3);
        let r = ops::run_query(&c.st, r).await.unwrap();
        assert!(r.statements[0].truncated, "{l}");
        assert_eq!(r.statements[0].row_count, 3, "{l}");
        // The drain left the transaction usable: the write is still pending and commits.
        assert!(status(c, "tab").await.unwrap().pending, "{l}");
        commit(c, "tab").await.unwrap();
        let outside = run_other(c, &format!("SELECT count(*) FROM {t}")).await;
        assert_eq!(cell(&outside, 0, 0), json!(6), "{l}: the insert was lost");
    })
    .await;
}

/// CSV export inside the tab session sees the pending rows; the same export without the tab
/// does not.
#[tokio::test]
async fn export_reads_inside_the_tab_transaction() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_other(c, &format!("INSERT INTO {t} VALUES (1, 1)")).await;
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("inside.csv");
        let r = ops::export_csv(
            &c.st,
            ExportRequest {
                conn_id: c.conn.id.clone(),
                database: None,
                sql: format!("SELECT id FROM {t} ORDER BY id"),
                tab_id: Some("tab".into()),
            },
            &file,
        )
        .await
        .unwrap_or_else(|e| panic!("{l}: in-session export failed: {e:?}"));
        assert_eq!(r.rows, 2, "{l}: the export must include the pending row");

        let file2 = dir.path().join("outside.csv");
        let r = ops::export_csv(
            &c.st,
            ExportRequest {
                conn_id: c.conn.id.clone(),
                database: None,
                sql: format!("SELECT id FROM {t} ORDER BY id"),
                tab_id: None,
            },
            &file2,
        )
        .await
        .unwrap_or_else(|e| panic!("{l}: pooled export failed: {e:?}"));
        assert_eq!(r.rows, 1, "{l}: the pooled export saw uncommitted work");
        rollback(c, "tab").await.unwrap();
    })
    .await;
}

/// Bound for a call that must not hang (pool close waiting on a lent-out connection).
const NO_HANG: std::time::Duration = std::time::Duration::from_secs(15);

/// Renaming a connection keeps a live tab session (its transaction stays open). An edit that
/// reconnects, or a delete, neither hangs nor commits: the session closes first (rolling back),
/// then the pool.
#[tokio::test]
async fn conn_edit_and_delete_with_a_live_session_do_not_hang() {
    for c in prod(Engine::Pg, common::pg16())
        .into_iter()
        .chain(prod(Engine::Mysql, common::mysql()))
    {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
            let mut i = input(c.conn.engine, c.conn.port, Env::Production, Path::new(""));
            i.name = format!("{} edited", c.conn.name);
            c.st.update_connection(&c.conn.id, i.clone(), None).await.unwrap();
            assert!(
                status(c, "tab").await.is_some(),
                "{l}: a rename closed the session"
            );
            let pw = Some(common::rw_password());
            tokio::time::timeout(NO_HANG, c.st.update_connection(&c.conn.id, i, pw))
                .await
                .unwrap_or_else(|_| panic!("{l}: update hung on the live session"))
                .unwrap();
            assert_eq!(
                status(c, "tab").await,
                None,
                "{l}: session survived the edit"
            );
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(cell(&other, 0, 0), json!(0), "{l}: the edit committed");
        })
        .await;
        // Delete with a pending write in a session (on a table that is gone afterwards: the
        // scratch table is dropped through a second connection first).
        let t = common::scratch();
        run_other(&c, &format!("CREATE TABLE {t} (id int PRIMARY KEY)")).await;
        run_tab_ok(&c, &format!("INSERT INTO {t} VALUES (1)"), "tab").await;
        let other = prod_ctx(&c.label, c.conn.engine, c.conn.port);
        tokio::time::timeout(NO_HANG, c.st.delete_connection(&c.conn.id))
            .await
            .unwrap_or_else(|_| panic!("{l}: delete hung on the live session"))
            .unwrap();
        assert!(c.st.sessions().pending_tab_ids().is_empty(), "{l}");
        let r = run_other(&other, &format!("SELECT count(*) AS n FROM {t}")).await;
        assert_eq!(cell(&r, 0, 0), json!(0), "{l}: the delete committed");
        run_other(&other, &format!("DROP TABLE {t}")).await;
    }
}

/// A connection edit while a run holds the session: the run finishes, and the session it
/// hands back dies instead of returning as a ghost on the old settings.
#[tokio::test]
async fn conn_edit_during_a_run_leaves_no_ghost() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
        let (run, edit) = tokio::join!(run_tab(c, "SELECT pg_sleep(1.5)", "tab"), async {
            wait_pending(c, "tab").await;
            assert_eq!(
                c.st.sessions().pending_tab_ids(),
                vec!["tab".to_string()],
                "{l}: a running tab must count as pending"
            );
            let i = input(Engine::Pg, common::pg16(), Env::Production, Path::new(""));
            let pw = Some(common::rw_password());
            tokio::time::timeout(NO_HANG, c.st.update_connection(&c.conn.id, i, pw)).await
        });
        run.unwrap_or_else(|e| panic!("{l}: run failed: {e:?}"));
        edit.unwrap_or_else(|_| panic!("{l}: update hung")).unwrap();
        assert!(
            !c.st.sessions().has("tab"),
            "{l}: ghost session after the run"
        );
        let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
        assert_eq!(
            cell(&other, 0, 0),
            json!(0),
            "{l}: pending work survived the edit"
        );
    })
    .await;
}

/// `close_tab_session` while a run holds the session: no ghost after the run, the pending
/// work is rolled back, and the tab works again with a fresh session.
#[tokio::test]
async fn close_during_a_run_leaves_no_ghost() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    with_scratch(&c, async |c: &Ctx, t: &str| {
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (1, 1)"), "tab").await;
        let (run, ()) = tokio::join!(run_tab(c, "SELECT pg_sleep(1)", "tab"), async {
            wait_pending(c, "tab").await;
            c.st.sessions().close("tab").await;
        });
        run.unwrap_or_else(|e| panic!("{l}: run failed: {e:?}"));
        assert!(
            !c.st.sessions().has("tab"),
            "{l}: ghost session after the run"
        );
        let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
        assert_eq!(cell(&other, 0, 0), json!(0), "{l}: close did not roll back");
        run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;
        commit(c, "tab").await.unwrap();
        let other = run_other(c, &format!("SELECT id FROM {t}")).await;
        assert_eq!(cell(&other, 0, 0), json!(2), "{l}");
    })
    .await;
}

/// A failed COMMIT (deferred constraint) closes the session and frees the tab: the next run
/// works instead of reporting "in use by another run".
#[tokio::test]
async fn failed_commit_frees_the_tab() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let l = &c.label;
    let t = common::scratch();
    run_other(
        &c,
        &format!("CREATE TABLE {t} (id int UNIQUE DEFERRABLE INITIALLY DEFERRED)"),
    )
    .await;
    run_tab_ok(&c, &format!("INSERT INTO {t} VALUES (1), (1)"), "tab").await;
    let e = commit(&c, "tab").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Database, "{l}: {e:?}");
    assert!(
        !c.st.sessions().has("tab"),
        "{l}: stale slot after the failed commit"
    );
    run_tab_ok(&c, &format!("INSERT INTO {t} VALUES (2)"), "tab").await;
    commit(&c, "tab").await.unwrap();
    let other = run_other(&c, &format!("SELECT id FROM {t}")).await;
    assert_eq!(other.statements[0].row_count, 1, "{l}");
    assert_eq!(cell(&other, 0, 0), json!(2), "{l}");
    run_other(&c, &format!("DROP TABLE {t}")).await;
}

/// A transaction that ends mid-run (typed COMMIT) is reopened before the next write: that
/// write stays pending instead of autocommitting.
#[tokio::test]
async fn write_after_a_typed_commit_stays_pending() {
    for c in prod_all() {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_tab_ok(
                c,
                &format!("INSERT INTO {t} VALUES (1, 1); COMMIT; INSERT INTO {t} VALUES (2, 2)"),
                "tab",
            )
            .await;
            let s = status(c, "tab").await.unwrap();
            assert!(s.tx_open && s.pending, "{l}: {s:?}");
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(
                cell(&other, 0, 0),
                json!(1),
                "{l}: the second insert autocommitted"
            );
            rollback(c, "tab").await.unwrap();
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(cell(&other, 0, 0), json!(1), "{l}");
        })
        .await;
    }
}

/// MySQL `ALTER …; UPDATE …` in one run: the ALTER commits implicitly (sweeping the earlier
/// pending insert, which the notice says), and the UPDATE after it runs in a new transaction
/// instead of autocommitting.
#[tokio::test]
async fn mysql_write_after_implicit_commit_stays_pending() {
    for c in prod(Engine::Mysql, common::mysql())
        .into_iter()
        .chain(prod(Engine::Mysql, common::mariadb()))
    {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            run_other(c, &format!("INSERT INTO {t} VALUES (1, 1)")).await;
            run_tab_ok(c, &format!("INSERT INTO {t} VALUES (2, 2)"), "tab").await;
            let r = run_tab_ok(
                c,
                &format!("ALTER TABLE {t} ADD COLUMN w int; UPDATE {t} SET v = 99 WHERE id = 1"),
                "tab",
            )
            .await;
            assert!(
                r.notices
                    .iter()
                    .any(|n| n.contains("MySQL committed this DDL immediately")
                        && n.contains("including the uncommitted changes")
                        && n.contains("new transaction")),
                "{l}: notices {:?}",
                r.notices
            );
            let s = status(c, "tab").await.unwrap();
            assert!(
                s.tx_open && s.pending,
                "{l}: the UPDATE is not pending: {s:?}"
            );
            let other = run_other(c, &format!("SELECT v FROM {t} ORDER BY id")).await;
            assert_eq!(
                other.statements[0].row_count, 2,
                "{l}: the insert was swept"
            );
            assert_eq!(
                cell(&other, 0, 0),
                json!(1),
                "{l}: the UPDATE autocommitted"
            );
            rollback(c, "tab").await.unwrap();
            let other = run_other(c, &format!("SELECT v FROM {t} WHERE id = 1")).await;
            assert_eq!(cell(&other, 0, 0), json!(1), "{l}");
        })
        .await;
    }
}

/// Sessions do not hold pool connections: more tabs with open transactions than the pool's
/// size neither stall nor block a pooled read.
#[tokio::test]
async fn many_open_transactions_do_not_exhaust_the_pool() {
    for c in prod(Engine::Pg, common::pg16())
        .into_iter()
        .chain(prod(Engine::Mysql, common::mysql()))
    {
        let l = &c.label;
        with_scratch(&c, async |c: &Ctx, t: &str| {
            for i in 0..7 {
                let tab = format!("tab-{i}");
                tokio::time::timeout(
                    NO_HANG,
                    run_tab(c, &format!("INSERT INTO {t} VALUES ({i}, {i})"), &tab),
                )
                .await
                .unwrap_or_else(|_| panic!("{l}: tab {i} stalled on the pool"))
                .unwrap_or_else(|e| panic!("{l}: tab {i}: {e:?}"));
            }
            let other = run_other(c, &format!("SELECT count(*) AS n FROM {t}")).await;
            assert_eq!(cell(&other, 0, 0), json!(0), "{l}");
            for i in 0..7 {
                rollback(c, &format!("tab-{i}")).await.unwrap();
            }
        })
        .await;
    }
}

/// A Local connection on `engine`; the save runs in autocommit, no tab session.
fn local_ctx(label: &str, engine: Engine, port: u16) -> Ctx {
    let (dir, st) = common::state();
    let pw = common::rw_password();
    let conn = st.create_connection(input(engine, port, Env::Local, Path::new("")), Some(pw), )
    .unwrap();
    Ctx {
        _dir: dir,
        st,
        conn,
        label: label.into(),
    }
}

/// A direct PG connection that holds a row lock, so a save's statement waits for it.
async fn locker() -> sqlx::PgConnection {
    let user = common::rw_user();
    let pw = common::rw_password();
    let opts = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(common::pg16())
        .username(&user)
        .password(&pw)
        .database("shop");
    sqlx::PgConnection::connect_with(&opts)
        .await
        .expect("direct PG connection for the row lock")
}

/// A direct SQLite connection to the same file, for holding the write lock.
async fn sqlite_locker(c: &Ctx) -> sqlx::SqliteConnection {
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&c.conn.file);
    sqlx::SqliteConnection::connect_with(&opts)
        .await
        .expect("direct SQLite connection for the write lock")
}

/// One `v` update 'a' -> `new` on row 1, saved through the editor's id and tab.
fn save(c: &Ctx, table: &str, new: &str, id: Option<&str>, tab: Option<&str>) -> ApplyEditsRequest {
    ApplyEditsRequest {
        conn_id: c.conn.id.clone(),
        database: "shop".into(),
        sql: format!("SELECT id, v FROM {table}"),
        tab_id: tab.map(str::to_string),
        query_id: id.map(str::to_string),
        operations: vec![EditOperation {
            key: "0".into(),
            kind: EditKind::Update,
            pk: BTreeMap::from([("id".into(), json!(1))]),
            set: BTreeMap::from([("v".into(), json!(new))]),
            old: BTreeMap::from([("v".into(), TypedValue { ty: "string".into(), value: Some("a".into()) })]),
            document: None,
        }],
    }
}

#[tokio::test]
async fn a_cancelled_save_rolls_back_and_needs_no_reconnect() {
    let c = local_ctx("pg16", Engine::Pg, common::pg16());
    let t = common::scratch();
    run_other(
        &c,
        &format!("CREATE TABLE {t} (id int PRIMARY KEY, v text)"),
    )
    .await;
    run_other(&c, &format!("INSERT INTO {t} VALUES (1, 'a')")).await;

    let mut lock = locker().await;
    sqlx::query("BEGIN").execute(&mut lock).await.unwrap();
    sqlx::query(&format!("UPDATE {t} SET v = 'locked' WHERE id = 1"))
        .execute(&mut lock)
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let saving = ops::apply_edits(&c.st, save(&c, &t, "b", Some(&id), None));
    let cancelling = async { cancel_started(&c, &id).await };
    let (r, ()) = tokio::join!(saving, cancelling);
    let e = r.expect_err("the blocked save must be cancelled");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");

    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("a"),
        "a cancelled save leaves no change"
    );

    // The cancelled statement's connection was closed, not returned: the pool opens a new one.
    let ok = ops::apply_edits(&c.st, save(&c, &t, "b", None, None))
        .await
        .expect("the next save works");
    assert!(ok.failed.is_none(), "{ok:?}");
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("b")
    );
    run_other(&c, &format!("DROP TABLE {t}")).await;
}

#[tokio::test]
async fn a_cancelled_save_keeps_the_tab_transaction() {
    let c = prod_ctx("pg16", Engine::Pg, common::pg16());
    let t = common::scratch();
    run_other(
        &c,
        &format!("CREATE TABLE {t} (id int PRIMARY KEY, v text)"),
    )
    .await;
    run_other(&c, &format!("INSERT INTO {t} VALUES (1, 'a')")).await;

    let mut lock = locker().await;
    sqlx::query("BEGIN").execute(&mut lock).await.unwrap();
    sqlx::query(&format!("UPDATE {t} SET v = 'locked' WHERE id = 1"))
        .execute(&mut lock)
        .await
        .unwrap();

    let tab = common::scratch();
    // The user has uncommitted work in the tab: the cancelled save must not lose it.
    run_tab_ok(&c, &format!("INSERT INTO {t} VALUES (2, 'tab')"), &tab).await;
    let id = uuid::Uuid::new_v4().to_string();
    let saving = ops::apply_edits(&c.st, save(&c, &t, "b", Some(&id), Some(&tab)));
    let cancelling = async { cancel_started(&c, &id).await };
    let (r, ()) = tokio::join!(saving, cancelling);
    let e = r.expect_err("the blocked save must be cancelled");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");

    // The savepoint rolled back; the user's transaction and its insert stay open and usable.
    let st = status(&c, &tab).await.expect("the tab session");
    assert!(st.open && st.tx_open && st.pending && !st.aborted, "{st:?}");
    let inside = run_tab_ok(&c, &format!("SELECT v FROM {t} WHERE id = 2"), &tab).await;
    assert_eq!(
        common::cell(&inside, 0, 0),
        json!("tab"),
        "the tab's own write survives"
    );

    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("a"),
        "a cancelled save leaves no change"
    );

    let ok = ops::apply_edits(&c.st, save(&c, &t, "c", None, Some(&tab)))
        .await
        .expect("the next save in the tab works");
    assert!(ok.failed.is_none(), "{ok:?}");
    commit(&c, &tab).await.expect("commit the tab");
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("c")
    );
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 2")).await, 0, 0),
        json!("tab")
    );
    run_other(&c, &format!("DROP TABLE {t}")).await;
}

#[tokio::test]
async fn a_cancelled_save_keeps_a_sqlite_tab_transaction() {
    let c = prod_ctx("sqlite", Engine::Sqlite, 0);
    let t = common::scratch();
    run_other(
        &c,
        &format!("CREATE TABLE {t} (id int PRIMARY KEY, v text)"),
    )
    .await;
    run_other(&c, &format!("INSERT INTO {t} VALUES (1, 'a')")).await;

    // The tab's own write goes to a TEMP table: it keeps the transaction non-empty without
    // holding the main database's write lock.
    let work = common::scratch();
    let tab = common::scratch();
    run_tab_ok(&c, &format!("CREATE TEMP TABLE {work} (x int)"), &tab).await;
    run_tab_ok(&c, &format!("INSERT INTO {work} VALUES (1)"), &tab).await;

    // Another connection holds the main database's write lock, so the save's UPDATE waits out
    // SQLite's busy timeout.
    let mut lock = sqlite_locker(&c).await;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut lock)
        .await
        .unwrap();
    sqlx::query(&format!("UPDATE {t} SET v = 'locked' WHERE id = 1"))
        .execute(&mut lock)
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let saving = ops::apply_edits(&c.st, save(&c, &t, "b", Some(&id), Some(&tab)));
    let cancelling = async { cancel_started(&c, &id).await };
    let (r, ()) = tokio::join!(saving, cancelling);
    let e = r.expect_err("the blocked save must be cancelled");
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");

    // Attaching the session would have let `sqlite3_interrupt` roll the whole transaction back.
    let st = status(&c, &tab).await.expect("the tab session");
    assert!(st.open && st.tx_open && st.pending && !st.aborted, "{st:?}");
    let inside = run_tab_ok(&c, &format!("SELECT count(*) FROM {work}"), &tab).await;
    assert_eq!(common::cell(&inside, 0, 0), json!(1), "the tab's own write survives");

    sqlx::query("ROLLBACK").execute(&mut lock).await.unwrap();
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("a"),
        "a cancelled save leaves no change"
    );
    ops::apply_edits(&c.st, save(&c, &t, "c", None, Some(&tab)))
        .await
        .expect("the next save in the tab works");
    commit(&c, &tab).await.expect("commit the tab");
    assert_eq!(
        common::cell(&run_other(&c, &format!("SELECT v FROM {t} WHERE id = 1")).await, 0, 0),
        json!("c")
    );
    run_other(&c, &format!("DROP TABLE {t}")).await;
}
