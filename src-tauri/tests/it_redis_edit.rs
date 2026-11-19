//! Grid saves against the isolated Redis fixtures in testdb/.
#![cfg(feature = "it")]
mod common;

use sandb_lib::core::connections::RedisMode;
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::ops;
use sandb_lib::core::result::{
    ApplyEditsRequest, ApplyEditsResult, CellValueRequest, EditOperation, EditableRequest, QueryRun,
};
use serde_json::{Value, json};

fn connect(port: u16, mode: RedisMode, read_only: bool) -> common::Ctx {
    common::ctx(
        "redis-edit",
        common::redis_input("grid", port, &common::rw_user(), mode, read_only),
        Some(common::rw_password()),
    )
}

async fn run(c: &common::Ctx, text: &str) -> QueryRun {
    ops::run_query(&c.st, common::req(&c.conn, text))
        .await
        .unwrap()
}

fn old(value: &str) -> Value {
    json!({"type": "string", "value": value})
}

fn op(kind: &str, pk: Value, set: Value, old: Value) -> EditOperation {
    serde_json::from_value(json!({"key": "row", "kind": kind, "pk": pk, "set": set, "old": old}))
        .unwrap()
}

fn request(c: &common::Ctx, sql: &str, operations: Vec<EditOperation>) -> ApplyEditsRequest {
    ApplyEditsRequest {
        conn_id: c.conn.id.clone(),
        database: "0".into(),
        sql: sql.into(),
        tab_id: None,
        query_id: Some(uuid::Uuid::new_v4().to_string()),
        operations,
    }
}

async fn save(c: &common::Ctx, sql: &str, operations: Vec<EditOperation>) -> ApplyEditsResult {
    ops::apply_edits(&c.st, request(c, sql, operations))
        .await
        .unwrap()
}

fn assert_saved(result: ApplyEditsResult) {
    assert!(result.failed.is_none() && !result.partial, "{result:?}");
}

#[tokio::test]
async fn a_hash_conflict_keeps_other_fields_unchanged() {
    let c = connect(common::redis(), RedisMode::Standalone, false);
    let other = connect(common::redis(), RedisMode::Standalone, false);
    let k = common::scratch();
    let sql = format!("HGETALL {k}");
    run(
        &c,
        &format!("HSET {k} name Amy city Paris retired yes\nPEXPIRE {k} 60000"),
    )
    .await;
    let edit = ops::editable_result(
        &c.st,
        EditableRequest {
            conn_id: c.conn.id.clone(),
            database: "0".into(),
            sql: sql.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(edit.pk, ["field"]);
    assert_eq!(edit.table.as_deref(), Some(k.as_str()));
    run(&other, &format!("HSET {k} name Bea")).await;
    let city = || {
        op(
            "update",
            json!({"field":"city"}),
            json!({"value":"Rome"}),
            json!({"value":old("Paris")}),
        )
    };
    let out = save(
        &c,
        &sql,
        vec![
            city(),
            op(
                "update",
                json!({"field":"name"}),
                json!({"value":"Cam"}),
                json!({"value":old("Amy")}),
            ),
        ],
    )
    .await;
    assert_eq!(out.failed.unwrap().index, 1);
    assert!(!out.partial);
    let reply = run(&c, &format!("HMGET {k} city name")).await;
    assert_eq!(common::cell_named(&reply, "value", 0), json!("Paris"));
    assert_eq!(common::cell_named(&reply, "value", 1), json!("Bea"));
    assert_saved(
        save(
            &c,
            &sql,
            vec![
                city(),
                op(
                    "update",
                    json!({"field":"name"}),
                    json!({"field":"nickname"}),
                    json!({"field":old("name"),"value":old("Bea")}),
                ),
                op(
                    "insert",
                    json!({}),
                    json!({"field":"color","value":"blue"}),
                    json!({}),
                ),
                op(
                    "delete",
                    json!({"field":"retired"}),
                    json!({}),
                    json!({"field":old("retired"),"value":old("yes")}),
                ),
            ],
        )
        .await,
    );
    let reply = run(&c, &format!("HMGET {k} city nickname color name retired")).await;
    let values: Vec<_> = (0..5)
        .map(|i| common::cell_named(&reply, "value", i))
        .collect();
    assert_eq!(
        values,
        [
            json!("Rome"),
            json!("Bea"),
            json!("blue"),
            Value::Null,
            Value::Null
        ]
    );
}

#[tokio::test]
async fn a_list_save_deletes_indexes_not_equal_values() {
    let c = connect(common::redis(), RedisMode::Standalone, false);
    let k = common::scratch();
    let sql = format!("LRANGE {k} 0 -1");
    run(
        &c,
        &format!("RPUSH {k} same middle same divider same\nPEXPIRE {k} 60000"),
    )
    .await;
    let del = |index: i32| {
        op(
            "delete",
            json!({"index":index}),
            json!({}),
            json!({"index":old(&index.to_string()),"value":old("same")}),
        )
    };
    let update = op(
        "update",
        json!({"index":1}),
        json!({"value":"updated"}),
        json!({"value":old("middle")}),
    );
    let insert = op("insert", json!({}), json!({"value":"last"}), json!({}));
    assert_saved(save(&c, &sql, vec![del(0), update, del(4), insert]).await);
    let reply = run(&c, &sql).await;
    assert_eq!(
        reply.statements[0].rows,
        [
            vec![json!(0), json!("updated")],
            vec![json!(1), json!("same")],
            vec![json!(2), json!("divider")],
            vec![json!(3), json!("last")]
        ]
    );
}

#[tokio::test]
async fn string_saves_keep_ttl_and_refuse_read_only_connections() {
    let c = connect(common::redis(), RedisMode::Standalone, false);
    let ro = connect(common::redis(), RedisMode::Standalone, true);
    let k = common::scratch();
    let sql = format!("GET {k}");
    let update = || {
        op(
            "update",
            json!({"key":k}),
            json!({"value":"new"}),
            json!({"value":old("original")}),
        )
    };
    run(&c, &format!("SET {k} original PX 60000")).await;
    let err = ops::apply_edits(&ro.st, request(&ro, &sql, vec![update()]))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ConnectionReadOnly);
    assert_saved(save(&c, &sql, vec![update()]).await);
    assert_eq!(common::cell(&run(&c, &sql).await, 0, 1), json!("new"));
    let ttl = common::cell(&run(&c, &format!("PTTL {k}")).await, 0, 0)
        .as_i64()
        .unwrap();
    assert!((1..=60000).contains(&ttl), "PTTL = {ttl}");
}

#[tokio::test]
async fn a_full_cell_read_keeps_binary_bytes() {
    let c = connect(common::redis(), RedisMode::Standalone, true);
    let value = ops::cell_value(
        &c.st,
        CellValueRequest {
            conn_id: c.conn.id.clone(),
            database: "0".into(),
            sql: "GET bin:1".into(),
            tab_id: None,
            pk: [("key".into(), json!("bin:1"))].into(),
            column: "value".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(value.as_deref(), Some(r#""\xff\x00\xfe""#));
}

#[tokio::test]
async fn cluster_saves_set_members_and_sorted_set_scores() {
    let c = connect(common::redis_cluster(), RedisMode::Cluster, false);
    let k = common::scratch();
    let set = format!("{k}:set");
    let ordered = format!("{k}:ordered");
    run(
        &c,
        &format!(
            "SADD {set} first second\nPEXPIRE {set} 60000\nZADD {ordered} 0.1 one 2 two\nPEXPIRE {ordered} 60000"
        ),
    )
    .await;
    let sql = format!("SMEMBERS {set}");
    assert_saved(
        save(
            &c,
            &sql,
            vec![
                op(
                    "update",
                    json!({"member":"first"}),
                    json!({"member":"renamed"}),
                    json!({"member":old("first")}),
                ),
                op("insert", json!({}), json!({"member":"extra"}), json!({})),
                op(
                    "delete",
                    json!({"member":"second"}),
                    json!({}),
                    json!({"member":old("second")}),
                ),
            ],
        )
        .await,
    );
    let result = run(&c, &sql).await;
    let mut rows = result.statements.into_iter().next().unwrap().rows;
    rows.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    assert_eq!(rows, [vec![json!("extra")], vec![json!("renamed")]]);

    let sql = format!("ZRANGE {ordered} 0 -1 WITHSCORES");
    assert_saved(
        save(
            &c,
            &sql,
            vec![
                op(
                    "update",
                    json!({"member":"one"}),
                    json!({"score":"4.5"}),
                    json!({"score":old("0.1")}),
                ),
                op(
                    "insert",
                    json!({}),
                    json!({"member":"less","score":"-1"}),
                    json!({}),
                ),
                op(
                    "delete",
                    json!({"member":"two"}),
                    json!({}),
                    json!({"member":old("two"),"score":old("2")}),
                ),
            ],
        )
        .await,
    );
    let result = run(&c, &sql).await;
    assert_eq!(
        result.statements[0].rows,
        [
            vec![json!("less"), json!(-1.0)],
            vec![json!("one"), json!(4.5)]
        ]
    );
}

#[tokio::test]
async fn a_denied_hash_removal_does_not_save_other_fields() {
    let c = connect(common::redis(), RedisMode::Standalone, false);
    let key = common::scratch();
    let user = common::scratch();
    let password = common::rw_password();
    run(
        &c,
        &format!(
            "HSET {key} stay before remove present\nPEXPIRE {key} 60000\nACL SETUSER {user} on >{password} ~{key} +@connection +type +hget +hexists +hset +eval"
        ),
    )
    .await;
    let restricted = common::ctx(
        "redis-edit-acl",
        common::redis_input("acl", common::redis(), &user, RedisMode::Standalone, false),
        Some(password),
    );
    let sql = format!("HGETALL {key}");
    let out = ops::apply_edits(
        &restricted.st,
        request(
            &restricted,
            &sql,
            vec![
                op(
                    "update",
                    json!({"field":"stay"}),
                    json!({"value":"after"}),
                    json!({"value":old("before")}),
                ),
                op(
                    "delete",
                    json!({"field":"remove"}),
                    json!({}),
                    json!({"field":old("remove"),"value":old("present")}),
                ),
            ],
        ),
    )
    .await;
    let values = run(&c, &format!("HMGET {key} stay remove")).await;
    run(&c, &format!("ACL DELUSER {user}")).await;
    let err = out.unwrap_err();
    assert_eq!(err.server_code.unwrap().code, "NOPERM");
    assert_eq!(common::cell_named(&values, "value", 0), json!("before"));
    assert_eq!(common::cell_named(&values, "value", 1), json!("present"));
}
