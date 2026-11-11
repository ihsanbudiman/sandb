//! One cached driver client per saved MongoDB connection. A client serves every database of the
//! connection, so the SQL rule "one pool for each database" does not apply here.
//!
//! The cache shares the generation of `PoolCache`: an update or delete of the connection bumps it,
//! so a client built from old settings or an old secret is never handed out.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use mongodb::Client;

use crate::core::connections::{self, Engine};
use crate::core::error::AppError;
use crate::core::mongo::conn::client_options;
use crate::core::secrets::password_account;
use crate::core::state::AppState;

#[derive(Default)]
pub struct MongoClients {
    /// Keyed by connection id; the `u64` is the `PoolCache` generation the client was built at.
    inner: Mutex<HashMap<String, (u64, Client)>>,
}

impl MongoClients {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, (u64, Client)>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drops the client of `id` and shuts it down in the background: on an `mongodb+srv://`
    /// cluster the shutdown can take up to a minute, and a connection update must not wait for it.
    /// Call after `PoolCache::close`.
    pub async fn close(&self, id: &str) {
        let old = self.lock().remove(id);
        if let Some((_, c)) = old {
            tokio::spawn(c.shutdown().into_future());
        }
    }
}

/// The cached client of a saved MongoDB connection, built on first use. Building does no I/O:
/// the driver connects on the first command.
pub async fn client(state: &AppState, id: &str) -> Result<Client, AppError> {
    let generation = state.pools.generation(id);
    if let Some((g, c)) = state.mongo.lock().get(id)
        && *g == generation
    {
        return Ok(c.clone());
    }
    let conn = connections::get(state, id)?;
    if conn.engine != Engine::Mongo {
        return Err(AppError::internal("Not a MongoDB connection"));
    }
    let password = state.secrets.get(&password_account(id))?;
    // `client_options` resolves `mongodb+srv://` hosts through DNS, so it runs without the lock.
    let client = Client::with_options(client_options(&conn, password.as_deref()).await?)?;
    let mut map = state.mongo.lock();
    if state.pools.generation(id) != generation {
        return Err(AppError::internal(
            "The connection settings changed while opening it. Try again.",
        ));
    }
    // A parallel call may have cached a client first; keep that one.
    if let Some((g, c)) = map.get(id)
        && *g == generation
    {
        return Ok(c.clone());
    }
    map.insert(id.to_string(), (generation, client.clone()));
    Ok(client)
}
