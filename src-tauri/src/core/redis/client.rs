//! One cached RESP3 connection per saved Redis connection and logical database. Separate
//! connections keep `SELECT` out of shared sessions. Settings changes invalidate every database.

use crate::core::cache::GenerationCache;
use crate::core::connections::{Connection, RedisMode};
use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::query_registry::QueryRegistry;
use crate::core::secrets::{SecretStore, password_account};

use super::conn::{RedisConn, mode, open, parse_database};

const MAX_DATABASES_PER_CONNECTION: usize = 8;

pub struct RedisClients {
    cache: GenerationCache<(String, String), RedisConn>,
}

impl Default for RedisClients {
    fn default() -> Self {
        Self {
            cache: GenerationCache::with_cap(MAX_DATABASES_PER_CONNECTION),
        }
    }
}

impl RedisClients {
    pub fn generation(&self, id: &str) -> u64 {
        self.cache.generation(id)
    }

    pub fn close(&self, id: &str) {
        drop(self.cache.close(id));
    }

    pub fn evict(&self, id: &str, db: &str) {
        if let Ok(db) = parse_database(db) {
            self.cache.remove(&(id.to_string(), db.to_string()));
        }
    }
}

pub trait Access {
    fn connection(&self, id: &str) -> Result<Connection, AppError>;
    fn redis(&self) -> &RedisClients;
    fn secrets(&self) -> &dyn SecretStore;
    fn queries(&self) -> &QueryRegistry;
}

pub async fn connection(access: &impl Access, id: &str, db: &str) -> Result<RedisConn, AppError> {
    let clients = access.redis();
    let generation = clients.generation(id);
    let conn = access.connection(id)?;
    if conn.engine != Engine::Redis {
        return Err(AppError::internal("Not a Redis connection"));
    }
    let db = if mode(&conn) == RedisMode::Cluster {
        0
    } else {
        parse_database(db)?
    };
    let key = (id.to_string(), db.to_string());
    if let Some(cached) = clients.cache.get(&key, generation) {
        return Ok(cached);
    }
    let password = access.secrets().get(&password_account(id))?;
    let client = open(&conn, password.as_deref(), i64::from(db)).await?;
    clients
        .cache
        .insert(key, generation, client)
        .map(|cached| cached.value)
        .ok_or_else(|| {
            AppError::internal("The connection settings changed while opening it. Try again.")
        })
}
