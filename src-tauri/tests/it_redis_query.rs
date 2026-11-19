//! Editor commands on the isolated Redis services in testdb/.
#![cfg(feature = "it")]
mod common;

use std::time::{Duration, Instant};

use sandb_lib::commands::dispatch;
use sandb_lib::core::connections::{Connection, RedisMode};
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::ops;
use sandb_lib::core::redis::client;
use sandb_lib::core::result::RunSort;
use sandb_lib::core::state::AppState;
use serde_json::json;

fn connect(state: &AppState, mode: RedisMode, port: u16, read_only: bool) -> Connection {
    let (user, password) = if read_only {
        (common::ro_user(), common::ro_password())
    } else {
        (common::rw_user(), common::rw_password())
    };
    state
        .create_connection(
            common::redis_input("editor", port, &user, mode, read_only),
            Some(password),
        )
        .unwrap()
}

#[tokio::test]
async fn scheduled_redis_settings_read_stays_out_of_chat() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Standalone, common::redis(), true);
    let request = json!({"connId": conn.id, "sql": "cOnFiG \"gEt\" databases"});
    let mut schedule = request.clone();
    schedule["maxRows"] = json!(1);

    let result = dispatch(&state, "run_scheduled_read", schedule.clone())
        .await
        .unwrap();
    assert_eq!(result["rows"], json!([["databases", "16"]]));

    let err = dispatch(&state, "run_ai_read", request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::GuardRejected);
    assert!(
        err.message
            .contains("CONFIG GET can expose private server details")
    );

    schedule["sql"] = json!("SET greeting changed");
    let err = dispatch(&state, "run_scheduled_read", schedule)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::GuardRejected);
}

#[tokio::test]
async fn standalone_editor_reads_guards_and_recovers_from_cancel() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Standalone, common::redis(), true);
    let r = ops::run_query(
        &state,
        common::req(
            &conn,
            "GET bin:1\nHGETALL user:1\nLRANGE queue:jobs 3 4\nSMEMBERS tags\nZRANGE leaderboard 0 1 WITHSCORES\nXRANGE events - + COUNT 1\nSELECT 1\nGET db1:only",
        ),
    )
    .await
    .unwrap();
    let expected: &[(&[&str], usize)] = &[
        (&["key", "value"], 1),
        (&["field", "value"], 3),
        (&["index", "value"], 2),
        (&["member"], 20),
        (&["member", "score"], 2),
        (&["id", "type", "amount"], 1),
        (&["key", "value"], 1),
    ];
    assert_eq!(r.statements.len(), expected.len());
    for (stmt, (columns, count)) in r.statements.iter().zip(expected) {
        assert_eq!(
            stmt.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            *columns
        );
        assert_eq!(stmt.rows.len(), *count);
        assert!(stmt.is_read);
    }
    assert_eq!(r.statements[0].rows[0][1], json!({"$bytes":"/wD+","len":3}));
    assert_eq!(r.statements[2].rows[0], [json!(3), json!("job:4")]);
    assert_eq!(r.statements[6].database.as_deref(), Some("1"));
    assert_eq!(common::cell(&r, 0, 1), json!("yes"));
    let r = ops::run_query(&state, common::req(&conn, "GET db1:only"))
        .await
        .unwrap();
    assert_eq!(common::cell(&r, 0, 1), json!(null));

    let mut req = common::req(&conn, "ZRANGE leaderboard 0 9 WITHSCORES");
    req.limit = Some(2);
    req.sort = Some(RunSort {
        column: "score".into(),
        column_index: 1,
        ascending: false,
    });
    let r = ops::run_query(&state, req).await.unwrap();
    assert!(r.statements[0].truncated);
    assert_eq!(
        r.statements[0].rows,
        [
            vec![json!("player:10"), json!(10.0)],
            vec![json!("player:9"), json!(9.0)],
        ]
    );

    let key = common::scratch();
    let write = format!("SET {key} 1 EX 60");
    let err = ops::run_query(&state, common::req(&conn, &write))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
    assert!(err.message.contains("SET is not a read"), "{err:?}");
    let err = ops::run_editor_unchecked(&state, common::req(&conn, &write))
        .await
        .unwrap_err();
    assert_eq!(err.server_code.unwrap().code, "NOPERM");
    let err = ops::run_query(&state, common::req(&conn, "PING\nMULTI"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
    assert!(err.message.contains("Use a Lua script"), "{err:?}");

    // Finish the standalone checks before the script holds its server. The server releases
    // itself after 12 seconds even if cancellation fails. Use DB 1 to exercise the kill path
    // without issuing SELECT while the server is busy.
    let conn = connect(&state, RedisMode::Standalone, common::redis(), false);
    let mut observer = client::connection(state.as_ref(), &conn.id, "0")
        .await
        .unwrap();
    let mut req = common::req(
        &conn,
        r#"EVAL "local start=tonumber(redis.call('TIME')[1]); repeat until tonumber(redis.call('TIME')[1])-start>=12; return 1" 0"#,
    );
    req.database = Some("1".into());
    let id = req.query_id.clone();
    let run = tokio::spawn({
        let state = state.clone();
        async move { ops::run_query(&state, req).await }
    });
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if observer
                .query(redis::cmd("PING").arg("observer"))
                .await
                .is_err_and(|err| err.code() == Some("BUSY"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the running script reaches Redis's busy threshold");
    let start = Instant::now();
    ops::cancel_query(&state, &id).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert_eq!(
        err.detail.as_deref(),
        Some("The command may have run on the server.")
    );
    let mut req = common::req(&conn, "PING");
    req.database = Some("1".into());
    let r = tokio::time::timeout(Duration::from_secs(2), ops::run_query(&state, req))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(common::cell(&r, 0, 0), json!("PONG"));
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn cluster_preserves_command_slots_and_sentinel_uses_primary() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Cluster, common::redis_cluster(), true);
    let r = ops::run_query(&state, common::req(&conn, "GET k:{1}"))
        .await
        .unwrap();
    assert_eq!(common::cell(&r, 0, 1), json!("1"));
    let err = ops::run_query(&state, common::req(&conn, "MGET k:{1} k:{2}"))
        .await
        .unwrap_err();
    assert_eq!(err.server_code.unwrap().code, "CROSSSLOT");
    let err = ops::run_query(&state, common::req(&conn, "SELECT 1"))
        .await
        .unwrap_err();
    assert_eq!(err.message, "Redis Cluster has only database 0");

    let conn = connect(&state, RedisMode::Sentinel, common::redis_sentinel(), true);
    let r = ops::run_query(&state, common::req(&conn, "GET greeting"))
        .await
        .unwrap();
    assert_eq!(common::cell(&r, 0, 1), json!("hello"));
}
