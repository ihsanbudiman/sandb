//! Process-wide state shared by every command.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use crate::core::ai::approval::Approvals;
use crate::core::ai::session::ChatRegistry;
use crate::core::connections::Connection;
use crate::core::db::pool::PoolCache;
use crate::core::db::query::QueryRegistry;
use crate::core::db::session::SessionRegistry;
use crate::core::error::AppError;
use crate::core::mongo::client::MongoClients;
use crate::core::secrets::SecretStore;
use crate::core::store::Store;

pub struct AppState {
    pub store: Store,
    pub secrets: Box<dyn SecretStore>,
    /// In-memory copy of `connections.json`; every change is written through to disk.
    pub connections: Mutex<Vec<Connection>>,
    pub pools: PoolCache,
    /// Cached MongoDB clients, one per connection. Shares the generation of `pools`.
    pub mongo: MongoClients,
    /// Running editor queries, for `cancel_query`.
    pub queries: QueryRegistry,
    /// Per-tab sessions on production connections (manual commit), keyed by tab id.
    pub sessions: SessionRegistry,
    /// Running AI chats, for `chat_cancel`.
    pub chats: ChatRegistry,
    /// Proposed AI writes that wait for Approve or Deny.
    pub approvals: Approvals,
    /// Unsaved grid edits, reported by the frontend: a window close asks before it drops them.
    pub unsaved_edits: AtomicBool,
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
            mongo: MongoClients::default(),
            queries: QueryRegistry::default(),
            sessions: SessionRegistry::default(),
            chats: ChatRegistry::default(),
            approvals: Approvals::default(),
            unsaved_edits: AtomicBool::new(false),
        })
    }
}
