//! Phase 2 integration tests: introspection, query runs, value mapping, cancel, CSV export.
//! Needs the sandb-test docker stack (`pnpm db:up`). Unreachable databases FAIL; nothing is skipped.
#![cfg(feature = "it")]

mod common;

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use common::Ctx;
use sandb_lib::core::result::ExportRequest;
use sandb_lib::core::result::TableKind;
use sandb_lib::core::result::{QueryRun, RunRequest};
use sandb_lib::core::cell::PREVIEW_BYTES;
use sandb_lib::core::engine::Engine;
use sandb_lib::core::ops;
use sandb_lib::core::error::{AppError, ErrorCode};

/// The notice a run that ended with an open transaction returns: a fixed expectation, not a
/// production constant, so a wording change is visible here.
const OPEN_TX_NOTICE: &str =
    "Open transaction was rolled back. Runs are auto-commit; put BEGIN…COMMIT in one run.";

/// A saved connection as `sandb_rw` (owner; can create temp tables) on a network engine.
fn net(label: &str, engine: Engine, port: u16) -> Ctx {
    common::net_ctx(label, engine, port)
}

/// A saved connection on a temp copy of the SQLite fixture (the fixture is never opened for writing).
fn sqlite() -> Ctx {
    common::sqlite_ctx(false)
}

fn all() -> Vec<Ctx> {
    common::all_sql()
}

fn req(c: &Ctx, sql: &str, limit: Option<usize>) -> RunRequest {
    let mut r = common::req(&c.conn, sql);
    r.limit = limit;
    r
}

async fn run(c: &Ctx, sql: &str, limit: Option<usize>) -> QueryRun {
    ops::run_query(&c.st, req(c, sql, limit))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{} on port {}: `{sql}` failed (is `pnpm db:up` running?): {e:?}",
                c.label, c.conn.port
            )
        })
}

fn col(r: &QueryRun, name: &str, row: usize) -> Value {
    common::cell_named(r, name, row)
}

#[tokio::test]
async fn introspection_on_each_engine() {
    for c in all() {
        let l = &c.label;
        let dbs = ops::list_databases(&c.st, &c.conn.id)
            .await
            .unwrap_or_else(|e| {
                panic!("{l}: list_databases failed (is `pnpm db:up` running?): {e:?}")
            });
        let (db, schema) = match c.conn.engine {
            Engine::Pg => {
                assert!(
                    dbs.contains(&"shop".into()) && dbs.contains(&"shop_staging".into()),
                    "{l}: {dbs:?}"
                );
                assert!(
                    !dbs.iter().any(|d| d.starts_with("template")),
                    "{l}: {dbs:?}"
                );
                let schemas = ops::list_schemas(&c.st, &c.conn.id, "shop")
                    .await
                    .unwrap();
                assert_eq!(schemas, vec!["analytics", "public"], "{l}");
                ("shop", Some("public"))
            }
            Engine::Mysql => {
                assert!(
                    dbs.contains(&"shop".into()) && dbs.contains(&"analytics".into()),
                    "{l}: {dbs:?}"
                );
                for sys in ["mysql", "information_schema", "performance_schema", "sys"] {
                    assert!(!dbs.contains(&sys.into()), "{l}: system db {sys} listed");
                }
                assert!(
                    ops::list_schemas(&c.st, &c.conn.id, "shop")
                        .await
                        .unwrap()
                        .is_empty()
                );
                ("shop", None)
            }
            Engine::Sqlite => {
                assert_eq!(dbs, vec!["main"]);
                ("main", None)
            }
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let tables = ops::list_tables(&c.st, &c.conn.id, db, schema)
            .await
            .unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        for t in [
            "customers",
            "products",
            "orders",
            "order_items",
            "notes",
            "note_tags",
            "order_status_history",
            "type_zoo",
        ] {
            assert!(names.contains(&t), "{l}: {t} missing from {names:?}");
        }
        let view = tables
            .iter()
            .find(|t| t.name == "order_summary")
            .unwrap_or_else(|| panic!("{l}: no view"));
        assert_eq!(view.kind, TableKind::View, "{l}");

        // analytics: a PG schema, a MySQL database, plain tables in SQLite.
        let analytics = match c.conn.engine {
            Engine::Pg => ops::list_tables(&c.st, &c.conn.id, "shop", Some("analytics"))
                .await
                .unwrap(),
            Engine::Mysql => ops::list_tables(&c.st, &c.conn.id, "analytics", None)
                .await
                .unwrap(),
            Engine::Sqlite => tables.clone(),
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let an: Vec<&str> = analytics.iter().map(|t| t.name.as_str()).collect();
        assert!(
            an.contains(&"daily_revenue") && an.contains(&"events"),
            "{l}: {an:?}"
        );

        let nt = ops::list_columns(&c.st, &c.conn.id, db, schema, "note_tags")
            .await
            .unwrap();
        assert_eq!(nt.len(), 2, "{l}");
        assert!(
            nt.iter().all(|c| !c.is_pk),
            "{l}: note_tags must have no PK: {nt:?}"
        );

        let osh = ops::list_columns(&c.st, &c.conn.id, db, schema, "order_status_history")
            .await
            .unwrap();
        let pk: Vec<(&str, Option<u32>)> = osh
            .iter()
            .filter(|c| c.is_pk)
            .map(|c| (c.name.as_str(), c.pk_ordinal))
            .collect();
        assert_eq!(pk, vec![("order_id", Some(1)), ("seq", Some(2))], "{l}");
        assert!(osh.iter().all(|c| !c.nullable), "{l}: {osh:?}");

        let e = ops::list_columns(&c.st, &c.conn.id, db, schema, "no_such_table")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{l}");

        let snap = ops::schema_snapshot(&c.st, &c.conn.id, db)
            .await
            .unwrap();
        assert!(!snap.truncated, "{l}");
        let customers = snap.tables.iter().find(|t| t.name == "customers").unwrap();
        assert!(customers.columns.iter().any(|c| c.name == "email"), "{l}");
        if c.conn.engine == Engine::Pg {
            assert!(
                snap.tables
                    .iter()
                    .any(|t| t.schema.as_deref() == Some("analytics") && t.name == "events"),
                "{l}"
            );
        }
    }
}

// Wide tables early in the order use up the column cap; the later table names must still reach autocomplete.
#[tokio::test]
async fn snapshot_keeps_table_names_past_the_column_cap() {
    let c = sqlite();
    let cols = |n: usize| {
        (0..n)
            .map(|i| format!("c{i} int"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    for t in ["aaa_wide_1", "aaa_wide_2", "aaa_wide_3"] {
        run(&c, &format!("CREATE TABLE {t} ({})", cols(700)), None).await;
    }
    run(&c, "CREATE TABLE zzz_after_cap (id int)", None).await;

    let snap = ops::schema_snapshot(&c.st, &c.conn.id, "main")
        .await
        .unwrap();
    assert!(snap.truncated);
    let names: Vec<&str> = snap.tables.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"zzz_after_cap"), "{names:?}");
    assert!(names.contains(&"customers"), "{names:?}");
    let n: usize = snap.tables.iter().map(|t| t.columns.len()).sum();
    // The documented cap, fixed here: raising it in the implementation must fail this test.
    assert_eq!(n, 2000);
}

#[tokio::test]
async fn sqlite_rowid_tables() {
    let c = sqlite();
    let tables = ops::list_tables(&c.st, &c.conn.id, "main", None)
        .await
        .unwrap();
    let t = |n: &str| tables.iter().find(|t| t.name == n).unwrap().clone();
    assert_eq!(t("rowid_only").has_rowid, Some(true));
    assert_eq!(
        t("order_summary").has_rowid,
        Some(false),
        "views have no rowid"
    );
    let cols = ops::list_columns(&c.st, &c.conn.id, "main", None, "rowid_only")
        .await
        .unwrap();
    assert!(
        cols.iter().all(|c| !c.is_pk),
        "rowid_only has no declared PK"
    );
}

#[tokio::test]
async fn pg_databases_get_their_own_pool() {
    let c = net("pg16", Engine::Pg, common::pg16());
    let mut r = req(&c, "SELECT current_database() AS db", None);
    r.database = Some("shop_staging".into());
    let out = ops::run_query(&c.st, r).await.expect("query on shop_staging");
    assert_eq!(col(&out, "db", 0), json!("shop_staging"));
    let tables = ops::list_tables(&c.st, &c.conn.id, "shop_staging", Some("public"))
        .await
        .unwrap();
    assert!(tables.iter().any(|t| t.name == "orders"));
}

fn no_unsupported(label: &str, r: &QueryRun) {
    let s = r.statements.last().unwrap();
    assert_eq!(s.row_count, 3, "{label}: type_zoo has 3 rows");
    for (ri, row) in s.rows.iter().enumerate() {
        for (ci, v) in row.iter().enumerate() {
            if let Value::String(t) = v {
                assert!(
                    !t.starts_with("<unsupported"),
                    "{label}: row {ri} column {} = {t}",
                    s.columns[ci].name
                );
            }
        }
    }
    // The all-NULL row decodes to null in every column but id.
    assert!(
        s.rows[2][1..].iter().all(Value::is_null),
        "{label}: {:?}",
        s.rows[2]
    );
}

#[tokio::test]
async fn type_zoo_decodes_on_pg() {
    for (label, port) in [("pg16", common::pg16()), ("pg13", common::pg13())] {
        let c = net(label, Engine::Pg, port);
        let r = run(&c, "SELECT * FROM type_zoo ORDER BY id", None).await;
        no_unsupported(label, &r);
        assert_eq!(col(&r, "c_int", 1), json!(-2147483648_i64), "{label}");
        assert_eq!(col(&r, "c_bigint", 0), json!("9007199254740993"), "{label}");
        assert_eq!(col(&r, "c_numeric", 0), json!("1234.5678"), "{label}");
        assert_eq!(col(&r, "c_float", 1), json!(-1.5e300), "{label}");
        assert_eq!(col(&r, "c_bool", 0), json!(true), "{label}");
        assert_eq!(
            col(&r, "c_text", 1),
            json!("Unicode: café, 日本語, 🚀🎉"),
            "{label}"
        );
        assert_eq!(col(&r, "c_varchar", 1), json!("emoji 👍"), "{label}");
        assert_eq!(
            col(&r, "c_uuid", 0),
            json!("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            "{label}"
        );
        assert_eq!(
            col(&r, "c_json", 0),
            json!({"a": 1, "b": [1, 2]}),
            "{label}"
        );
        assert_eq!(
            col(&r, "c_jsonb", 0),
            json!({"nested": {"k": "v"}}),
            "{label}"
        );
        assert_eq!(col(&r, "c_date", 0), json!("2026-11-02"), "{label}");
        assert_eq!(
            col(&r, "c_timestamp", 1),
            json!("1999-12-31 23:59:59.999999"),
            "{label}"
        );
        assert!(
            col(&r, "c_timestamptz", 0)
                .as_str()
                .unwrap()
                .starts_with("2026-11-02"),
            "{label}"
        );
        assert_eq!(col(&r, "c_time", 0), json!("23:59:59"), "{label}");
        assert_eq!(col(&r, "c_interval", 0), json!("1 day 02:03:04"), "{label}");
        assert_eq!(
            col(&r, "c_bytea", 0),
            json!({"$bytes": "3q2+7w==", "len": 4}),
            "{label}"
        );
        assert_eq!(col(&r, "c_int_arr", 0), json!([1, 2, 3]), "{label}");
        assert_eq!(col(&r, "c_text_arr", 0), json!(["a", "b c"]), "{label}");
        assert_eq!(col(&r, "c_int_arr", 1), json!([]), "{label}");
        assert_eq!(col(&r, "c_enum", 0), json!("happy"), "{label}");
    }
}

#[tokio::test]
async fn type_zoo_decodes_on_mysql_and_mariadb() {
    for (label, port) in [("mysql", common::mysql()), ("mariadb", common::mariadb())] {
        let c = net(label, Engine::Mysql, port);
        let r = run(&c, "SELECT * FROM type_zoo ORDER BY id", None).await;
        no_unsupported(label, &r);
        assert_eq!(col(&r, "c_int", 1), json!(-2147483648_i64), "{label}");
        assert_eq!(
            col(&r, "c_bigint", 1),
            json!("-9223372036854775808"),
            "{label}"
        );
        assert_eq!(col(&r, "c_decimal", 0), json!("1234.5678"), "{label}");
        assert_eq!(col(&r, "c_double", 1), json!(-1.5e300), "{label}");
        assert_eq!(col(&r, "c_bool", 0), json!(true), "{label}");
        assert_eq!(col(&r, "c_bool", 1), json!(false), "{label}");
        assert_eq!(
            col(&r, "c_text", 1),
            json!("Unicode: café, 日本語, 🚀🎉"),
            "{label}"
        );
        assert_eq!(col(&r, "c_varchar", 1), json!("emoji 👍"), "{label}");
        assert_eq!(
            col(&r, "c_uuid", 0),
            json!("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            "{label}"
        );
        // MySQL has a JSON type; MariaDB stores JSON as LONGTEXT, so it stays text there.
        let j = col(&r, "c_json", 0);
        assert!(
            j == json!({"a": 1, "b": [1, 2]}) || j.as_str().is_some_and(|s| s.contains("\"a\"")),
            "{label}: {j}"
        );
        assert_eq!(col(&r, "c_date", 0), json!("2026-11-02"), "{label}");
        assert_eq!(
            col(&r, "c_datetime", 0),
            json!("2026-11-02 10:30:00.123456"),
            "{label}"
        );
        assert_eq!(col(&r, "c_time", 1), json!("-838:59:59"), "{label}");
        assert_eq!(
            col(&r, "c_blob", 0),
            json!({"$bytes": "3q2+7w==", "len": 4}),
            "{label}"
        );
        assert_eq!(
            col(&r, "c_varbinary", 0),
            json!({"$bytes": "AP8=", "len": 2}),
            "{label}"
        );
        assert_eq!(col(&r, "c_enum", 0), json!("happy"), "{label}");
        assert_eq!(col(&r, "c_set", 0), json!("red,blue"), "{label}");
    }
}

#[tokio::test]
async fn type_zoo_decodes_on_sqlite() {
    let c = sqlite();
    let r = run(&c, "SELECT * FROM type_zoo ORDER BY id", None).await;
    no_unsupported("sqlite", &r);
    assert_eq!(col(&r, "c_int", 0), json!(1));
    assert_eq!(col(&r, "c_bigint", 0), json!("9007199254740993"));
    assert_eq!(col(&r, "c_real", 1), json!(-1.5e300));
    assert_eq!(col(&r, "c_bool", 0), json!(true));
    assert_eq!(col(&r, "c_bool", 1), json!(false));
    assert_eq!(col(&r, "c_text", 1), json!("Unicode: café, 日本語, 🚀🎉"));
    assert_eq!(col(&r, "c_varchar", 1), json!("emoji 👍"));
    assert_eq!(col(&r, "c_json", 0), json!("{\"a\": 1, \"b\": [1, 2]}"));
    assert_eq!(
        col(&r, "c_blob", 0),
        json!({"$bytes": "3q2+7w==", "len": 4})
    );
}

#[tokio::test]
async fn paging_caps_rows_and_flags_truncation() {
    for c in all() {
        let sql = match c.conn.engine {
            Engine::Pg => "SELECT * FROM analytics.events ORDER BY id",
            Engine::Mysql => "SELECT * FROM analytics.events ORDER BY id",
            Engine::Sqlite => "SELECT * FROM events ORDER BY id",
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let r = run(&c, sql, None).await;
        let s = &r.statements[0];
        assert_eq!(
            (s.row_count, s.truncated),
            (200, true),
            "{}: default limit",
            c.label
        );
        assert_eq!(s.rows.len(), 200);
        let r = run(&c, sql, Some(200)).await;
        assert_eq!(
            (r.statements[0].row_count, r.statements[0].truncated),
            (200, true),
            "{}",
            c.label
        );
        let r = run(&c, sql, Some(20_000)).await;
        assert_eq!(
            (r.statements[0].row_count, r.statements[0].truncated),
            (10_000, false),
            "{}",
            c.label
        );
        // The pool still works after a run that stopped reading mid-result.
        let r = run(&c, "SELECT 1 AS one", None).await;
        assert_eq!(col(&r, "one", 0), json!(1), "{}", c.label);
    }
}

#[tokio::test]
async fn multi_statement_runs_on_one_connection() {
    for c in all() {
        let sql = match c.conn.engine {
            Engine::Pg => {
                "SET application_name = 'it_multi';\nCREATE TEMP TABLE it_scratch (id int, label text);\nINSERT INTO it_scratch VALUES (1, 'a'), (2, 'b');\nSELECT id, label, current_setting('application_name') AS app FROM it_scratch ORDER BY id"
            }
            Engine::Mysql => {
                "SET @it_v = 'it_multi';\nCREATE TEMPORARY TABLE it_scratch (id int, label text);\nINSERT INTO it_scratch VALUES (1, 'a'), (2, 'b');\nSELECT id, label, @it_v AS app FROM it_scratch ORDER BY id"
            }
            Engine::Sqlite => {
                "CREATE TEMP TABLE it_scratch (id int, label text);\nINSERT INTO it_scratch VALUES (1, 'a'), (2, 'b');\nSELECT id, label, 'it_multi' AS app FROM it_scratch ORDER BY id"
            }
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let r = run(&c, sql, None).await;
        let n = r.statements.len();
        assert_eq!(
            n,
            if c.conn.engine == Engine::Sqlite {
                3
            } else {
                4
            },
            "{}",
            c.label
        );
        let insert = &r.statements[n - 2];
        assert_eq!(insert.affected, Some(2), "{}: {insert:?}", c.label);
        assert!(insert.columns.is_empty());
        let last = &r.statements[n - 1];
        assert_eq!(last.row_count, 2, "{}", c.label);
        assert_eq!(col(&r, "label", 1), json!("b"), "{}", c.label);
        assert_eq!(col(&r, "app", 0), json!("it_multi"), "{}", c.label);
    }
}

#[tokio::test]
async fn empty_result_still_has_columns_and_errors_name_the_statement() {
    for c in all() {
        let r = run(&c, "SELECT id, name FROM customers WHERE 1 = 0", None).await;
        let s = &r.statements[0];
        assert_eq!(s.row_count, 0, "{}", c.label);
        let names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name"], "{}", c.label);
        assert_eq!(s.affected, None, "{}", c.label);

        let e = ops::run_query(&c.st, req(&c, "SELECT 1; SELECT * FROM no_such_table", None), )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Database, "{}: {e:?}", c.label);
        assert!(
            e.message.starts_with("Statement 2 of 2:"),
            "{}: {}",
            c.label,
            e.message
        );
    }
}

async fn expect_cancel(c: &Ctx, sql: &str) {
    let r = req(c, sql, None);
    let qid = r.query_id.clone();
    let st = c.st.clone();
    let task = tokio::spawn(async move { ops::run_query(&st, r).await });
    // Wait until the run is registered, then cancel it.
    let t0 = Instant::now();
    while !c.st.queries().is_running(&qid) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "{}: query never started",
            c.label
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ops::cancel_query(&c.st, &qid).await.expect("cancel");
    let res: Result<QueryRun, AppError> = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap_or_else(|_| panic!("{}: query did not stop within 10 s after cancel", c.label))
        .unwrap();
    let e = res.expect_err("a cancelled query must fail");
    assert_eq!(e.code, ErrorCode::Cancelled, "{}: {e:?}", c.label);
    assert!(!c.st.queries().is_running(&qid));
    // The pool is still usable afterwards.
    let r = run(c, "SELECT 1 AS one", None).await;
    assert_eq!(col(&r, "one", 0), json!(1));
}

#[tokio::test]
async fn cancel_stops_long_queries() {
    for c in all() {
        let sql = match c.conn.engine {
            Engine::Pg => "SELECT pg_sleep(30)",
            Engine::Mysql => "SELECT SLEEP(30)",
            Engine::Sqlite => {
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000000000) SELECT count(*) FROM n"
            }
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        expect_cancel(&c, sql).await;
    }
}

#[tokio::test]
async fn cancel_of_unknown_query_is_a_no_op() {
    let c = sqlite();
    ops::cancel_query(&c.st, "never-started").await.unwrap();
    assert!(
        ops::query_progress(&c.st, "never-started").is_none(),
        "an unknown id must not create progress"
    );
}

/// Minimal RFC 4180 reader for the round trip.
fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let mut rows = vec![];
    let mut row = vec![];
    let mut f = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quoted, ch) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                f.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => f.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut f)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut f));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => f.push(c),
        }
    }
    rows
}

#[tokio::test]
async fn csv_export_round_trips() {
    for c in all() {
        let tricky = "a,b \"q\"\nline2";
        let (events, lit) = match c.conn.engine {
            Engine::Pg => ("analytics.events", "'a,b \"q\"' || chr(10) || 'line2'"),
            // Not `CHAR(10 USING …)`: sqlparser cannot parse it, and SQL that does not parse is
            // never treated as a read, so it could not be exported.
            Engine::Mysql => ("analytics.events", "CONCAT('a,b \"q\"', '\\n', 'line2')"),
            Engine::Sqlite => ("events", "'a,b \"q\"' || char(10) || 'line2'"),
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let sql = format!(
            "SELECT id, name, {lit} AS tricky, NULL AS empty_col FROM {events} ORDER BY id"
        );
        let r = ops::export_csv(&c.st, ereq(&c, &sql), &path)
            .await
            .unwrap_or_else(|e| panic!("{}: export failed: {e:?}", c.label));
        assert_eq!(r.rows, 10_000, "{}: no row cap on export", c.label);
        assert!(!r.cancelled);
        let text = std::fs::read_to_string(&path).unwrap();
        let rows = parse_csv(&text);
        assert_eq!(rows.len(), 10_001, "{}: header + rows", c.label);
        assert_eq!(
            rows[0],
            vec!["id", "name", "tricky", "empty_col"],
            "{}",
            c.label
        );
        assert_eq!(rows[1][2], tricky, "{}", c.label);
        assert_eq!(rows[1][3], "", "{}: NULL is empty", c.label);

        let bytes_path = dir.path().join("zoo.csv");
        let blob = match c.conn.engine {
            Engine::Pg => "c_bytea",
            _ => "c_blob",
        };
        ops::export_csv(
            &c.st,
            ereq(&c, &format!("SELECT {blob} FROM type_zoo ORDER BY id")),
            &bytes_path,
        )
        .await
        .unwrap();
        let rows = parse_csv(&std::fs::read_to_string(&bytes_path).unwrap());
        assert_eq!(rows[1][0], "3q2+7w==", "{}: bytes as base64", c.label);

        // Only one plain read is exported, so an export never repeats a write.
        for sql in [
            "SELECT 1; SELECT 2",
            "DELETE FROM customers WHERE 1 = 0",
            "UPDATE customers SET name = name WHERE 1 = 0 RETURNING id",
        ] {
            let e = ops::export_csv(&c.st, ereq(&c, sql), &dir.path().join("x.csv"))
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{}: {sql}", c.label);
        }
        assert!(!dir.path().join("x.csv").exists());

        // A failed export leaves an existing file as it was and no temp file behind.
        std::fs::write(&path, b"keep me").unwrap();
        ops::export_csv(&c.st, ereq(&c, "SELECT * FROM no_such_table"), &path)
            .await
            .unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me", "{}", c.label);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.ends_with(".part")),
            "{}: {names:?}",
            c.label
        );
    }
}

/// Geometry as WKT runs to megabytes per cell; the grid gets a short preview so 200 rows stay
/// small, and the CSV export still writes the whole value.
#[tokio::test]
async fn large_text_is_cut_in_the_grid_but_not_in_the_export() {
    const REPEATS: usize = 50_000;
    let full_len = "POLYGON((".len() + "106.8 -6.2, ".len() * REPEATS + "))".len();
    for c in all() {
        let (events, wkt) = match c.conn.engine {
            Engine::Pg => (
                "analytics.events",
                format!("'POLYGON((' || repeat('106.8 -6.2, ', {REPEATS}) || '))'"),
            ),
            Engine::Mysql => (
                "analytics.events",
                format!("CONCAT('POLYGON((', REPEAT('106.8 -6.2, ', {REPEATS}), '))')"),
            ),
            // SQLite has no repeat(): hex(zeroblob(n)) is n pairs of "00".
            Engine::Sqlite => (
                "events",
                format!(
                    "'POLYGON((' || replace(hex(zeroblob({REPEATS})), '00', '106.8 -6.2, ') || '))'"
                ),
            ),
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        let l = &c.label;
        let r = run(
            &c,
            &format!("SELECT id, {wkt} AS wkt FROM {events} ORDER BY id"),
            None,
        )
        .await;
        assert_eq!(r.statements[0].row_count, 200, "{l}");
        for i in 0..200 {
            let v = col(&r, "wkt", i);
            let s = v
                .as_str()
                .unwrap_or_else(|| panic!("{l}: row {i} is not text: {v}"));
            assert!(s.starts_with("POLYGON((106.8 -6.2, "), "{l}: {}", &s[..30]);
            assert!(s.ends_with('…'), "{l}: row {i} not cut");
            assert!(
                s.len() <= PREVIEW_BYTES + '…'.len_utf8(),
                "{l}: {} bytes",
                s.len()
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wkt.csv");
        let sql = format!("SELECT {wkt} AS wkt FROM {events} ORDER BY id LIMIT 1");
        ops::export_csv(&c.st, ereq(&c, &sql), &path)
            .await
            .unwrap_or_else(|e| panic!("{l}: export failed: {e:?}"));
        let rows = parse_csv(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(
            rows[1][0].len(),
            full_len,
            "{l}: export keeps the full value"
        );
    }
}

fn ereq(c: &Ctx, sql: &str) -> ExportRequest {
    ExportRequest {
        conn_id: c.conn.id.clone(),
        database: None,
        sql: sql.into(),
        tab_id: None,
    }
}

async fn try_run(c: &Ctx, sql: &str, limit: Option<usize>) -> Result<QueryRun, AppError> {
    ops::run_query(&c.st, req(c, sql, limit)).await
}

fn session_ctxs() -> Vec<Ctx> {
    vec![
        net("pg16", Engine::Pg, common::pg16()),
        net("mysql", Engine::Mysql, common::mysql()),
        sqlite(),
    ]
}

#[tokio::test]
async fn open_transaction_does_not_leak_into_the_next_run() {
    for c in session_ctxs() {
        let l = &c.label;
        let t = common::scratch();
        run(
            &c,
            &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)"),
            None,
        )
        .await;
        run(&c, &format!("INSERT INTO {t} VALUES (1, 1)"), None).await;

        let begin = if c.conn.engine == Engine::Mysql {
            "START TRANSACTION"
        } else {
            "BEGIN"
        };
        let a = run(
            &c,
            &format!("{begin}; UPDATE {t} SET v = 2 WHERE id = 1"),
            None,
        )
        .await;
        assert_eq!(a.notices, vec![OPEN_TX_NOTICE.to_string()], "{l}");

        let b = run(&c, &format!("SELECT v FROM {t} WHERE id = 1"), None).await;
        assert_eq!(
            col(&b, "v", 0),
            json!(1),
            "{l}: run B saw run A's uncommitted write"
        );

        // The row lock of run A is gone: a write from another run does not wait for it.
        let w = tokio::time::timeout(
            Duration::from_secs(2),
            try_run(&c, &format!("UPDATE {t} SET v = 3 WHERE id = 1"), None),
        )
        .await
        .unwrap_or_else(|_| panic!("{l}: blocked by run A's lock"))
        .unwrap_or_else(|e| panic!("{l}: {e:?}"));
        assert_eq!(w.statements[0].affected, Some(1), "{l}");
        assert!(w.notices.is_empty(), "{l}");

        // A transaction that ends in the same run is fine and gives no notice.
        let ok = run(
            &c,
            &format!("{begin}; UPDATE {t} SET v = 4 WHERE id = 1; COMMIT"),
            None,
        )
        .await;
        assert!(ok.notices.is_empty(), "{l}");
        let b = run(&c, &format!("SELECT v FROM {t} WHERE id = 1"), None).await;
        assert_eq!(col(&b, "v", 0), json!(4), "{l}");
        run(&c, &format!("DROP TABLE {t}"), None).await;
    }
}

#[tokio::test]
async fn session_settings_do_not_leak_into_the_next_run() {
    for c in session_ctxs() {
        let l = &c.label;
        let (set, check, expect) = match c.conn.engine {
            Engine::Pg => (
                "SET search_path TO analytics",
                "SELECT current_schema() AS v",
                json!("public"),
            ),
            Engine::Mysql => ("USE analytics", "SELECT DATABASE() AS v", json!("shop")),
            Engine::Sqlite => (
                "CREATE TEMP TABLE it_leak (a int)",
                "SELECT count(*) AS v FROM sqlite_temp_master WHERE name = 'it_leak'",
                json!(0),
            ),
            Engine::Mongo => unreachable!("SQL engines only"),
        };
        run(&c, set, None).await;
        // Several runs, so a pooled connection would be picked up if one leaked.
        for _ in 0..6 {
            let r = run(&c, check, None).await;
            assert_eq!(col(&r, "v", 0), expect, "{l}: `{set}` leaked");
        }
    }
}

#[tokio::test]
async fn returning_over_the_limit_is_truncated_and_still_committed() {
    for c in session_ctxs() {
        let l = &c.label;
        let t = common::scratch();
        run(
            &c,
            &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)"),
            None,
        )
        .await;
        let fill = match c.conn.engine {
            Engine::Pg => format!("INSERT INTO {t} SELECT g, 0 FROM generate_series(1, 300) g"),
            _ => format!(
                "INSERT INTO {t} WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300) SELECT i, 0 FROM n"
            ),
        };
        run(&c, &fill, None).await;
        if c.conn.engine != Engine::Mysql {
            // MySQL has no UPDATE … RETURNING.
            let r = run(&c, &format!("UPDATE {t} SET v = 1 RETURNING id"), Some(100)).await;
            let s = &r.statements[0];
            assert_eq!(
                (s.row_count, s.truncated, s.is_read),
                (100, true, false),
                "{l}"
            );
            let n = run(
                &c,
                &format!("SELECT count(*) AS n FROM {t} WHERE v = 1"),
                None,
            )
            .await;
            assert_eq!(col(&n, "n", 0).to_string().trim_matches('"'), "300", "{l}");
        }
        let r = run(&c, &format!("SELECT * FROM {t} ORDER BY id"), Some(100)).await;
        let s = &r.statements[0];
        assert_eq!(
            (s.row_count, s.truncated, s.is_read),
            (100, true, true),
            "{l}"
        );
        run(&c, &format!("DROP TABLE {t}"), None).await;
    }
}

#[tokio::test]
async fn unsplittable_sql_returns_each_result_set() {
    let c = net("pg16", Engine::Pg, common::pg16());
    let s = common::scratch();
    run(&c, &format!("CREATE SCHEMA {s}"), None).await;
    // sqlparser has no DO statement, so this goes to the server as one piece of SQL.
    let sql = format!(
        "DO $do$ BEGIN EXECUTE 'CREATE FUNCTION {s}.f() RETURNS int AS $$ BEGIN PERFORM 1; RETURN 1; END; $$ LANGUAGE plpgsql'; END $do$; SELECT {s}.f() AS a; SELECT 'x' AS b, 'y' AS c"
    );
    let r = try_run(&c, &sql, None).await;
    run(&c, &format!("DROP SCHEMA {s} CASCADE"), None).await;
    let r = r.unwrap();
    let cols: Vec<Vec<&str>> = r
        .statements
        .iter()
        .map(|s| s.columns.iter().map(|c| c.name.as_str()).collect())
        .collect();
    assert_eq!(cols, vec![vec![], vec!["a"], vec!["b", "c"]]);
    assert_eq!(r.statements[1].rows, vec![vec![json!(1)]]);
    assert_eq!(r.statements[2].rows, vec![vec![json!("x"), json!("y")]]);
    assert!(r.statements.iter().all(|s| !s.is_read));
}

#[tokio::test]
async fn cancel_before_the_run_registers_is_not_lost() {
    let c = sqlite();
    let r = req(&c, "SELECT 1", None);
    ops::cancel_query(&c.st, &r.query_id).await.unwrap();
    let e = ops::run_query(&c.st, r).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Cancelled);
    assert!(!run(&c, "SELECT 1 AS one", None).await.statements.is_empty());
}
/// The server, not the SQL text, decides whether a run left a transaction open.
#[tokio::test]
async fn open_transaction_notice_comes_from_the_server() {
    // MySQL/MariaDB: DDL commits the open transaction, so nothing is rolled back.
    for (label, port) in [("mysql", common::mysql()), ("mariadb", common::mariadb())] {
        let c = net(label, Engine::Mysql, port);
        let t = common::scratch();
        run(
            &c,
            &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)"),
            None,
        )
        .await;
        run(&c, &format!("INSERT INTO {t} VALUES (1, 1)"), None).await;
        let a = run(
            &c,
            &format!(
                "START TRANSACTION; UPDATE {t} SET v = 2 WHERE id = 1; CREATE TABLE {t}_x (a int)"
            ),
            None,
        )
        .await;
        let b = run(&c, &format!("SELECT v FROM {t} WHERE id = 1"), None).await;
        run(&c, &format!("DROP TABLE {t}, {t}_x"), None).await;
        assert!(a.notices.is_empty(), "{label}: {:?}", a.notices);
        assert_eq!(
            col(&b, "v", 0),
            json!(2),
            "{label}: the UPDATE was committed"
        );
    }

    // PG: an open transaction the text cannot show (DO is not parsable), and the plain case.
    let c = net("pg16", Engine::Pg, common::pg16());
    let t = common::scratch();
    run(
        &c,
        &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)"),
        None,
    )
    .await;
    run(&c, &format!("INSERT INTO {t} VALUES (1, 1)"), None).await;
    for sql in [
        format!("BEGIN; UPDATE {t} SET v = 2 WHERE id = 1"),
        format!("BEGIN; DO $$ BEGIN UPDATE {t} SET v = 3 WHERE id = 1; END $$"),
    ] {
        let a = run(&c, &sql, None).await;
        assert_eq!(a.notices, vec![OPEN_TX_NOTICE.to_string()], "{sql}");
        let b = run(&c, &format!("SELECT v FROM {t} WHERE id = 1"), None).await;
        assert_eq!(col(&b, "v", 0), json!(1), "{sql}: rolled back");
    }
    // A failed statement inside the transaction leaves it open (aborted) too.
    let e = try_run(&c, &format!("BEGIN; UPDATE {t} SET v = 1 / 0"), None).await;
    assert!(e.is_err());
    let ok = run(&c, &format!("BEGIN; UPDATE {t} SET v = 5; COMMIT"), None).await;
    assert!(ok.notices.is_empty());
    run(&c, &format!("DROP TABLE {t}"), None).await;
}

/// An error after the row limit must still fail the whole UPDATE … RETURNING: the rows past
/// the limit are read to the end instead of being dropped with the stream.
#[tokio::test]
async fn returning_error_after_the_limit_fails_the_write() {
    let c = net("pg16", Engine::Pg, common::pg16());
    let t = common::scratch();
    run(
        &c,
        &format!("CREATE TABLE {t} (id int PRIMARY KEY, v int)"),
        None,
    )
    .await;
    run(
        &c,
        &format!("INSERT INTO {t} SELECT g, 0 FROM generate_series(1, 300) g"),
        None,
    )
    .await;
    let r = try_run(
        &c,
        &format!("UPDATE {t} SET v = 1 RETURNING 1 / (id - 250)"),
        Some(100),
    )
    .await;
    let n = run(
        &c,
        &format!("SELECT count(*) AS n FROM {t} WHERE v = 1"),
        None,
    )
    .await;
    run(&c, &format!("DROP TABLE {t}"), None).await;
    let e = r.expect_err("division by zero at id 250 must fail the run");
    assert!(e.message.contains("division by zero"), "{e:?}");
    assert_eq!(col(&n, "n", 0).to_string().trim_matches('"'), "0");
}

/// Functions with side effects make a SELECT "not a plain read" (no Load more, no export).
#[tokio::test]
async fn select_with_side_effects_is_not_a_plain_read() {
    let c = net("pg16", Engine::Pg, common::pg16());
    let s = common::scratch();
    run(
        &c,
        &format!("CREATE SCHEMA {s}; CREATE SEQUENCE {s}.seq"),
        None,
    )
    .await;
    let r = try_run(&c, &format!("SELECT nextval('{s}.seq') AS n"), None).await;
    let e = ops::export_csv(
        &c.st,
        ereq(&c, &format!("SELECT nextval('{s}.seq')")),
        &tempfile::tempdir().unwrap().path().join("x.csv"),
    )
    .await;
    run(&c, &format!("DROP SCHEMA {s} CASCADE"), None).await;
    assert!(!r.unwrap().statements[0].is_read);
    assert_eq!(e.unwrap_err().code, ErrorCode::InvalidInput);
    let r = run(&c, "SELECT * FROM orders", None).await;
    assert!(r.statements[0].is_read);
}

#[tokio::test]
async fn csv_export_of_an_empty_result_has_a_header() {
    for c in all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.csv");
        let r = ops::export_csv(
            &c.st,
            ereq(&c, "SELECT id, name FROM customers WHERE 1 = 0"),
            &path,
        )
        .await
        .unwrap_or_else(|e| panic!("{}: {e:?}", c.label));
        assert_eq!(r.rows, 0, "{}", c.label);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "id,name\r\n",
            "{}",
            c.label
        );
    }
}
