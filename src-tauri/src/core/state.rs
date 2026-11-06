//! Process-wide state shared by every command.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::core::ai::oauth::OAuthSession;
use crate::core::ai::session::ChatRegistry;
use crate::core::connections::Connection;
use crate::core::db::pool::PoolCache;
use crate::core::db::query::QueryRegistry;
use crate::core::db::session::SessionRegistry;
use crate::core::error::AppError;
use crate::core::secrets::SecretStore;
use crate::core::store::Store;

pub struct AppState {
    pub store: Store,
    pub secrets: Box<dyn SecretStore>,
    /// In-memory copy of `connections.json`; every change is written through to disk.
    pub connections: Mutex<Vec<Connection>>,
    pub pools: PoolCache,
    /// Running editor queries, for `cancel_query`.
    pub queries: QueryRegistry,
    /// Per-tab sessions on production connections (manual commit), keyed by tab id.
    pub sessions: SessionRegistry,
    /// Running AI chats, for `chat_cancel`.
    pub chats: ChatRegistry,
    pub chatgpt: OAuthSession,
}

impl AppState {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        secrets: Box<dyn SecretStore>,
    ) -> Result<Self, AppError> {
        let store = Store::new(data_dir)?;
        let connections = store.load_connections()?;
        Ok(Self {
            store,
            secrets,
            connections: Mutex::new(connections),
            pools: PoolCache::default(),
            queries: QueryRegistry::default(),
            sessions: SessionRegistry::default(),
            chats: ChatRegistry::default(),
            chatgpt: OAuthSession::default(),
        })
    }
}
