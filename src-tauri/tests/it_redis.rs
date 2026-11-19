//! Connection tests on the Redis services in the isolated sandb-test stack.
#![cfg(feature = "it")]

mod common;

use sandb_lib::core::connections::RedisMode;
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::redis::client;
use sandb_lib::core::secrets::password_account;

#[tokio::test]
async fn test_connection_reports_version_for_each_deployment() {
    let (_dir, state) = common::state();
    for (mode, port, user, password) in [
        (
            RedisMode::Standalone,
            common::redis(),
            common::ro_user(),
            common::ro_password(),
        ),
        (
            RedisMode::Cluster,
            common::redis_cluster(),
            common::ro_user(),
            common::ro_password(),
        ),
        (
            RedisMode::Sentinel,
            common::redis_sentinel(),
            "default".into(),
            common::env("REDIS_ROOT_PASSWORD"),
        ),
    ] {
        let input = common::redis_input("connection", port, &user, mode, true);
        let test = state
            .test_connection(input, Some(password), None)
            .await
            .unwrap_or_else(|err| panic!("{mode:?}: {err:?}"));
        assert!(
            test.server_version.starts_with("Redis 7.4."),
            "{mode:?}: {test:?}"
        );
        assert_eq!(test.topology, None, "{mode:?}");
    }
}

#[tokio::test]
async fn test_connection_reports_auth_and_deployment_errors() {
    let (_dir, state) = common::state();
    let user = common::ro_user();
    let input = common::redis_input(
        "wrong-password",
        common::redis(),
        &user,
        RedisMode::Standalone,
        true,
    );
    let error = state
        .test_connection(input, Some("wrong".into()), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AuthFailed);
    assert_eq!(
        error.message,
        "Authentication failed. Check the user and password."
    );

    let input = common::redis_input(
        "wrong-mode",
        common::redis_cluster(),
        &user,
        RedisMode::Standalone,
        true,
    );
    let error = state
        .test_connection(input, Some(common::ro_password()), None)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "This server runs in cluster mode. Choose Cluster."
    );

    let mut input = common::redis_input(
        "wrong-master",
        common::redis_sentinel(),
        "default",
        RedisMode::Sentinel,
        true,
    );
    input.redis.as_mut().unwrap().sentinel_master = "not-mymaster".into();
    let error = state
        .test_connection(input, Some(common::env("REDIS_ROOT_PASSWORD")), None)
        .await
        .unwrap_err();
    assert_eq!(error.message, "The sentinels do not know this master name");
}

#[tokio::test]
async fn uri_connection_can_be_edited_duplicated_and_deleted_without_exposing_password() {
    let (dir, state) = common::state();
    let user = common::ro_user();
    let mut input = common::redis_input("uri", common::redis(), &user, RedisMode::Standalone, true);
    let port = common::redis();
    let pw = common::ro_password();
    input.uri = format!("redis://{user}:{pw}@127.0.0.1:{port}/1");
    let conn = state.create_connection(input.clone(), None).unwrap();
    assert_eq!(conn.database, "1");
    assert_eq!(conn.uri, format!("redis://{user}@127.0.0.1:{port}/1"));
    assert_eq!(
        state.secrets().get(&password_account(&conn.id)).unwrap(),
        Some(pw.clone())
    );
    let stored = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(
        !stored.contains(&pw),
        "the connection file contains the password"
    );

    input.name = "renamed".into();
    input.uri = conn.uri.clone();
    let edited = state
        .update_connection(&conn.id, input.clone(), None)
        .await
        .unwrap();
    assert_eq!(edited.name, "renamed");
    let copied = state
        .duplicate_connection(&edited.id, Some(input.clone()), None)
        .unwrap();
    state.delete_connection(&conn.id).await.unwrap();
    assert_eq!(
        state.secrets().get(&password_account(&conn.id)).unwrap(),
        None
    );
    let test = state
        .test_connection(input, None, Some(&copied.id))
        .await
        .unwrap();
    assert!(test.server_version.starts_with("Redis 7.4."));
    state.delete_connection(&copied.id).await.unwrap();
    assert!(state.list_connections().is_empty());
}

#[tokio::test]
async fn changing_the_password_drops_existing_redis_sessions() {
    let (_dir, state) = common::state();
    let input = common::redis_input(
        "cache",
        common::redis(),
        &common::ro_user(),
        RedisMode::Standalone,
        true,
    );
    let conn = state
        .create_connection(input.clone(), Some(common::ro_password()))
        .unwrap();
    for db in ["0", "1"] {
        client::connection(state.as_ref(), &conn.id, db)
            .await
            .unwrap();
    }

    state
        .update_connection(&conn.id, input, Some("wrong".into()))
        .await
        .unwrap();
    for db in ["0", "1"] {
        let Err(error) = client::connection(state.as_ref(), &conn.id, db).await else {
            panic!("the old session for database {db} survived a password change");
        };
        assert_eq!(
            error.code,
            ErrorCode::AuthFailed,
            "database {db}: {error:?}"
        );
    }
}
