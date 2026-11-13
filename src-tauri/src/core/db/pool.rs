//! Connection pools per saved connection and database, plus the one-shot probe used by
//! "Test connection". SQL only: a MongoDB connection is probed through its own client.

use std::time::Duration;

use sqlx::Connection as _;
use sqlx::mysql::{
    MySqlConnectOptions, MySqlConnection, MySqlPool, MySqlPoolOptions, MySqlSslMode,
};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};

use crate::core::cache::GenerationCache;
use crate::core::connections::{Connection, SslMode, Topology, expand_home};
use crate::core::engine::{CONNECT_TIMEOUT, MAX_CONNECTIONS, SqlEngine};
use crate::core::error::{AppError, ErrorCode};

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

    #[cfg(any(test, feature = "it"))]
    pub fn is_closed(&self) -> bool {
        match self {
            DbPool::Pg(p) => p.is_closed(),
            DbPool::MySql(p) => p.is_closed(),
            DbPool::Sqlite(p) => p.is_closed(),
        }
    }
}

/// Pools by connection and database, opened lazily (no I/O), so a std mutex is enough. The
/// generation rule is the shared `GenerationCache` one: a pool is never cached from settings that
/// changed while it was being opened.
pub struct PoolCache {
    cache: GenerationCache<(String, String), DbPool>,
}

impl Default for PoolCache {
    fn default() -> Self {
        Self {
            cache: GenerationCache::with_cap(MAX_POOLS_PER_CONNECTION),
        }
    }
}

impl PoolCache {

    /// Read this before reading the connection settings and secret that go into `get_or_open`.
    pub fn generation(&self, id: &str) -> u64 {
        self.cache.generation(id)
    }

    /// `None` when `close` ran after `generation` was read: the caller must re-read the settings.
    pub fn get_or_open(
        &self,
        conn: &Connection,
        database: &str,
        password: Option<&str>,
        generation: u64,
    ) -> Result<Option<DbPool>, AppError> {
        let key = (conn.id.clone(), database.to_string());
        if let Some(p) = self.cache.get(&key, generation) {
            return Ok(Some(p));
        }
        if !self.cache.is_current(&conn.id, generation) {
            return Ok(None);
        }
        let engine = conn
            .engine
            .sql()
            .ok_or_else(AppError::not_sql)?;
        let mut target = conn.clone();
        target.database = database.to_string();
        let p = open_lazy(&target, engine, password)?;
        let Some(cached) = self.cache.insert(key, generation, p) else {
            return Ok(None);
        };
        if let Some(old) = cached.evicted {
            // `close` waits for checked-out connections, so a run on the old pool finishes first.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move { old.close().await });
            }
        }
        Ok(Some(cached.value))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.cache.contains(id)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.cache.len()
    }

    #[cfg(test)]
    fn holds(&self, conn_id: &str, database: &str) -> bool {
        self.cache
            .get(&(conn_id.to_string(), database.to_string()), self.generation(conn_id))
            .is_some()
    }

    /// Closes every pool of `id` and invalidates pools that are being opened right now.
    pub async fn close(&self, id: &str) {
        for p in self.cache.close(id) {
            p.close().await;
        }
    }
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

fn open_lazy(
    c: &Connection,
    engine: SqlEngine,
    password: Option<&str>,
) -> Result<DbPool, AppError> {
    let read_only = c.read_only;
    Ok(match engine {
        SqlEngine::Pg => DbPool::Pg(
            PgPoolOptions::new()
                .max_connections(MAX_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(IDLE_TIMEOUT)
                .acquire_timeout(CONNECT_TIMEOUT)
                .connect_lazy_with(pg_options(c, password)),
        ),
        SqlEngine::Mysql => {
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
        SqlEngine::Sqlite => {
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
    })
}

/// What "Test connection" reports.
pub struct ServerInfo {
    pub version: String,
    /// MongoDB only.
    pub topology: Option<Topology>,
}

/// Closes a probe connection. The version was already read, so a failed close cannot change
/// the answer.
async fn close_quietly<C: sqlx::Connection>(c: C) {
    let _ = c.close().await;
}

pub async fn probe(
    c: &Connection,
    engine: SqlEngine,
    password: Option<&str>,
) -> Result<ServerInfo, AppError> {
    let work = async {
        match engine {
            SqlEngine::Pg => {
                let mut conn = PgConnection::connect_with(&pg_options(c, password)).await?;
                let v: String = sqlx::query_scalar("SELECT current_setting('server_version')")
                    .fetch_one(&mut conn)
                    .await?;
                close_quietly(conn).await;
                Ok(ServerInfo {
                    version: format_pg_version(&v),
                    topology: None,
                })
            }
            SqlEngine::Mysql => {
                let mut conn = MySqlConnection::connect_with(&mysql_options(c, password)).await?;
                let v: String = sqlx::query_scalar("SELECT VERSION()")
                    .fetch_one(&mut conn)
                    .await?;
                close_quietly(conn).await;
                Ok(ServerInfo {
                    version: format_mysql_version(&v),
                    topology: None,
                })
            }
            SqlEngine::Sqlite => {
                let mut conn =
                    SqliteConnection::connect_with(&sqlite_options(c, c.read_only)?).await?;
                let v: String = sqlx::query_scalar("SELECT sqlite_version()")
                    .fetch_one(&mut conn)
                    .await?;
                close_quietly(conn).await;
                Ok(ServerInfo {
                    version: format!("SQLite {v}"),
                    topology: None,
                })
            }
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, work).await {
        Ok(r) => r,
        Err(_) => Err(AppError::connect_timeout()),
    }
}

fn format_pg_version(v: &str) -> String {
    format!("PostgreSQL {}", v.split_whitespace().next().unwrap_or(v))
}

fn format_mysql_version(v: &str) -> String {
    let num = v.split('-').next().unwrap_or(v);
    if SqlEngine::is_mariadb(v) {
        format!("MariaDB {num}")
    } else {
        format!("MySQL {num}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::Env;
    use crate::core::engine::Engine;

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
        assert_eq!(cache.len(), 1, "the same key reuses its pool");
        cache.close("c1").await;
        assert!(p.is_closed());
        assert!(!cache.contains("c1"));
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
        assert_eq!(cache.len(), 2);
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
        assert_eq!(cache.len(), MAX_POOLS_PER_CONNECTION);
        assert!(!cache.holds("c1", "db1"));
        assert!(cache.holds("c1", "db0"));
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
