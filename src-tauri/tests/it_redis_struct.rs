//! Script and CSV exports against the isolated Redis fixture in testdb/.
#![cfg(feature = "it")]
mod common;

use sandb_lib::core::connections::RedisMode;
use sandb_lib::core::error::{AppError, ErrorCode};
use sandb_lib::core::ops;
use sandb_lib::core::redis::parse;
use sandb_lib::core::result::{ExportRequest, QueryRun, TableExportRequest};

fn setup() -> common::Ctx {
    common::ctx(
        "redis-export",
        common::redis_input(
            "export",
            common::redis(),
            &common::rw_user(),
            RedisMode::Standalone,
            false,
        ),
        Some(common::rw_password()),
    )
}

async fn run(ctx: &common::Ctx, text: &str) -> QueryRun {
    ops::run_query(&ctx.st, common::req(&ctx.conn, text))
        .await
        .unwrap()
}

async fn script(ctx: &common::Ctx, key: &str) -> Result<String, AppError> {
    ops::table_ddl(&ctx.st, &ctx.conn.id, "0", None, key).await
}

#[tokio::test]
async fn key_scripts_restore_values_order_binary_bytes_and_expiry() {
    let ctx = setup();
    for (source, command, options, unordered) in [
        ("user:1", "HGETALL", "", true),
        ("queue:jobs", "LRANGE", " 0 -1", false),
        ("tags", "SMEMBERS", "", true),
        ("leaderboard", "ZRANGE", " 0 30 WITHSCORES", false),
        ("events", "XRANGE", " - + COUNT 100", false),
        ("bin:1", "GET", "", false),
        ("ttl:1", "GET", "", false),
    ] {
        let dest = common::scratch();
        let content = script(&ctx, source).await.unwrap();
        let mut import = String::new();
        for stmt in parse::parse(&content).unwrap() {
            let (verb, tail) = stmt.text.split_once(' ').unwrap();
            let tail = tail.strip_prefix(source).unwrap();
            import.push_str(&format!("{verb} {dest}{tail}\n"));
        }
        run(&ctx, &import).await;
        let ttl = |key| format!("PTTL {key}");
        let before = run(&ctx, &ttl(source)).await;
        let after = run(&ctx, &ttl(&dest)).await;
        let before = common::cell_named(&before, "value", 0).as_i64().unwrap();
        let after = common::cell_named(&after, "value", 0).as_i64().unwrap();
        let mut original = run(&ctx, &format!("{command} {source}{options}")).await;
        let mut restored = run(&ctx, &format!("{command} {dest}{options}")).await;
        run(&ctx, &format!("UNLINK {dest}")).await;
        let original = &mut original.statements[0].rows;
        let restored = &mut restored.statements[0].rows;
        if command == "GET" {
            assert_eq!(original[0][1], restored[0][1], "{source}");
        } else {
            if unordered {
                original.sort_unstable_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
                restored.sort_unstable_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
            }
            assert_eq!(original, restored, "{source}");
        }
        if source == "ttl:1" {
            assert!(before > 0 && after > 0 && (after - before).abs() < 5_000);
        } else {
            assert_eq!((before, after), (-1, -1), "{source}");
        }
    }
}

#[tokio::test]
async fn large_hash_refuses_preview_but_exports_every_field_in_bounded_lines() {
    let ctx = setup();
    let error = script(&ctx, "big:hash").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("100000"));
    let path = ctx._dir.path().join("big.redis");
    let out = ops::export_table_sql(
        &ctx.st,
        TableExportRequest {
            conn_id: ctx.conn.id.clone(),
            database: "0".into(),
            schema: None,
            table: "big:hash".into(),
            data: false,
        },
        &path,
    )
    .await
    .unwrap();
    assert_eq!(out.rows, 100_000);
    let exported = std::fs::read_to_string(path).unwrap();
    let mut fields = vec![false; 100_000];
    let mut batches = 0;
    for stmt in parse::parse(&exported).unwrap() {
        if stmt.args[0] != b"HSET" {
            assert_eq!(stmt.args[0], b"DEL");
            continue;
        }
        assert_eq!(stmt.args[1], b"big:hash");
        assert_eq!(stmt.args.len(), 2_002);
        batches += 1;
        for [field, value] in stmt.args[2..].as_chunks::<2>().0 {
            let field = std::str::from_utf8(field).unwrap();
            let number = field.strip_prefix("field:").unwrap();
            assert_eq!(value, format!("value:{number}").as_bytes());
            fields[number.parse::<usize>().unwrap() - 1] = true;
        }
    }
    assert_eq!(batches, 100);
    assert!(fields.into_iter().all(|seen| seen));
}

#[tokio::test]
async fn csv_exports_all_fields_and_refuses_writes_before_replacing_a_file() {
    let ctx = setup();
    let path = ctx._dir.path().join("result.csv");
    let req = |text: &str| ExportRequest {
        conn_id: ctx.conn.id.clone(),
        database: None,
        sql: text.into(),
        tab_id: None,
    };
    let out = ops::export_csv(&ctx.st, req("HGETALL user:1"), &path)
        .await
        .unwrap();
    assert_eq!(out.rows, 3);
    let original = std::fs::read_to_string(&path).unwrap();
    let mut lines = original.split_terminator("\r\n");
    assert_eq!(lines.next(), Some("field,value"));
    let mut records: Vec<_> = lines.collect();
    records.sort_unstable();
    assert_eq!(
        records,
        ["age,21", "email,user1@example.test", "name,User 1"]
    );

    let dest = common::scratch();
    for input in [format!("SET {dest} no"), "GET greeting\nGET counter".into()] {
        assert!(ops::check_export(&ctx.st, &req(&input)).is_err());
        assert!(ops::export_csv(&ctx.st, req(&input), &path).await.is_err());
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    let exists = run(&ctx, &format!("EXISTS {dest}")).await;
    assert_eq!(common::cell_named(&exists, "value", 0), 0);
}
