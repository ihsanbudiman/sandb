//! Key browsing on the isolated Redis services in testdb/.
#![cfg(feature = "it")]
mod common;

use sandb_lib::core::connections::{Connection, RedisMode};
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::redis::client;
use sandb_lib::core::state::AppState;
use sandb_lib::core::{ops, result::TableInfo};

fn connect(state: &AppState, mode: RedisMode, port: u16, write: bool) -> Connection {
    let (user, password) = if write {
        (common::rw_user(), common::rw_password())
    } else {
        (common::ro_user(), common::ro_password())
    };
    state
        .create_connection(
            common::redis_input("browse", port, &user, mode, !write),
            Some(password),
        )
        .unwrap()
}

fn shape(key: &TableInfo) -> (Option<&str>, Option<i64>) {
    (key.redis_type.as_deref(), key.row_estimate)
}

#[tokio::test]
async fn standalone_browses_databases_and_key_types() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Standalone, common::redis(), false);
    let id = &conn.id;
    assert_eq!(ops::list_databases(&state, id).await.unwrap(), ["0", "1"]);
    let db1 = ops::list_tables(&state, id, "1", None).await.unwrap();
    assert_eq!(db1.len(), 1);
    assert_eq!(db1[0].name, "db1:only");
    assert_eq!(shape(&db1[0]), (Some("string"), Some(3)));

    let users = ops::list_keys(&state, id, "0", "user:*", None)
        .await
        .unwrap();
    assert_eq!(users.keys.len(), 1_000);
    assert!(users.truncated);
    assert!(users.keys.windows(2).all(|k| k[0].name < k[1].name));
    assert!(
        users
            .keys
            .iter()
            .all(|k| shape(k) == (Some("hash"), Some(3)))
    );

    let all = ops::list_keys(&state, id, "0", "*", Some(10_000))
        .await
        .unwrap();
    assert!(!all.truncated);
    for (name, ty, length) in [
        ("greeting", "string", 5),
        ("counter", "string", 2),
        ("bin:1", "string", 3),
        ("ttl:1", "string", 7),
        ("big:hash", "hash", 100_000),
        ("queue:jobs", "list", 50),
        ("tags", "set", 20),
        ("leaderboard", "zset", 1_000),
        ("events", "stream", 10_000),
        ("seed:done", "string", 1),
    ] {
        let key = all.keys.iter().find(|k| k.name == name).unwrap();
        assert_eq!(shape(key), (Some(ty), Some(length)), "{name}");
    }

    let cols = ops::list_columns(&state, id, "0", None, "user:1")
        .await
        .unwrap();
    assert_eq!((cols[0].name.as_str(), cols[0].is_pk), ("field", true));
    assert_eq!(cols[1].name, "value");
    let mut fields: Vec<_> = cols[2..].iter().map(|c| c.name.as_str()).collect();
    fields.sort_unstable();
    assert_eq!(fields, ["age", "email", "name"]);
    assert!(cols[2..].iter().all(|c| c.sample_size == Some(3)));

    let props = ops::table_properties(&state, id, "0", None, "ttl:1")
        .await
        .unwrap()
        .redis
        .unwrap();
    assert_eq!(props.key_type, "string");
    assert!(props.ttl_ms.is_some_and(|ttl| ttl > 0));
    assert!(props.memory_bytes.is_some_and(|bytes| bytes > 0));
    let snapshot = ops::schema_snapshot(&state, id, "0").await.unwrap();
    assert_eq!(snapshot.tables.len(), 1_000);
    assert!(snapshot.truncated);
    assert!(snapshot.tables.iter().all(|table| table.columns.is_empty()));
}

#[tokio::test]
async fn cluster_scan_crosses_all_primaries() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Cluster, common::redis_cluster(), false);
    let id = &conn.id;
    assert_eq!(ops::list_databases(&state, id).await.unwrap(), ["0"]);
    let result = ops::list_keys(&state, id, "0", "k:*", Some(10_000))
        .await
        .unwrap();
    assert!(!result.truncated);
    assert_eq!(result.keys.len(), 3_000);
    let by_name: std::collections::HashMap<_, _> = result
        .keys
        .iter()
        .map(|key| (key.name.as_str(), key))
        .collect();
    for n in 1_u32..=3_000 {
        let name = format!("k:{{{n}}}");
        let key = by_name.get(name.as_str()).unwrap();
        assert_eq!(
            shape(key),
            (Some("string"), Some(i64::from(n.ilog10() + 1)))
        );
    }
    let props = ops::table_properties(&state, id, "0", None, "user:{1}")
        .await
        .unwrap()
        .redis
        .unwrap();
    assert_eq!((props.key_type.as_str(), props.length), ("hash", Some(3)));
}

#[tokio::test]
async fn sentinel_browses_the_primary() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Sentinel, common::redis_sentinel(), false);
    let id = &conn.id;
    assert_eq!(ops::list_databases(&state, id).await.unwrap(), ["0"]);
    let result = ops::list_keys(&state, id, "0", "*", None).await.unwrap();
    assert!(!result.truncated);
    assert_eq!(result.keys.len(), 2);
    assert_eq!(result.keys[0].name, "greeting");
    assert_eq!(shape(&result.keys[0]), (Some("string"), Some(5)));
    assert_eq!(result.keys[1].name, "seed:done");
    assert_eq!(shape(&result.keys[1]), (Some("string"), Some(1)));
}

#[tokio::test]
async fn listed_binary_name_opens_the_same_key() {
    let (_dir, state) = common::state();
    let conn = connect(&state, RedisMode::Standalone, common::redis(), true);
    let id = &conn.id;
    let mut session = client::connection(state.as_ref(), id, "0").await.unwrap();
    let prefix = common::scratch();
    let mut bytes = prefix.as_bytes().to_vec();
    bytes.extend_from_slice(b":\xff\x00");
    session
        .query(redis::cmd("SET").arg(&*bytes).arg("test").arg("EX").arg(60))
        .await
        .unwrap();
    let result = ops::list_keys(&state, id, "0", &format!("{prefix}:*"), None).await;
    let name = format!(r#""{prefix}:\xff\x00""#);
    let props = ops::table_properties(&state, id, "0", None, &name).await;
    session.query(redis::cmd("DEL").arg(&*bytes)).await.unwrap();

    let keys = result.unwrap().keys;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, name);
    assert_eq!(props.unwrap().redis.unwrap().length, Some(4));
    let gone = ops::table_properties(&state, id, "0", None, &name)
        .await
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
}
