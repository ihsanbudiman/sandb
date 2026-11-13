//! Runs registered for a query id: statement progress, the cancellation token and the server
//! session that runs them. The registry knows the connection's engine, so `cancel_query` routes
//! to the SQL kill or the MongoDB server kill without either engine owning the registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::result::QueryProgress;

/// How long a cancel is remembered for a query that has not registered yet.
pub const EARLY_CANCEL_TTL: Duration = Duration::from_secs(30);

/// Raw SQLite handle, used only for `sqlite3_interrupt`.
#[derive(Debug, Clone, Copy)]
pub struct SqliteHandle(pub(crate) *mut libsqlite3_sys::sqlite3);

// SAFETY: the pointer is only passed to `sqlite3_interrupt`, which SQLite allows from any thread,
// and the registry clears it (under the same lock `cancel` holds) before the connection is released.
unsafe impl Send for SqliteHandle {}
unsafe impl Sync for SqliteHandle {}

/// The server session running a SQL query, so another connection can stop it.
#[derive(Debug, Clone, Copy)]
pub enum Session {
    Pg(i32),
    MySql(i64),
    Sqlite(SqliteHandle),
}

/// What a cancel needs: the run's connection and its kill handle. MongoDB runs have no SQL kill
/// handle; their operations stop through the flag and the server `$currentOp` kill.
pub struct CancelTarget {
    pub conn_id: String,
    pub database: String,
    pub engine: Engine,
    /// `None` until the run attaches the server session, and after it detaches.
    pub session: Option<Session>,
}

struct Running {
    conn_id: String,
    database: String,
    engine: Engine,
    cancelled: CancellationToken,
    /// Statements the split produced; `query_progress` reports `done` of it.
    total: usize,
    done: Arc<AtomicUsize>,
    session: Option<Session>,
}

#[derive(Default)]
struct Inner {
    running: HashMap<String, Running>,
    /// Cancels that arrived before their run registered, by arrival time.
    early: HashMap<String, Instant>,
}

#[derive(Default)]
pub struct QueryRegistry {
    inner: Mutex<Inner>,
}

impl QueryRegistry {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_running(&self, query_id: &str) -> bool {
        self.lock().running.contains_key(query_id)
    }

    /// The returned guard's `cancelled` is already set when a cancel for `query_id` came first.
    /// `total` is the run's statement count, known before the run registers.
    pub fn register(
        &self,
        query_id: &str,
        conn_id: &str,
        database: &str,
        engine: Engine,
        total: usize,
    ) -> Result<RunGuard<'_>, AppError> {
        let mut inner = self.lock();
        if inner.running.contains_key(query_id) {
            return Err(AppError::invalid(format!(
                "Query {query_id} is already running"
            )));
        }
        let early = inner
            .early
            .remove(query_id)
            .is_some_and(|t| t.elapsed() < EARLY_CANCEL_TTL);
        let cancelled = CancellationToken::new();
        if early {
            cancelled.cancel();
        }
        let done = Arc::new(AtomicUsize::new(0));
        inner.running.insert(
            query_id.to_string(),
            Running {
                conn_id: conn_id.to_string(),
                database: database.to_string(),
                engine,
                cancelled: cancelled.clone(),
                total,
                done: done.clone(),
                session: None,
            },
        );
        Ok(RunGuard {
            reg: self,
            id: query_id.to_string(),
            cancelled,
            done,
        })
    }

    pub fn attach(&self, query_id: &str, session: Session) -> SessionGuard<'_> {
        if let Some(r) = self.lock().running.get_mut(query_id) {
            r.session = Some(session);
        }
        SessionGuard {
            reg: self,
            id: query_id.to_string(),
        }
    }

    pub fn progress(&self, query_id: &str) -> Option<QueryProgress> {
        let inner = self.lock();
        let r = inner.running.get(query_id)?;
        Some(QueryProgress {
            done: r.done.load(Ordering::SeqCst),
            total: r.total,
        })
    }

    pub fn cancel_target(&self, query_id: &str) -> Option<CancelTarget> {
        let mut inner = self.lock();
        let r = inner.running.get_mut(query_id)?;
        r.cancelled.cancel();
        Some(CancelTarget {
            conn_id: r.conn_id.clone(),
            database: r.database.clone(),
            engine: r.engine,
            session: r.session,
        })
    }

    pub fn remember_early_cancel(&self, query_id: &str) {
        let mut inner = self.lock();
        inner.early.retain(|_, t| t.elapsed() < EARLY_CANCEL_TTL);
        inner.early.insert(query_id.to_string(), Instant::now());
    }
}

/// Removes the run from the registry when the run ends or its future is dropped.
pub struct RunGuard<'a> {
    reg: &'a QueryRegistry,
    id: String,
    /// Wakes the moment a cancel for this run arrives; the one cancel mechanism (no polling).
    pub cancelled: CancellationToken,
    pub done: Arc<AtomicUsize>,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.reg.lock().running.remove(&self.id);
    }
}

/// Clears the session so no cancel targets the connection any more. Drop it before reading
/// `cancelled` for the last time: a cancel that saw the session set the flag first.
pub struct SessionGuard<'a> {
    reg: &'a QueryRegistry,
    id: String,
}

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        if let Some(r) = self.reg.lock().running.get_mut(&self.id) {
            r.session = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorCode;

    #[test]
    fn registry_rejects_duplicate_ids_and_cleans_up() {
        let reg = QueryRegistry::default();
        {
            let _g = reg.register("q1", "c", "db", Engine::Pg, 3).unwrap();
            assert!(reg.is_running("q1"));
            assert_eq!(
                reg.register("q1", "c", "db", Engine::Pg, 3)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::InvalidInput
            );
            {
                let _s = reg.attach("q1", Session::Pg(42));
                assert!(matches!(
                    reg.lock().running["q1"].session,
                    Some(Session::Pg(42))
                ));
            }
            assert!(
                reg.lock().running["q1"].session.is_none(),
                "the session is cleared before the connection is released"
            );
        }
        assert!(!reg.is_running("q1"));
    }

    #[test]
    fn cancel_before_register_starts_the_run_cancelled() {
        let reg = QueryRegistry::default();
        reg.lock().early.insert("q1".into(), Instant::now());
        let g = reg.register("q1", "c", "db", Engine::Pg, 3).unwrap();
        assert!(g.cancelled.is_cancelled());
        drop(g);
        let g = reg.register("q1", "c", "db", Engine::Pg, 3).unwrap();
        assert!(
            !g.cancelled.is_cancelled(),
            "an early cancel is used once"
        );
        drop(g);
        reg.lock()
            .early
            .insert("q2".into(), Instant::now() - EARLY_CANCEL_TTL);
        assert!(
            !reg.register("q2", "c", "db", Engine::Pg, 3)
                .unwrap()
                .cancelled
                .is_cancelled()
        );
    }

    #[test]
    fn progress_reports_done_of_total_and_stops_with_the_run() {
        let reg = QueryRegistry::default();
        assert!(reg.progress("q1").is_none(), "unknown id has no progress");
        {
            let g = reg.register("q1", "c", "db", Engine::Pg, 3).unwrap();
            assert_eq!(reg.progress("q1"), Some(QueryProgress { done: 0, total: 3 }));
            g.done.fetch_add(1, Ordering::SeqCst);
            assert_eq!(reg.progress("q1"), Some(QueryProgress { done: 1, total: 3 }));
        }
        assert!(
            reg.progress("q1").is_none(),
            "the entry is removed with the run"
        );
    }

    #[test]
    fn a_cancel_target_carries_the_engine_and_flags_the_run() {
        let reg = QueryRegistry::default();
        let g = reg.register("q1", "c1", "shop", Engine::Mongo, 1).unwrap();
        let target = reg.cancel_target("q1").unwrap();
        assert_eq!(target.conn_id, "c1");
        assert_eq!(target.database, "shop");
        assert_eq!(target.engine, Engine::Mongo);
        assert!(target.session.is_none());
        assert!(g.cancelled.is_cancelled());
        assert!(reg.cancel_target("unknown").is_none());
    }
}
