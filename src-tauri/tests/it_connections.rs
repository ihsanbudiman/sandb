//! Integration tests against the sandb-test docker stack (`pnpm db:up`).
//! Run: `cargo test --features it`. An unreachable database FAILS the test; nothing is skipped.
#![cfg(feature = "it")]

mod common;

use sandb_lib::core::connections::ConnectionInput;
use sandb_lib::core::engine::Engine;
use sandb_lib::core::error::ErrorCode;

/// A `sandb_ro` login on a network SQL engine.
fn net(engine: Engine, port: u16) -> ConnectionInput {
    let mut input = common::sql_input("connections", engine, port, false);
    input.user = common::ro_user();
    input
}

async fn expect_ok(label: &str, input: ConnectionInput, prefix: &str) {
    let (_d, st) = common::state();
    let port = input.port;
    let r = st
        .test_connection(input, Some(common::ro_password()), None)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{label} on 127.0.0.1:{port} is not reachable or rejected the login \
                 (is `pnpm db:up` running?): {e:?}"
            )
        });
    assert!(
        r.server_version.starts_with(prefix),
        "{label}: unexpected version {:?}",
        r.server_version
    );
}

#[tokio::test]
async fn pg16_ok() {
    expect_ok("pg16", net(Engine::Pg, common::pg16()), "PostgreSQL 16.").await;
}

#[tokio::test]
async fn pg13_ok() {
    expect_ok("pg13", net(Engine::Pg, common::pg13()), "PostgreSQL 13.").await;
}

#[tokio::test]
async fn mysql_ok() {
    expect_ok("mysql", net(Engine::Mysql, common::mysql()), "MySQL 8.4").await;
}

#[tokio::test]
async fn mariadb_ok() {
    expect_ok("mariadb", net(Engine::Mysql, common::mariadb()), "MariaDB 11.").await;
}

#[tokio::test]
async fn sqlite_fixture_ok_and_unchanged() {
    let file = common::sqlite_fixture();
    let before = std::fs::read(&file).unwrap();
    let input = common::sqlite_input(&file, false);
    let (_d, st) = common::state();
    let r = st.test_connection(input, None, None)
        .await
        .expect("sqlite test");
    assert!(
        r.server_version.starts_with("SQLite 3."),
        "{}",
        r.server_version
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        before,
        "fixture must not change"
    );
    for side in ["shop.db-wal", "shop.db-shm", "shop.db-journal"] {
        assert!(
            !file.parent().unwrap().join(side).exists(),
            "{side} was created"
        );
    }
}

#[tokio::test]
async fn wrong_password_is_auth_failed_on_each_engine() {
    for n in common::sqlnet() {
        let (label, engine, port) = (n.label, n.engine, n.port);
        let (_d, st) = common::state();
        let e = st.test_connection(net(engine, port), Some("definitely-wrong".into()), None, )
        .await
        .expect_err(&format!("{label}: a wrong password must fail"));
        assert_eq!(e.code, ErrorCode::AuthFailed, "{label}: {e:?}");
    }
}

#[tokio::test]
async fn closed_port_is_connect_failed() {
    for engine in [Engine::Pg, Engine::Mysql] {
        let (_d, st) = common::state();
        let e = st.test_connection(net(engine, 1), Some("x".into()), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectFailed, "{engine:?}: {e:?}");
    }
}

#[tokio::test]
async fn saved_connection_tests_with_stored_password() {
    let (_d, st) = common::state();
    let pg = common::pg16();
    let c = st
        .create_connection(net(Engine::Pg, pg), Some(common::ro_password()))
        .unwrap();
    let r = st.test_connection(net(Engine::Pg, pg), None, Some(&c.id))
        .await
        .expect("stored password works");
    assert!(r.server_version.starts_with("PostgreSQL 16."));
    let d = st.duplicate_connection(&c.id, None, None).unwrap();
    st.test_connection(net(Engine::Pg, pg), None, Some(&d.id))
        .await
        .expect("duplicate has the secret");
    st.delete_connection(&c.id).await.unwrap();
    let account = sandb_lib::core::secrets::password_account(&c.id);
    assert_eq!(
        st.secrets().get(&account).unwrap(),
        None,
        "deleted secret must be gone"
    );
    let e = st.test_connection(net(Engine::Pg, pg), None, Some(&c.id))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    // Same target is the only reason the duplicate got the secret: a moved host gets nothing.
    let mut moved = net(Engine::Pg, pg);
    moved.host = "localhost".into();
    let e = st.test_connection(moved, None, Some(&d.id))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
}

#[tokio::test]
async fn mysql_pool_uses_utf8mb4() {
    let (_d, st) = common::state();
    let c = st
        .create_connection(
            net(Engine::Mysql, common::mysql()),
            Some(common::ro_password()),
        )
        .unwrap();
    let sandb_lib::core::db::pool::DbPool::MySql(p) = st.pool(&c.id).unwrap() else {
        panic!("expected a MySQL pool");
    };
    let cs: String = sqlx::query_scalar("SELECT @@character_set_client")
        .fetch_one(&p)
        .await
        .expect("mysql query");
    assert_eq!(cs, "utf8mb4");
    let s: String = sqlx::query_scalar("SELECT CAST('🐘' AS CHAR)")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(s, "🐘");
    st.delete_connection(&c.id).await.unwrap();
    let e = match st.pool(&c.id) {
        Ok(_) => panic!("a deleted connection must not get a pool"),
        Err(e) => e,
    };
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}
