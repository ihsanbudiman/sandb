//! What a SQL module needs from the app: the pool cache, the run registry, the tab sessions and
//! the secret store. `db` never imports `state`: `AppState::db()` builds this view and `ops`
//! passes it down, so `state` can compose `db` without a cycle.

use crate::core::connections::{Connection, target_database};
use crate::core::db::pool::{DbPool, PoolCache};
use crate::core::db::session::SessionRegistry;
use crate::core::error::AppError;
use crate::core::query_registry::QueryRegistry;
use crate::core::secrets::{SecretStore, password_account};

pub struct Ctx<'a> {
    pub pools: &'a PoolCache,
    pub queries: &'a QueryRegistry,
    pub sessions: &'a SessionRegistry,
    pub secrets: &'a dyn SecretStore,
}

impl Ctx<'_> {
    /// The cached pool of a saved connection for one database (PG has one pool per database).
    pub fn pool_for(
        &self,
        conn: &Connection,
        database: Option<&str>,
    ) -> Result<DbPool, AppError> {
        // An update or delete between reading the settings and caching the pool bumps the
        // generation; re-read so a pool with stale settings is never cached.
        for _ in 0..3 {
            let generation = self.pools.generation(&conn.id);
            let password = self.password(conn)?;
            let db = target_database(conn, database);
            if let Some(p) = self
                .pools
                .get_or_open(conn, &db, password.as_deref(), generation)?
            {
                return Ok(p);
            }
        }
        Err(AppError::internal(
            "The connection settings changed while opening it. Try again.",
        ))
    }

    pub fn password(&self, conn: &Connection) -> Result<Option<String>, AppError> {
        self.secrets.get(&password_account(&conn.id))
    }
}
