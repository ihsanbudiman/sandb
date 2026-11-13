//! Process-wide state shared by every command.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::ai::approval::Approvals;
use crate::core::ai::session::ChatRegistry;
use crate::core::connections::Connection;
use crate::core::db::pool::PoolCache;
use crate::core::db::session::SessionRegistry;
use crate::core::error::AppError;
use crate::core::mongo::client::MongoClients;
use crate::core::query_registry::QueryRegistry;
use crate::core::secrets::SecretStore;
use crate::core::store::Store;

pub struct AppState {
    pub(in crate::core) store: Store,
    pub(in crate::core) secrets: Box<dyn SecretStore>,
    /// In-memory copy of `connections.json`; every change is written through to disk.
    pub(in crate::core) connections: Mutex<Vec<Connection>>,
    pub(in crate::core) pools: PoolCache,
    pub(in crate::core) mongo: MongoClients,
    /// Running editor queries, for `cancel_query`.
    pub(in crate::core) queries: QueryRegistry,
    /// Per-tab sessions on production connections (manual commit), keyed by tab id.
    pub(in crate::core) sessions: SessionRegistry,
    /// Running AI chats, for `chat_cancel`.
    pub(in crate::core) chats: ChatRegistry,
    /// Proposed AI writes that wait for Approve or Deny.
    pub(in crate::core) approvals: Approvals,
    /// Unsaved grid edits, reported by the frontend: a window close asks before it drops them.
    pub(in crate::core) unsaved_edits: AtomicBool,
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

    pub fn db(&self) -> crate::core::db::ctx::Ctx<'_> {
        crate::core::db::ctx::Ctx {
            pools: &self.pools,
            queries: &self.queries,
            sessions: &self.sessions,
            secrets: self.secrets.as_ref(),
        }
    }

    pub fn secrets(&self) -> &dyn SecretStore {
        self.secrets.as_ref()
    }

    #[cfg(any(test, feature = "it"))]
    pub fn pools(&self) -> &PoolCache {
        &self.pools
    }

    pub fn queries(&self) -> &QueryRegistry {
        &self.queries
    }

    #[cfg(any(test, feature = "it"))]
    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }

    #[cfg(any(test, feature = "it"))]
    pub fn chats(&self) -> &ChatRegistry {
        &self.chats
    }

    pub fn has_pending_work(&self) -> bool {
        !self.sessions.pending_tab_ids().is_empty()
            || self.unsaved_edits.load(Ordering::Relaxed)
    }

    pub fn set_unsaved_edits(&self, value: bool) {
        self.unsaved_edits.store(value, Ordering::Relaxed);
    }
}

impl crate::core::db::session::TabSessions for AppState {
    fn tab_sessions(&self) -> &SessionRegistry {
        &self.sessions
    }
}

/// Tauri's command layer hands `State<'_, AppState>`; it derefs to the app state, so the
/// command-facing intents need no call-site change.
impl<T> crate::core::db::session::TabSessions for T
where
    T: std::ops::Deref,
    T::Target: crate::core::db::session::TabSessions,
{
    fn tab_sessions(&self) -> &SessionRegistry {
        self.deref().tab_sessions()
    }
}

impl crate::core::mongo::client::Access for AppState {
    fn connection(&self, id: &str) -> Result<Connection, AppError> {
        Self::connection(self, id)
    }

    fn mongo(&self) -> &MongoClients {
        &self.mongo
    }

    fn secrets(&self) -> &dyn SecretStore {
        self.secrets.as_ref()
    }

    fn queries(&self) -> &QueryRegistry {
        &self.queries
    }
}
