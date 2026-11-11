//! Connection pools per saved connection, plus the one-shot probe used by "Test connection".

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use sqlx::Connection as _;
use sqlx::mysql::{
    MySqlConnectOptions, MySqlConnection, MySqlPool, MySqlPoolOptions, MySqlSslMode,
};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};

use crate::core::connections::{Connection, Engine, SslMode, Topology, expand_home};
use crate::core::error::{AppError, ErrorCode};
use crate::core::mongo;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const MAX_CONNECTIONS: u32 = 5;
/// Database pools kept per saved connection; browsing more databases closes the least recently used.
const MAX_POOLS_PER_CONNECTION: usize = 8;
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub enum DbPool {
    Pg(PgPool),
    MySql(MySqlPool),
    Sqlite(SqlitePool),
}

impl DbPool {
    pub async fn close(&self) {
        match self {
            DbPool::Pg(p) => p.close().await,
            DbPool::MySql(p) => p.close().await,
            DbPool::Sqlite(p) => p.close().await,
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            DbPool::Pg(p) => p.is_closed(),
            DbPool::MySql(p) => p.is_closed(),
            DbPool::Sqlite(p) => p.is_closed(),
        }
    }
}

#[derive(Default)]
struct Pools {
    /// Keyed by (connection id, database): PG needs one pool per database. The `u64` is the
    /// `tick` of the last use.
    open: HashMap<(String, String), (DbPool, u64)>,
    tick: u64,
    /// Bumped by `close`. A pool built from settings read before the bump is never cached.
    generation: HashMap<String, u64>,
}

/// Pools by connection and database. Opening is lazy (no I/O), so a std mutex is enough.
#[derive(Default)]
pub struct PoolCache {
    inner: Mutex<Pools>,
}

impl PoolCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Pools> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read this before reading the connection settings and secret that go into `get_or_open`.
    pub fn generation(&self, id: &str) -> u64 {
        self.lock().generation.get(id).copied().unwrap_or(0)
    }

    /// The cached pool for (`conn.id`, `database`), or a new one opened with `conn` and `password`.
    /// `None` when `close` ran after `generation` was read: the caller must re-read the settings.
    pub fn get_or_open(
        &self,
        conn: &Connection,
        database: &str,
        password: Option<&str>,
        generation: u64,
    ) -> Result<Option<DbPool>, AppError> {
        let mut pools = self.lock();
        if pools.generation.get(&conn.id).copied().unwrap_or(0) != generation {
            return Ok(None);
        }
        pools.tick += 1;
        let tick = pools.tick;
        let key = (conn.id.clone(), database.to_string());
        if let Some((p, used)) = pools.open.get_mut(&key) {
            *used = tick;
            return Ok(Some(p.clone()));
        }
        let mut target = conn.clone();
        target.database = database.to_string();
        let p = open_lazy(&target, password)?;
        let evicted = evict_lru(&mut pools, &conn.id);
        pools.open.insert(key, (p.clone(), tick));
        if let Some(old) = evicted {
            // `close` waits for checked-out connections, so a run on the old pool finishes first.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move { old.close().await });
            }
        }
        Ok(Some(p))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.lock().open.keys().any(|(c, _)| c == id)
    }

    /// Closes every pool of `id` and invalidates pools that are being opened right now.
    pub async fn close(&self, id: &str) {
        let closing: Vec<DbPool> = {
            let mut pools = self.lock();
            *pools.generation.entry(id.to_string()).or_insert(0) += 1;
            let keys: Vec<_> = pools
                .open
                .keys()
                .filter(|(c, _)| c == id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| pools.open.remove(&k).map(|(p, _)| p))
                .collect()
        };
        for p in closing {
            p.close().await;
        }
    }
}

/// Removes the least recently used pool of `id` when `id` already has the maximum.
fn evict_lru(pools: &mut Pools, id: &str) -> Option<DbPool> {
    let mine = pools.open.iter().filter(|((c, _), _)| c == id);
    if mine.clone().count() < MAX_POOLS_PER_CONNECTION {
        return None;
    }
    let oldest = mine.min_by_key(|(_, (_, used))| *used)?.0.clone();
    pools.open.remove(&oldest).map(|(p, _)| p)
}

fn pg_options(c: &Connection, password: Option<&str>) -> PgConnectOptions {
    // `new_without_pgpass` so PG* env vars and ~/.pgpass never change what the user configured.
    let mut o = PgConnectOptions::new_without_pgpass()
        .host(&c.host)
        .port(c.port)
        .username(&c.user)
        .database(&c.database)
        .application_name("sandb")
        .ssl_mode(match c.ssl {
            SslMode::Disable => PgSslMode::Disable,
            SslMode::Prefer => PgSslMode::Prefer,
            SslMode::Require => PgSslMode::Require,
        });
    if let Some(pw) = password {
        o = o.password(pw);
    }
    // Read-only connections: the database itself refuses every write of every session.
    if c.read_only {
        o = o.options([("default_transaction_read_only", "on")]);
    }
    o
}

fn mysql_options(c: &Connection, password: Option<&str>) -> MySqlConnectOptions {
    // utf8mb4: anything narrower breaks on emoji and other 4-byte characters.
    let mut o = MySqlConnectOptions::new()
        .host(&c.host)
        .port(c.port)
        .username(&c.user)
        .database(&c.database)
        .charset("utf8mb4")
        .ssl_mode(match c.ssl {
            SslMode::Disable => MySqlSslMode::Disabled,
            SslMode::Prefer => MySqlSslMode::Preferred,
            SslMode::Require => MySqlSslMode::Required,
        });
    if let Some(pw) = password {
        o = o.password(pw);
    }
    o
}

pub(crate) fn sqlite_options(
    c: &Connection,
    read_only: bool,
) -> Result<SqliteConnectOptions, AppError> {
    let path = expand_home(&c.file);
    if !path.is_file() {
        return Err(
            AppError::new(ErrorCode::ConnectFailed, "Database file not found")
                .with_detail(path.display().to_string()),
        );
    }
    Ok(SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(read_only))
}

/// A single connection outside any pool.
pub(crate) enum OneConnection {
    Pg(PgConnection),
    MySql(MySqlConnection),
}

/// Opens one connection for `database` (never pooled, never cached) for the AI read's
/// safety-net cancel, which must not queue behind the user's busy pool.
pub(crate) async fn connect_one(
    c: &Connection,
    database: &str,
    password: Option<&str>,
) -> Result<OneConnection, AppError> {
    let mut target = c.clone();
    target.database = database.to_string();
    match c.engine {
        Engine::Pg => PgConnection::connect_with(&pg_options(&target, password))
            .await
            .map(OneConnection::Pg)
            .map_err(AppError::from),
        Engine::Mysql => MySqlConnection::connect_with(&mysql_options(&target, password))
            .await
            .map(OneConnection::MySql)
            .map_err(AppError::from),
        Engine::Sqlite => Err(AppError::internal(
            "SQLite kills never use a second connection",
        )),
        Engine::Mongo => Err(AppError::unsupported_mongo()),
    }
}

fn open_lazy(c: &Connection, password: Option<&str>) -> Result<DbPool, AppError> {
    let read_only = c.read_only;
    Ok(match c.engine {
        Engine::Pg => DbPool::Pg(
            PgPoolOptions::new()
                .max_connections(MAX_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(IDLE_TIMEOUT)
                .acquire_timeout(CONNECT_TIMEOUT)
                .connect_lazy_with(pg_options(c, password)),
        ),
        Engine::Mysql => {
            let mut o = MySqlPoolOptions::new()
                .max_connections(MAX_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(IDLE_TIMEOUT)
                .acquire_timeout(CONNECT_TIMEOUT);
            if read_only {
                // Every pooled session is read-only at the transaction level, so pooled
                // connections stay interchangeable after `release`.
                o = o.after_connect(|c, _| {
                    Box::pin(async move {
                        use sqlx::Executor;
                        c.execute(sqlx::raw_sql("SET SESSION TRANSACTION READ ONLY"))
                            .await?;
                        Ok(())
                    })
                });
            }
            DbPool::MySql(o.connect_lazy_with(mysql_options(c, password)))
        }
        Engine::Sqlite => {
            let mut o = SqlitePoolOptions::new()
                .max_connections(MAX_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(IDLE_TIMEOUT)
                .acquire_timeout(CONNECT_TIMEOUT);
            if read_only {
                // The file opens read-only; query_only is a second barrier on the session.
                o = o.after_connect(|c, _| {
                    Box::pin(async move {
                        use sqlx::Executor;
                        c.execute(sqlx::raw_sql("PRAGMA query_only = ON")).await?;
                        Ok(())
                    })
                });
            }
            DbPool::Sqlite(o.connect_lazy_with(sqlite_options(c, read_only)?))
        }
        // MongoDB has its own client; no SQL pool opens for it. This refuses every SQL path
        // (browse, query, chat, export, edit) on a Mongo connection with `unsupported`.
        Engine::Mongo => return Err(AppError::unsupported_mongo()),
    })
}

/// What "Test connection" reports.
pub struct ServerInfo {
    pub version: String,
    /// MongoDB only.
    pub topology: Option<Topology>,
}

/// Opens one connection (not pooled), reads the server version and closes it. Times out after 10 s.
pub async fn probe(c: &Connection, password: Option<&str>) -> Result<ServerInfo, AppError> {
    let work = async {
        match c.engine {
            Engine::Pg => {
                let mut conn = PgConnection::connect_with(&pg_options(c, password)).await?;
                let v: String = sqlx::query_scalar("SELECT current_setting('server_version')")
                    .fetch_one(&mut conn)
                    .await?;
                let _ = conn.close().await;
                Ok::<_, AppError>(ServerInfo {
                    version: format_pg_version(&v),
                    topology: None,
                })
            }
            Engine::Mysql => {
                let mut conn = MySqlConnection::connect_with(&mysql_options(c, password)).await?;
                let v: String = sqlx::query_scalar("SELECT VERSION()")
                    .fetch_one(&mut conn)
                    .await?;
                let _ = conn.close().await;
                Ok(ServerInfo {
                    version: format_mysql_version(&v),
                    topology: None,
                })
            }
            Engine::Sqlite => {
                let mut conn = SqliteConnection::connect_with(&sqlite_options(c, true)?).await?;
                let v: String = sqlx::query_scalar("SELECT sqlite_version()")
                    .fetch_one(&mut conn)
                    .await?;
                let _ = conn.close().await;
                Ok(ServerInfo {
                    version: format!("SQLite {v}"),
                    topology: None,
                })
            }
            Engine::Mongo => {
                let (version, topology) = mongo::conn::probe(c, password).await?;
                Ok(ServerInfo {
                    version,
                    topology: Some(topology),
                })
            }
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, work).await {
        Ok(r) => r,
        Err(_) => Err(AppError::new(
            ErrorCode::Timeout,
            format!(
                "No answer from the server within {} s",
                CONNECT_TIMEOUT.as_secs()
            ),
        )),
    }
}

/// `16.4 (Debian 16.4-1.pgdg120+2)` → `PostgreSQL 16.4`.
fn format_pg_version(v: &str) -> String {
    format!("PostgreSQL {}", v.split_whitespace().next().unwrap_or(v))
}

/// `11.4.2-MariaDB-ubu2404` → `MariaDB 11.4.2`; `8.4.3` → `MySQL 8.4.3`.
fn format_mysql_version(v: &str) -> String {
    let num = v.split('-').next().unwrap_or(v);
    if v.to_ascii_lowercase().contains("mariadb") {
        format!("MariaDB {num}")
    } else {
        format!("MySQL {num}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::Env;

    fn sqlite_conn(file: &std::path::Path) -> Connection {
        Connection {
            id: "c1".into(),
            name: "x".into(),
            engine: Engine::Sqlite,
            version: String::new(),
            env: Env::Local,
            host: String::new(),
            port: 0,
            database: "main".into(),
            user: String::new(),
            ssl: SslMode::Prefer,
            file: file.to_string_lossy().into_owned(),
            ai_write: false,
            read_only: false,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    #[tokio::test]
    async fn stale_generation_is_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"").unwrap();
        let c = sqlite_conn(&db);
        let cache = PoolCache::default();

        // A reader takes the generation, then an update closes the pools before it opens one.
        let g = cache.generation("c1");
        cache.close("c1").await;
        assert!(cache.get_or_open(&c, "main", None, g).unwrap().is_none());
        assert!(
            !cache.contains("c1"),
            "a pool from stale settings was cached"
        );

        let g = cache.generation("c1");
        assert_eq!(g, 1);
        let p = cache.get_or_open(&c, "main", None, g).unwrap().unwrap();
        assert!(cache.contains("c1"));
        cache.get_or_open(&c, "main", None, g).unwrap().unwrap();
        assert_eq!(cache.lock().open.len(), 1, "the same key reuses its pool");
        cache.close("c1").await;
        assert!(p.is_closed());
        assert!(!cache.contains("c1"));
        // Other connections keep their generation.
        assert_eq!(cache.generation("other"), 0);
    }

    #[tokio::test]
    async fn one_pool_per_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"").unwrap();
        let c = sqlite_conn(&db);
        let cache = PoolCache::default();
        let a = cache.get_or_open(&c, "a", None, 0).unwrap().unwrap();
        let b = cache.get_or_open(&c, "b", None, 0).unwrap().unwrap();
        assert_eq!(cache.lock().open.len(), 2);
        cache.close("c1").await;
        assert!(a.is_closed() && b.is_closed());
    }

    #[tokio::test]
    async fn database_pools_are_capped_lru() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"").unwrap();
        let c = sqlite_conn(&db);
        let cache = PoolCache::default();
        let open = |name: &str| cache.get_or_open(&c, name, None, 0).unwrap().unwrap();
        let first: Vec<DbPool> = (0..MAX_POOLS_PER_CONNECTION)
            .map(|i| open(&format!("db{i}")))
            .collect();
        // db0 is used again, so db1 is now the least recently used.
        open("db0");
        open("extra");
        assert_eq!(cache.lock().open.len(), MAX_POOLS_PER_CONNECTION);
        assert!(!cache.lock().open.contains_key(&("c1".into(), "db1".into())));
        assert!(cache.lock().open.contains_key(&("c1".into(), "db0".into())));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !first[1].is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the evicted pool is closed");
        assert!(!first[0].is_closed());
    }

    #[test]
    fn version_strings() {
        assert_eq!(
            format_pg_version("16.4 (Debian 16.4-1.pgdg120+2)"),
            "PostgreSQL 16.4"
        );
        assert_eq!(format_pg_version("13.16"), "PostgreSQL 13.16");
        assert_eq!(format_mysql_version("8.4.3"), "MySQL 8.4.3");
        assert_eq!(
            format_mysql_version("11.4.2-MariaDB-ubu2404"),
            "MariaDB 11.4.2"
        );
    }
}
