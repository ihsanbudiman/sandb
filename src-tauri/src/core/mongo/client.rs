//! One cached driver client per saved MongoDB connection. A client serves every database of the
//! connection, so the SQL rule "one pool for each database" does not apply here.
//!
//! The cache uses the same generation rule as the SQL pools: a client built from settings read
//! before an update or delete is never handed out.
//!
//! [`Access`] is the slice of the app a MongoDB operation needs, so this module does not import
//! `AppState`; `state.rs` implements the trait for it.

use mongodb::Client;

use crate::core::cache::GenerationCache;
use crate::core::connections::Connection;
use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::mongo::conn::client_options;
use crate::core::query_registry::QueryRegistry;
use crate::core::secrets::password_account;

pub struct MongoClients {
    cache: GenerationCache<String, Client>,
}

impl Default for MongoClients {
    fn default() -> Self {
        Self {
            cache: GenerationCache::with_cap(0),
        }
    }
}

impl MongoClients {
    /// The generation of `id`'s settings; bumped by `close`.
    pub fn generation(&self, id: &str) -> u64 {
        self.cache.generation(id)
    }

    /// Drops the client of `id` and shuts it down in the background: on an `mongodb+srv://`
    /// cluster the shutdown can take up to a minute, and a connection update must not wait for it.
    /// Call after `PoolCache::close`.
    pub async fn close(&self, id: &str) {
        for client in self.cache.close(id) {
            tokio::spawn(client.shutdown().into_future());
        }
    }
}

pub trait Access {
    fn connection(&self, id: &str) -> Result<Connection, AppError>;
    fn mongo(&self) -> &MongoClients;
    fn secrets(&self) -> &dyn crate::core::secrets::SecretStore;
    fn queries(&self) -> &QueryRegistry;
}

/// The cached client of a saved MongoDB connection, built on first use. Building does no I/O:
/// the driver connects on the first command.
pub async fn client(access: &impl Access, id: &str) -> Result<Client, AppError> {
    let generation = access.mongo().generation(id);
    if let Some(client) = access.mongo().cache.get(&id.to_string(), generation) {
        return Ok(client);
    }
    let conn = access.connection(id)?;
    if conn.engine != Engine::Mongo {
        return Err(AppError::internal("Not a MongoDB connection"));
    }
    let password = access.secrets().get(&password_account(id))?;
    // `client_options` resolves `mongodb+srv://` hosts through DNS, so it runs without the lock.
    let client = Client::with_options(client_options(&conn, password.as_deref()).await?)?;
    access
        .mongo()
        .cache
        .insert(id.to_string(), generation, client)
        .map(|cached| cached.value)
        .ok_or_else(|| {
            AppError::internal("The connection settings changed while opening it. Try again.")
        })
}
