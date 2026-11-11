//! Integration tests against the sandb-test docker stack (`pnpm db:up`).
//! Run: `cargo test --features it`. An unreachable database FAILS the test; nothing is skipped.
#![cfg(feature = "it")]

use std::collections::HashMap;
use std::path::PathBuf;

use sandb_lib::core::connections::{self, ConnectionInput, Engine, Env, SslMode};
use sandb_lib::core::error::ErrorCode;
use sandb_lib::core::secrets::MemoryStore;
use sandb_lib::core::state::AppState;

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

fn ro_password() -> String {
    env_test()
        .get("SANDB_RO_PASSWORD")
        .cloned()
        .expect("SANDB_RO_PASSWORD in testdb/.env.test")
}

fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
    (dir, st)
}

fn net(engine: Engine, port: u16) -> ConnectionInput {
    ConnectionInput {
        name: format!("it-{port}"),
        engine,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        database: "shop".into(),
        user: env_test()
            .get("SANDB_RO_USER")
            .cloned()
            .unwrap_or_else(|| "sandb_ro".into()),
        ssl: SslMode::Prefer,
        file: String::new(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    }
}

async fn expect_ok(label: &str, input: ConnectionInput, prefix: &str) {
    let (_d, st) = state();
    let port = input.port;
    let r = connections::test(&st, input, Some(ro_password()), None)
        .await
        .unwrap_or_else(|e| panic!("{label} on 127.0.0.1:{port} is not reachable or rejected the login (is `pnpm db:up` running?): {e:?}"));
    assert!(
        r.server_version.starts_with(prefix),
        "{label}: unexpected version {:?}",
        r.server_version
    );
    assert!(r.latency_ms < 10_000);
}

#[tokio::test]
async fn pg16_ok() {
    expect_ok("pg16", net(Engine::Pg, 55416), "PostgreSQL 16.").await;
}

#[tokio::test]
async fn pg13_ok() {
    expect_ok("pg13", net(Engine::Pg, 55413), "PostgreSQL 13.").await;
}

#[tokio::test]
async fn mysql_ok() {
    expect_ok("mysql", net(Engine::Mysql, 53384), "MySQL 8.4").await;
}

#[tokio::test]
async fn mariadb_ok() {
    expect_ok("mariadb", net(Engine::Mysql, 53311), "MariaDB 11.").await;
}

#[tokio::test]
async fn sqlite_fixture_ok_and_unchanged() {
    let file = root().join("testdb/sqlite/shop.db");
    let before = std::fs::read(&file).unwrap_or_else(|e| {
        panic!(
            "missing fixture {} (run `pnpm db:sqlite`): {e}",
            file.display()
        )
    });
    let input = ConnectionInput {
        name: "shop.db".into(),
        engine: Engine::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        database: String::new(),
        user: String::new(),
        ssl: SslMode::Prefer,
        file: file.to_string_lossy().into_owned(),
        ai_write: false,
        read_only: false,
        version: None,
        uri: String::new(),
        auth_source: String::new(),
        topology: None,
    };
    let (_d, st) = state();
    let r = connections::test(&st, input, None, None)
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
            !root().join("testdb/sqlite").join(side).exists(),
            "{side} was created"
        );
    }
}

#[tokio::test]
async fn wrong_password_is_auth_failed_on_each_engine() {
    for (label, engine, port) in [
        ("pg16", Engine::Pg, 55416),
        ("pg13", Engine::Pg, 55413),
        ("mysql", Engine::Mysql, 53384),
        ("mariadb", Engine::Mysql, 53311),
    ] {
        let (_d, st) = state();
        let e = connections::test(
            &st,
            net(engine, port),
            Some("definitely-wrong".into()),
            None,
        )
        .await
        .expect_err(&format!("{label}: a wrong password must fail"));
        assert_eq!(e.code, ErrorCode::AuthFailed, "{label}: {e:?}");
    }
}

#[tokio::test]
async fn closed_port_is_connect_failed() {
    for engine in [Engine::Pg, Engine::Mysql] {
        let (_d, st) = state();
        let e = connections::test(&st, net(engine, 1), Some("x".into()), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectFailed, "{engine:?}: {e:?}");
    }
}

#[tokio::test]
async fn saved_connection_tests_with_stored_password() {
    let (_d, st) = state();
    let c = connections::create(&st, net(Engine::Pg, 55416), Some(ro_password())).unwrap();
    let r = connections::test(&st, net(Engine::Pg, 55416), None, Some(&c.id))
        .await
        .expect("stored password works");
    assert!(r.server_version.starts_with("PostgreSQL 16."));
    let d = connections::duplicate(&st, &c.id, None, None).unwrap();
    connections::test(&st, net(Engine::Pg, 55416), None, Some(&d.id))
        .await
        .expect("duplicate has the secret");
    connections::delete(&st, &c.id).await.unwrap();
    let account = sandb_lib::core::secrets::password_account(&c.id);
    assert_eq!(
        st.secrets.get(&account).unwrap(),
        None,
        "deleted secret must be gone"
    );
    let e = connections::test(&st, net(Engine::Pg, 55416), None, Some(&c.id))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    // Same target is the only reason the duplicate got the secret: a moved host gets nothing.
    let mut moved = net(Engine::Pg, 55416);
    moved.host = "localhost".into();
    let e = connections::test(&st, moved, None, Some(&d.id))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
}

#[tokio::test]
async fn mysql_pool_uses_utf8mb4() {
    let (_d, st) = state();
    let c = connections::create(&st, net(Engine::Mysql, 53384), Some(ro_password())).unwrap();
    let sandb_lib::core::db::pool::DbPool::MySql(p) = connections::pool(&st, &c.id).unwrap() else {
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
    connections::delete(&st, &c.id).await.unwrap();
    assert!(p.is_closed());
}
