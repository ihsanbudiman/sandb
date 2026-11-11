//! Saved connections: model, validation, CRUD and the connection test.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::core::db::pool::{self, DbPool};
use crate::core::error::AppError;
use crate::core::mongo;
use crate::core::secrets::password_account;
use crate::core::state::AppState;

/// `mysql` covers MySQL and MariaDB (same driver; the version string tells them apart).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Pg,
    Mysql,
    Sqlite,
    Mongo,
}

impl Engine {
    pub fn default_port(self) -> u16 {
        match self {
            Engine::Pg => 5432,
            Engine::Mysql => 3306,
            Engine::Sqlite => 0,
            Engine::Mongo => 27017,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Production,
    Staging,
    #[default]
    Local,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

/// The kind of MongoDB deployment, from the last successful test. Later phases need it: only a
/// replica set or a sharded cluster has transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    Standalone,
    ReplicaSet,
    Sharded,
}

/// A saved connection. The password is never part of it: it lives in the secret store only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    /// Server version from the last successful test, e.g. `PostgreSQL 16.4`. Empty if never tested.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    /// `main` for SQLite.
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub ssl: SslMode,
    /// SQLite database file; empty for other engines.
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub ai_write: bool,
    /// Blocks every write on this connection, user queries included. Missing in files saved
    /// before phase 3b, so `false`.
    #[serde(default)]
    pub read_only: bool,
    /// MongoDB connection string WITHOUT the password; empty when the fields are used. The
    /// password of the string lives in the secret store, like every other password.
    #[serde(default)]
    pub uri: String,
    /// MongoDB `authSource` for the fields; empty is the driver default.
    #[serde(default)]
    pub auth_source: String,
    /// MongoDB only, from the last successful test.
    #[serde(default)]
    pub topology: Option<Topology>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInput {
    pub name: String,
    pub engine: Engine,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub ssl: SslMode,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub ai_write: bool,
    #[serde(default)]
    pub read_only: bool,
    /// MongoDB connection string as typed, password included: `normalize` takes the password out.
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub auth_source: String,
    /// Topology reported by a test run in the dialog. `None` keeps the stored one.
    #[serde(default)]
    pub topology: Option<Topology>,
    /// Version reported by a test run in the dialog. `None` keeps the stored one.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub server_version: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<Topology>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Trims fields and fills engine defaults. `require_name` is off for a test run from an unnamed form.
/// The second value is the password of a MongoDB connection string; it overrides the password
/// argument of the caller.
fn normalize(
    input: ConnectionInput,
    require_name: bool,
) -> Result<(ConnectionInput, Option<String>), AppError> {
    let mut i = input;
    i.name = i.name.trim().to_string();
    i.host = i.host.trim().to_string();
    i.database = i.database.trim().to_string();
    i.user = i.user.trim().to_string();
    i.file = i.file.trim().to_string();
    i.uri = i.uri.trim().to_string();
    i.auth_source = i.auth_source.trim().to_string();
    if require_name && i.name.is_empty() {
        return Err(AppError::invalid("Name is required"));
    }
    // A read-only connection has no use for the AI write permission.
    if i.read_only {
        i.ai_write = false;
    }
    let mut uri_password = None;
    match i.engine {
        Engine::Sqlite => {
            if i.file.is_empty() {
                return Err(AppError::invalid("Database file is required"));
            }
            i.database = "main".into();
            i.host.clear();
            i.user.clear();
            i.port = 0;
        }
        Engine::Pg | Engine::Mysql => {
            if i.host.is_empty() {
                return Err(AppError::invalid("Host is required"));
            }
            if i.database.is_empty() {
                return Err(AppError::invalid("Database is required"));
            }
            if i.user.is_empty() {
                return Err(AppError::invalid("User is required"));
            }
            if i.port == 0 {
                i.port = i.engine.default_port();
            }
            i.file.clear();
        }
        // The database and the user are optional: a server can run without auth, and the
        // databases are listed from the server.
        Engine::Mongo => {
            i.file.clear();
            if !i.uri.is_empty() {
                // The string decides; the fields only show what it says.
                let p = mongo::conn::parse_uri(&i.uri)?;
                i.uri = p.redacted;
                i.host = p.host;
                i.port = p.port;
                i.user = p.user;
                i.database = p.database;
                i.auth_source = p.auth_source;
                i.ssl = p.ssl;
                uri_password = p.password;
            } else if i.host.is_empty() {
                return Err(AppError::invalid("Host is required"));
            } else if i.ssl == SslMode::Prefer {
                return Err(AppError::invalid(
                    "MongoDB has no Prefer TLS mode. Choose Disable or Require.",
                ));
            }
            if i.port == 0 {
                i.port = i.engine.default_port();
            }
        }
    }
    if i.engine != Engine::Mongo {
        i.uri.clear();
        i.auth_source.clear();
        i.topology = None;
    }
    Ok((i, uri_password))
}

fn build(
    id: String,
    i: ConnectionInput,
    prev_version: &str,
    prev_topology: Option<Topology>,
) -> Connection {
    Connection {
        id,
        name: i.name,
        engine: i.engine,
        version: i.version.unwrap_or_else(|| prev_version.to_string()),
        env: i.env,
        host: i.host,
        port: i.port,
        database: i.database,
        user: i.user,
        ssl: i.ssl,
        file: i.file,
        ai_write: i.ai_write,
        read_only: i.read_only,
        topology: match i.engine {
            Engine::Mongo => i.topology.or(prev_topology),
            _ => None,
        },
        uri: i.uri,
        auth_source: i.auth_source,
    }
}

fn input_of(c: &Connection) -> ConnectionInput {
    ConnectionInput {
        name: c.name.clone(),
        engine: c.engine,
        env: c.env,
        host: c.host.clone(),
        port: c.port,
        database: c.database.clone(),
        user: c.user.clone(),
        ssl: c.ssl,
        file: c.file.clone(),
        ai_write: c.ai_write,
        read_only: c.read_only,
        version: Some(c.version.clone()),
        uri: c.uri.clone(),
        auth_source: c.auth_source.clone(),
        topology: c.topology,
    }
}

/// Empty string means "no password".
fn store_password(state: &AppState, id: &str, password: &str) -> Result<(), AppError> {
    let account = password_account(id);
    if password.is_empty() {
        state.secrets.delete(&account)
    } else {
        state.secrets.set(&account, password)
    }
}

/// The stored secret of `src`, but only for the same target: it must never be sent to another server.
fn reuse_secret(
    state: &AppState,
    src: &Connection,
    target: &ConnectionInput,
) -> Result<Option<String>, AppError> {
    // `uri` is saved without the password, so a changed host list or SRV host shows here.
    let same = src.engine == target.engine
        && src.host == target.host
        && src.port == target.port
        && src.user == target.user
        && src.file == target.file
        && src.uri == target.uri;
    if !same {
        return Err(AppError::invalid(
            "Enter the password to test changed connection settings",
        ));
    }
    state.secrets.get(&password_account(&src.id))
}

fn find(state: &AppState, id: &str) -> Result<Connection, AppError> {
    lock(&state.connections)
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("Connection {id} not found")))
}

/// Adds `conn` (and its secret, if any) and writes through to disk; rolls the secret back on failure.
fn insert(
    state: &AppState,
    conn: Connection,
    password: Option<&str>,
) -> Result<Connection, AppError> {
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        state.secrets.set(&password_account(&conn.id), pw)?;
    }
    let mut list = lock(&state.connections);
    let mut next = list.clone();
    next.push(conn.clone());
    if let Err(e) = state.store.save_connections(&next) {
        let _ = state.secrets.delete(&password_account(&conn.id));
        return Err(e);
    }
    *list = next;
    Ok(conn)
}

pub fn list(state: &AppState) -> Vec<Connection> {
    lock(&state.connections).clone()
}

pub fn create(
    state: &AppState,
    input: ConnectionInput,
    password: Option<String>,
) -> Result<Connection, AppError> {
    let (i, uri_password) = normalize(input, true)?;
    let conn = build(uuid::Uuid::new_v4().to_string(), i, "", None);
    insert(state, conn, uri_password.or(password).as_deref())
}

/// `password: None` keeps the stored secret; `Some("")` clears it.
pub async fn update(
    state: &AppState,
    id: &str,
    input: ConnectionInput,
    password: Option<String>,
) -> Result<Connection, AppError> {
    let (i, uri_password) = normalize(input, true)?;
    let password = uri_password.or(password);
    let updated = {
        let mut list = lock(&state.connections);
        let pos = list
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| AppError::not_found(format!("Connection {id} not found")))?;
        if list[pos].engine != i.engine {
            return Err(AppError::invalid(
                "The engine of a saved connection cannot change",
            ));
        }
        let updated = build(id.to_string(), i, &list[pos].version, list[pos].topology);
        let mut next = list.clone();
        next[pos] = updated.clone();
        // Secret first, so the JSON never points at settings whose password was not saved.
        let previous = match &password {
            Some(pw) => {
                let prev = state.secrets.get(&password_account(id))?;
                store_password(state, id, pw)?;
                Some(prev)
            }
            None => None,
        };
        if let Err(e) = state.store.save_connections(&next) {
            if let Some(prev) = previous {
                let _ = store_password(state, id, prev.as_deref().unwrap_or(""));
            }
            return Err(e);
        }
        *list = next;
        updated
    };
    // Production tabs keep their own session: drop it too, so the next run reconnects with the
    // new settings instead of continuing a transaction on the old target. Sessions first: a
    // pool close waits for every connection it lent out.
    state.sessions.close_conn(id).await;
    // After the new secret is stored, so a pool reopened right away cannot pick up the old one.
    state.pools.close(id).await;
    state.mongo.close(id).await;
    Ok(updated)
}

/// New connection with a new id and a copy of the source secret.
/// `input` overrides the copied fields (the dialog lets the user edit before saving);
/// `password: Some` replaces the copied secret.
pub fn duplicate(
    state: &AppState,
    id: &str,
    input: Option<ConnectionInput>,
    password: Option<String>,
) -> Result<Connection, AppError> {
    let src = find(state, id)?;
    let (i, uri_password) = match input {
        Some(i) => {
            if i.engine != src.engine {
                return Err(AppError::invalid(
                    "The engine of a duplicated connection cannot change",
                ));
            }
            normalize(i, true)?
        }
        None => {
            let mut i = input_of(&src);
            i.name = format!("{} (copy)", src.name);
            i.ai_write = false;
            (i, None)
        }
    };
    let password = match uri_password.or(password) {
        Some(p) => Some(p),
        None => reuse_secret(state, &src, &i)?,
    };
    let conn = build(
        uuid::Uuid::new_v4().to_string(),
        i,
        &src.version,
        src.topology,
    );
    insert(state, conn, password.as_deref())
}

/// Deletes the secret first: if the JSON write then fails, the connection stays listed without
/// its password, which is acceptable for a connection the user is deleting. The reverse order
/// could leave an orphan secret in the password file.
pub async fn delete(state: &AppState, id: &str) -> Result<(), AppError> {
    {
        let mut list = lock(&state.connections);
        let pos = list
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| AppError::not_found(format!("Connection {id} not found")))?;
        state.secrets.delete(&password_account(id))?;
        let mut next = list.clone();
        next.remove(pos);
        state.store.save_connections(&next)?;
        *list = next;
    }
    state.sessions.close_conn(id).await;
    state.pools.close(id).await;
    state.mongo.close(id).await;
    Ok(())
}

/// Saves the list in the order of `ids`. `ids` must name every saved connection once, so a stale
/// order (a connection added or deleted in between) is rejected instead of dropping one.
pub fn reorder(state: &AppState, ids: &[String]) -> Result<Vec<Connection>, AppError> {
    let mut list = lock(&state.connections);
    let mut next = Vec::with_capacity(list.len());
    for id in ids {
        match list.iter().find(|c| &c.id == id) {
            Some(c) if !next.iter().any(|n: &Connection| n.id == c.id) => next.push(c.clone()),
            _ => return Err(AppError::invalid("The connection order is out of date")),
        }
    }
    if next.len() != list.len() {
        return Err(AppError::invalid("The connection order is out of date"));
    }
    state.store.save_connections(&next)?;
    *list = next.clone();
    Ok(next)
}

/// Tests the settings in `input` (saved or not). Password: the given one, else the stored one of `id`.
pub async fn test(
    state: &AppState,
    input: ConnectionInput,
    password: Option<String>,
    id: Option<&str>,
) -> Result<TestResult, AppError> {
    let (i, uri_password) = normalize(input, false)?;
    let password = match (uri_password.or(password), id) {
        (Some(p), _) => Some(p),
        (None, Some(id)) => reuse_secret(state, &find(state, id)?, &i)?,
        (None, None) => None,
    };
    let conn = build(String::new(), i, "", None);
    let started = Instant::now();
    let info = pool::probe(&conn, password.as_deref()).await?;
    Ok(TestResult {
        server_version: info.version,
        latency_ms: started.elapsed().as_millis() as u64,
        topology: info.topology,
    })
}

/// A saved connection by id.
pub fn get(state: &AppState, id: &str) -> Result<Connection, AppError> {
    find(state, id)
}

/// The database a request targets: the given one, else the connection's own. Always `main` on SQLite.
pub fn target_database(conn: &Connection, database: Option<&str>) -> String {
    match (conn.engine, database.map(str::trim)) {
        (Engine::Sqlite, _) => "main".into(),
        (_, Some(d)) if !d.is_empty() => d.to_string(),
        _ => conn.database.clone(),
    }
}

/// The cached pool of a saved connection, opened lazily on first use.
pub fn pool(state: &AppState, id: &str) -> Result<DbPool, AppError> {
    pool_for(state, id, None)
}

/// The cached pool of a saved connection for one database (PG has one pool per database).
pub fn pool_for(state: &AppState, id: &str, database: Option<&str>) -> Result<DbPool, AppError> {
    // An update or delete between reading the settings and caching the pool bumps the generation;
    // re-read so a pool with stale settings is never cached.
    for _ in 0..3 {
        let generation = state.pools.generation(id);
        let conn = find(state, id)?;
        let password = state.secrets.get(&password_account(id))?;
        let db = target_database(&conn, database);
        if let Some(p) = state
            .pools
            .get_or_open(&conn, &db, password.as_deref(), generation)?
        {
            return Ok(p);
        }
    }
    Err(AppError::internal(
        "The connection settings changed while opening it. Try again.",
    ))
}

/// `~/x` → `$HOME/x` for SQLite paths typed in the dialog.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorCode;
    use crate::core::secrets::{MemoryStore, SecretStore};
    use crate::core::store::CONNECTIONS_FILE;

    const PW: &str = "s3cret-Pa55-unique-7f1c";

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        (dir, st)
    }

    fn pg_input(name: &str) -> ConnectionInput {
        ConnectionInput {
            name: name.into(),
            engine: Engine::Pg,
            env: Env::Staging,
            host: " 127.0.0.1 ".into(),
            port: 0,
            database: "shop".into(),
            user: "sandb_ro".into(),
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

    fn sqlite_input(file: &std::path::Path) -> ConnectionInput {
        ConnectionInput {
            name: "local".into(),
            engine: Engine::Sqlite,
            env: Env::Local,
            host: "ignored".into(),
            port: 99,
            database: "ignored".into(),
            user: "ignored".into(),
            ssl: SslMode::Prefer,
            file: file.to_string_lossy().into_owned(),
            ai_write: false,
            read_only: false,
            version: None,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    fn secret(st: &AppState, id: &str) -> Option<String> {
        st.secrets.get(&password_account(id)).unwrap()
    }

    /// Records every read and can be told to fail deletes.
    #[derive(Default)]
    struct Spy {
        inner: MemoryStore,
        accounts: Mutex<std::collections::BTreeSet<String>>,
        gets: Mutex<Vec<String>>,
        fail_delete: std::sync::atomic::AtomicBool,
    }

    #[derive(Clone, Default)]
    struct SpyStore(std::sync::Arc<Spy>);

    impl SpyStore {
        fn gets(&self) -> Vec<String> {
            self.0.gets.lock().unwrap().clone()
        }
        fn stored_count(&self) -> usize {
            self.0.accounts.lock().unwrap().len()
        }
    }

    impl SecretStore for SpyStore {
        fn get(&self, account: &str) -> Result<Option<String>, AppError> {
            self.0.gets.lock().unwrap().push(account.to_string());
            self.0.inner.get(account)
        }
        fn set(&self, account: &str, secret: &str) -> Result<(), AppError> {
            self.0.accounts.lock().unwrap().insert(account.to_string());
            self.0.inner.set(account, secret)
        }
        fn delete(&self, account: &str) -> Result<(), AppError> {
            if self.0.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::SecretStore,
                    "password file unreadable",
                ));
            }
            self.0.accounts.lock().unwrap().remove(account);
            self.0.inner.delete(account)
        }
    }

    fn spy_state() -> (tempfile::TempDir, AppState, SpyStore) {
        let dir = tempfile::tempdir().unwrap();
        let spy = SpyStore::default();
        let st = AppState::new(dir.path(), Box::new(spy.clone())).unwrap();
        (dir, st, spy)
    }

    /// Makes every later `save_connections` fail: the atomic rename cannot replace a directory.
    fn block_json_writes(dir: &std::path::Path) {
        let path = dir.join(CONNECTIONS_FILE);
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir(&path).unwrap();
    }

    #[test]
    fn create_normalizes_and_persists() {
        let (dir, st) = state();
        let c = create(&st, pg_input(" a "), None).unwrap();
        assert_eq!(c.name, "a");
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 5432, "port 0 means the engine default");
        let reloaded = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        assert_eq!(list(&reloaded), vec![c]);
    }

    #[test]
    fn password_is_never_written_to_connections_json() {
        let (dir, st) = state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        duplicate(&st, &c.id, None, None).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(CONNECTIONS_FILE)).unwrap();
        assert!(!raw.contains(PW), "password leaked into connections.json");
        assert!(
            !raw.to_lowercase().contains("password"),
            "no password field at all: {raw}"
        );
        assert_eq!(secret(&st, &c.id).as_deref(), Some(PW));
        // The API shape the UI sees has no password either.
        let json = serde_json::to_string(&list(&st)).unwrap();
        assert!(!json.contains(PW));
    }

    #[test]
    fn create_validates_input() {
        let (_d, st) = state();
        let mut i = pg_input("  ");
        assert_eq!(
            create(&st, i.clone(), None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        i.name = "x".into();
        i.host = " ".into();
        assert_eq!(
            create(&st, i, None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        let mut s = sqlite_input(std::path::Path::new(""));
        s.file = "  ".into();
        assert_eq!(
            create(&st, s, None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert!(list(&st).is_empty());
    }

    #[test]
    fn sqlite_clears_network_fields() {
        let (_d, st) = state();
        let c = create(
            &st,
            sqlite_input(std::path::Path::new("/tmp/x.db")),
            Some("pw".into()),
        )
        .unwrap();
        assert_eq!(
            (
                c.database.as_str(),
                c.host.as_str(),
                c.user.as_str(),
                c.port
            ),
            ("main", "", "", 0)
        );
    }

    #[test]
    fn reorder_persists_and_rejects_stale_orders() {
        let (dir, st) = state();
        let a = create(&st, pg_input("a"), None).unwrap();
        let b = create(&st, pg_input("b"), None).unwrap();
        let c = create(&st, pg_input("c"), None).unwrap();
        let ids = |v: &[&Connection]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        reorder(&st, &ids(&[&c, &a, &b])).unwrap();
        let reloaded = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        assert_eq!(list(&reloaded), vec![c.clone(), a.clone(), b.clone()]);
        // Missing, duplicated or unknown ids would drop or invent a connection: the order is kept.
        for bad in [
            ids(&[&a, &b]),
            ids(&[&a, &a, &b]),
            vec![a.id.clone(), b.id.clone(), "x".into()],
        ] {
            assert_eq!(
                reorder(&st, &bad).unwrap_err().code,
                ErrorCode::InvalidInput
            );
        }
        assert_eq!(list(&st), vec![c, a, b]);
    }

    #[tokio::test]
    async fn update_keeps_password_when_none_and_replaces_when_some() {
        let (_d, st) = state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        let mut i = pg_input("renamed");
        i.version = Some("PostgreSQL 16.4".into());
        let u = update(&st, &c.id, i, None).await.unwrap();
        assert_eq!(u.name, "renamed");
        assert_eq!(u.version, "PostgreSQL 16.4");
        assert_eq!(secret(&st, &c.id).as_deref(), Some(PW));

        // No version in the input keeps the stored one.
        let u = update(&st, &c.id, pg_input("again"), Some("new".into()))
            .await
            .unwrap();
        assert_eq!(u.version, "PostgreSQL 16.4");
        assert_eq!(secret(&st, &c.id).as_deref(), Some("new"));

        update(&st, &c.id, pg_input("again"), Some(String::new()))
            .await
            .unwrap();
        assert_eq!(secret(&st, &c.id), None, "empty password clears the secret");
    }

    #[tokio::test]
    async fn update_rejects_engine_change_and_unknown_id() {
        let (_d, st) = state();
        let c = create(&st, pg_input("a"), None).unwrap();
        let mut i = pg_input("a");
        i.engine = Engine::Mysql;
        assert_eq!(
            update(&st, &c.id, i, None).await.unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            update(&st, "nope", pg_input("a"), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(list(&st)[0].engine, Engine::Pg);
    }

    #[test]
    fn duplicate_copies_fields_and_secret() {
        let (_d, st) = state();
        let mut i = pg_input("a");
        i.ai_write = true;
        let c = create(&st, i, Some(PW.into())).unwrap();
        let d = duplicate(&st, &c.id, None, None).unwrap();
        assert_ne!(d.id, c.id);
        assert_eq!(d.name, "a (copy)");
        assert!(!d.ai_write, "a copy starts with AI writes off");
        assert_eq!(
            (d.host.as_str(), d.port, d.database.as_str()),
            (c.host.as_str(), c.port, c.database.as_str())
        );
        assert_eq!(secret(&st, &d.id).as_deref(), Some(PW));
        assert_eq!(list(&st).len(), 2);
    }

    #[test]
    fn duplicate_with_edits_and_new_password() {
        let (_d, st) = state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        let d = duplicate(&st, &c.id, Some(pg_input("edited")), Some("other".into())).unwrap();
        assert_eq!(d.name, "edited");
        assert_eq!(secret(&st, &d.id).as_deref(), Some("other"));
        assert_eq!(
            secret(&st, &c.id).as_deref(),
            Some(PW),
            "source secret untouched"
        );
        let mut bad = pg_input("x");
        bad.engine = Engine::Mysql;
        assert_eq!(
            duplicate(&st, &c.id, Some(bad), None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            duplicate(&st, "nope", None, None).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn delete_removes_connection_secret_and_pool() {
        let (dir, st) = state();
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"").unwrap();
        let c = create(&st, sqlite_input(&db), None).unwrap();
        let keep = create(&st, pg_input("keep"), Some("k".into())).unwrap();
        st.secrets.set(&password_account(&c.id), PW).unwrap();
        let p = pool(&st, &c.id).unwrap();
        assert!(st.pools.contains(&c.id));
        assert!(!p.is_closed());

        delete(&st, &c.id).await.unwrap();
        assert!(!st.pools.contains(&c.id));
        assert!(
            p.is_closed(),
            "pool handed out before delete must be closed"
        );
        assert_eq!(secret(&st, &c.id), None);
        assert_eq!(list(&st), vec![keep.clone()]);
        assert_eq!(secret(&st, &keep.id).as_deref(), Some("k"));
        assert_eq!(
            delete(&st, &c.id).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn update_closes_the_cached_pool() {
        let (dir, st) = state();
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"").unwrap();
        let c = create(&st, sqlite_input(&db), None).unwrap();
        let p1 = pool(&st, &c.id).unwrap();
        assert!(st.pools.contains(&c.id));
        update(&st, &c.id, sqlite_input(&db), None).await.unwrap();
        assert!(p1.is_closed());
        assert!(!st.pools.contains(&c.id));
    }

    #[tokio::test]
    async fn test_sqlite_file_without_docker() {
        let (dir, st) = state();
        let db = dir.path().join("t.db");
        std::fs::write(&db, b"").unwrap();
        let r = test(&st, sqlite_input(&db), None, None).await.unwrap();
        assert!(
            r.server_version.starts_with("SQLite 3."),
            "{}",
            r.server_version
        );

        let missing = dir.path().join("missing.db");
        let e = test(&st, sqlite_input(&missing), None, None)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectFailed);
        assert!(!missing.exists(), "a test must not create the file");
    }

    #[tokio::test]
    async fn test_uses_stored_password_of_id() {
        let (dir, st, spy) = spy_state();
        let db = dir.path().join("t.db");
        std::fs::write(&db, b"").unwrap();
        let c = create(&st, sqlite_input(&db), Some(PW.into())).unwrap();
        test(&st, sqlite_input(&db), None, Some(&c.id))
            .await
            .unwrap();
        assert_eq!(spy.gets(), vec![password_account(&c.id)]);

        // Name is not required for a test; validation still runs.
        let mut i = pg_input("");
        i.host = String::new();
        assert_eq!(
            test(&st, i, None, Some(&c.id)).await.unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }

    #[tokio::test]
    async fn test_never_sends_stored_password_to_a_changed_target() {
        let (dir, st, spy) = spy_state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        let mut host = pg_input("a");
        host.host = "attacker.example".into();
        let mut port = pg_input("a");
        port.port = 6543;
        let mut user = pg_input("a");
        user.user = "postgres".into();
        for i in [host, port, user] {
            let e = test(&st, i, None, Some(&c.id)).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
        }
        assert!(spy.gets().is_empty(), "stored secret must not be read");

        let db = dir.path().join("a.db");
        let other = dir.path().join("b.db");
        std::fs::write(&db, b"").unwrap();
        std::fs::write(&other, b"").unwrap();
        let s = create(&st, sqlite_input(&db), Some(PW.into())).unwrap();
        assert_eq!(
            test(&st, sqlite_input(&other), None, Some(&s.id))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        assert!(spy.gets().is_empty());
    }

    #[test]
    fn duplicate_never_copies_secret_to_a_changed_target() {
        let (_d, st) = state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        let mut i = pg_input("copy");
        i.host = "attacker.example".into();
        assert_eq!(
            duplicate(&st, &c.id, Some(i.clone()), None)
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        assert_eq!(list(&st).len(), 1);
        // A typed password is fine for a new target.
        let d = duplicate(&st, &c.id, Some(i), Some("typed".into())).unwrap();
        assert_eq!(secret(&st, &d.id).as_deref(), Some("typed"));
    }

    fn mongo_input(name: &str, uri: &str) -> ConnectionInput {
        ConnectionInput {
            engine: Engine::Mongo,
            database: String::new(),
            user: String::new(),
            ssl: SslMode::Disable,
            uri: uri.into(),
            ..pg_input(name)
        }
    }

    #[tokio::test]
    async fn mongo_string_password_lives_in_the_secret_store_only() {
        let (dir, st) = state();
        let typed = format!("mongodb://ann:{PW}@db1:27018,db2/shop?authSource=admin");
        let c = create(&st, mongo_input("m", &typed), None).unwrap();
        assert_eq!(c.uri, "mongodb://ann@db1:27018,db2/shop?authSource=admin");
        assert_eq!(
            (
                c.host.as_str(),
                c.port,
                c.user.as_str(),
                c.database.as_str()
            ),
            ("db1,db2", 27018, "ann", "shop")
        );
        assert_eq!(c.auth_source, "admin");
        assert_eq!(secret(&st, &c.id).as_deref(), Some(PW));
        let raw = std::fs::read_to_string(dir.path().join(CONNECTIONS_FILE)).unwrap();
        assert!(!raw.contains(PW), "password leaked into connections.json");

        // The dialog sends back the saved string: the stored password stays.
        let same = mongo_input("renamed", &c.uri);
        update(&st, &c.id, same, None).await.unwrap();
        assert_eq!(secret(&st, &c.id).as_deref(), Some(PW));
        // A password typed into the string replaces it, even over the password argument.
        let new = mongo_input("m", "mongodb://ann:n3w@db1:27018,db2/shop?authSource=admin");
        update(&st, &c.id, new, Some("ignored".into()))
            .await
            .unwrap();
        assert_eq!(secret(&st, &c.id).as_deref(), Some("n3w"));
    }

    #[tokio::test]
    async fn mongo_secret_never_follows_a_changed_host_list_or_srv_host() {
        let (_d, st, spy) = spy_state();
        let list_conn = create(
            &st,
            mongo_input("a", &format!("mongodb://ann:{PW}@db1,db2/shop")),
            None,
        )
        .unwrap();
        let srv_conn = create(
            &st,
            mongo_input(
                "b",
                &format!("mongodb+srv://ann:{PW}@cluster0.example.net/shop"),
            ),
            None,
        )
        .unwrap();
        for (id, uri) in [
            (&list_conn.id, "mongodb://ann@db1,attacker.example/shop"),
            (&list_conn.id, "mongodb://ann@db1/shop"),
            (&srv_conn.id, "mongodb+srv://ann@attacker.example/shop"),
            (&srv_conn.id, "mongodb://ann@cluster0.example.net/shop"),
        ] {
            let e = test(&st, mongo_input("x", uri), None, Some(id))
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{uri}: {e:?}");
            let e = duplicate(&st, id, Some(mongo_input("x", uri)), None).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{uri}: {e:?}");
        }
        assert!(spy.gets().is_empty(), "stored secret must not be read");
        assert_eq!(list(&st).len(), 2);
        // The same target gets the copy.
        let d = duplicate(&st, &srv_conn.id, None, None).unwrap();
        assert_eq!(secret(&st, &d.id).as_deref(), Some(PW));
    }

    #[test]
    fn mongo_fields_rules() {
        let (_d, st) = state();
        let mut i = mongo_input("m", "");
        i.host = "127.0.0.1".into();
        // Database and user are optional; the port gets the Mongo default.
        let c = create(&st, i.clone(), None).unwrap();
        assert_eq!(
            (c.port, c.database.as_str(), c.user.as_str()),
            (27017, "", "")
        );
        let mut prefer = i.clone();
        prefer.ssl = SslMode::Prefer;
        assert_eq!(
            create(&st, prefer, None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        let mut no_host = i.clone();
        no_host.host.clear();
        assert_eq!(
            create(&st, no_host, None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        let bad = mongo_input("m", "mongodb://h:notaport");
        assert_eq!(
            create(&st, bad, None).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        // Mongo-only fields never stick to a SQL connection.
        let mut pg = pg_input("p");
        pg.uri = "mongodb://h".into();
        pg.auth_source = "admin".into();
        pg.topology = Some(Topology::ReplicaSet);
        let p = create(&st, pg, None).unwrap();
        assert_eq!(
            (p.uri.as_str(), p.auth_source.as_str(), p.topology),
            ("", "", None)
        );
    }

    #[tokio::test]
    async fn delete_keeps_connection_when_secret_delete_fails() {
        let (dir, st, spy) = spy_state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        spy.0
            .fail_delete
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            delete(&st, &c.id).await.unwrap_err().code,
            ErrorCode::SecretStore
        );
        assert_eq!(list(&st), vec![c.clone()]);
        let reloaded = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        assert_eq!(list(&reloaded), vec![c]);
    }

    #[tokio::test]
    async fn delete_reports_a_failed_json_write() {
        let (dir, st, spy) = spy_state();
        let c = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        block_json_writes(dir.path());
        assert_eq!(delete(&st, &c.id).await.unwrap_err().code, ErrorCode::Io);
        assert_eq!(list(&st), vec![c], "memory must match the unchanged file");
        assert_eq!(spy.stored_count(), 0, "no orphan secret is left behind");
    }

    #[tokio::test]
    async fn update_restores_the_previous_secret_when_json_write_fails() {
        let (dir, st, spy) = spy_state();
        let with_pw = create(&st, pg_input("a"), Some(PW.into())).unwrap();
        let without_pw = create(&st, pg_input("b"), None).unwrap();
        block_json_writes(dir.path());

        let e = update(&st, &with_pw.id, pg_input("x"), Some("new".into()))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Io);
        assert_eq!(secret(&st, &with_pw.id).as_deref(), Some(PW));

        let e = update(&st, &without_pw.id, pg_input("y"), Some("new".into()))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Io);
        assert_eq!(secret(&st, &without_pw.id), None);
        assert_eq!(spy.stored_count(), 1);
        assert_eq!(list(&st), vec![with_pw, without_pw]);
    }

    #[test]
    fn create_with_unwritable_json_leaves_no_secret() {
        let (dir, st, spy) = spy_state();
        block_json_writes(dir.path());
        assert_eq!(
            create(&st, pg_input("a"), Some(PW.into()))
                .unwrap_err()
                .code,
            ErrorCode::Io
        );
        assert!(list(&st).is_empty());
        assert_eq!(spy.stored_count(), 0, "secret must be rolled back");
    }

    #[test]
    fn expand_home_only_touches_tilde_prefix() {
        assert_eq!(expand_home("/a/b.db"), PathBuf::from("/a/b.db"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/x.db"), PathBuf::from(home).join("x.db"));
        }
    }

    #[test]
    fn connection_json_shape_is_camel_case() {
        let (_d, st) = state();
        let mut i = pg_input("a");
        i.ai_write = true;
        let c = create(&st, i, None).unwrap();
        let v = serde_json::to_value(&c).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "aiWrite",
                "authSource",
                "database",
                "engine",
                "env",
                "file",
                "host",
                "id",
                "name",
                "port",
                "readOnly",
                "ssl",
                "topology",
                "uri",
                "user",
                "version"
            ]
        );
        assert_eq!(v["engine"], "pg");
        assert_eq!(v["env"], "staging");
        assert_eq!(v["ssl"], "prefer");
    }
}
