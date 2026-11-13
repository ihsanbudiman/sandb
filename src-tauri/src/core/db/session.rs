//! Phase 3b, manual commit on Production: each editor tab on a production connection gets its
//! own dedicated session — one connection, detached from the pool, inside an explicit
//! transaction the user commits or rolls back. The session exists only while that
//! transaction is open: it opens with the first statement of a run that is not a plain read,
//! and closes on Commit / Rollback, when a run ends without pending changes, when the tab
//! closes or its target changes, or when the app exits. There is no timer.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::core::connections::Connection;
use crate::core::db::conn::{CLOSE_TIMEOUT, Conn, TX_TIMEOUT};
use crate::core::db::ctx::Ctx;
use crate::core::error::AppError;
use crate::core::sql::facts::StatementKind;

/// One tab's dedicated session. `dirty` tracks "a statement that could have changed data ran":
/// the pending probes (`pg_current_xact_id_if_assigned`, `innodb_trx`) only see write
/// transactions, so the flag gates them. SQLite asks `sqlite3_txn_state` directly.
pub struct TabSession {
    pub conn_id: String,
    pub database: String,
    pub conn: Conn,
    /// The session is inside the explicit transaction (verified after each statement that
    /// could end it, so MySQL implicit commits and user-typed COMMITs are seen).
    pub tx_open: bool,
    pub dirty: bool,
    /// PG only: a statement failed inside the transaction, which is now aborted (25P02).
    pub aborted: bool,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabStatus {
    pub open: bool,
    pub tx_open: bool,
    pub pending: bool,
    /// PG: the transaction aborted; only ROLLBACK helps.
    pub aborted: bool,
}

enum Slot {
    Live(TabSession),
    /// A command holds the session, or is opening one for `conn_id`: two commands never use
    /// one at once. `closed`: the tab closed or its connection changed meanwhile, so the
    /// session dies when the command hands it back instead of returning to the map.
    InUse {
        conn_id: String,
        closed: bool,
    },
}

#[derive(Default)]
struct Sessions {
    tabs: HashMap<String, Slot>,
}

/// Tab sessions keyed by tab id. A command takes the session out of its slot and leaves an
/// `InUse` marker, so a second run on the same tab fails fast; every take ends in `put` or
/// `discard`, which clear the marker.
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<Sessions>,
}

/// The tab sessions behind the app state. The command layer hands `AppState`, which implements
/// this; `db` cannot import `state` without a cycle, and the intents need only this.
pub trait TabSessions {
    fn tab_sessions(&self) -> &SessionRegistry;
}

fn in_use() -> AppError {
    AppError::invalid("The tab's transaction is in use by another run")
}

impl SessionRegistry {
    fn lock(&self) -> MutexGuard<'_, Sessions> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The tab's session, `None` when it has none, or `InvalidInput` while another command
    /// holds it (e.g. a still-running query on the same tab). A returned session must go
    /// back through `put` or `discard`.
    pub fn take(&self, tab_id: &str) -> Result<Option<TabSession>, AppError> {
        self.take_inner(tab_id, None)
    }

    /// Like `take`, but reserves the slot for a session the caller opens on `conn_id` when
    /// none exists, so two concurrent runs cannot both open one. The reservation ends in
    /// `put` or `discard` too.
    fn take_or_reserve(&self, tab_id: &str, conn_id: &str) -> Result<Option<TabSession>, AppError> {
        self.take_inner(tab_id, Some(conn_id))
    }

    fn take_inner(
        &self,
        tab_id: &str,
        reserve: Option<&str>,
    ) -> Result<Option<TabSession>, AppError> {
        let mut g = self.lock();
        match g.tabs.remove(tab_id) {
            Some(Slot::Live(s)) => {
                let conn_id = reserve.unwrap_or(&s.conn_id).to_string();
                g.tabs.insert(
                    tab_id.to_string(),
                    Slot::InUse {
                        conn_id,
                        closed: false,
                    },
                );
                Ok(Some(s))
            }
            Some(busy) => {
                g.tabs.insert(tab_id.to_string(), busy);
                Err(in_use())
            }
            None => {
                if let Some(conn_id) = reserve {
                    let conn_id = conn_id.to_string();
                    g.tabs.insert(
                        tab_id.to_string(),
                        Slot::InUse {
                            conn_id,
                            closed: false,
                        },
                    );
                }
                Ok(None)
            }
        }
    }

    pub fn has(&self, tab_id: &str) -> bool {
        self.lock().tabs.contains_key(tab_id)
    }

    /// Hands a taken session back. When the tab was closed meanwhile the session is closed
    /// instead; a session the slot somehow still held is closed, never returned to a pool
    /// mid-transaction.
    pub async fn put(&self, tab_id: &str, s: TabSession) {
        let dead = {
            let mut g = self.lock();
            match g.tabs.remove(tab_id) {
                Some(Slot::InUse { closed: false, .. }) => {
                    g.tabs.insert(tab_id.to_string(), Slot::Live(s));
                    None
                }
                Some(Slot::Live(old)) => {
                    g.tabs.insert(tab_id.to_string(), Slot::Live(s));
                    Some(old)
                }
                Some(Slot::InUse { closed: true, .. }) | None => Some(s),
            }
        };
        if let Some(s) = dead {
            close_session(s).await;
        }
    }

    /// Ends a take without handing a session back: clears the `InUse` marker and closes `s`
    /// (its transaction rolls back).
    pub async fn discard(&self, tab_id: &str, s: Option<TabSession>) {
        {
            let mut g = self.lock();
            if matches!(g.tabs.get(tab_id), Some(Slot::InUse { .. })) {
                g.tabs.remove(tab_id);
            }
        }
        if let Some(s) = s {
            close_session(s).await;
        }
    }

    /// Drops the tab's session (its transaction rolls back when the connection dies). A
    /// session in use is marked, so it dies when its command hands it back.
    pub async fn close(&self, tab_id: &str) {
        let s = {
            let mut g = self.lock();
            match g.tabs.get_mut(tab_id) {
                Some(Slot::InUse { closed, .. }) => {
                    *closed = true;
                    None
                }
                Some(Slot::Live(_)) => match g.tabs.remove(tab_id) {
                    Some(Slot::Live(s)) => Some(s),
                    _ => None,
                },
                None => None,
            }
        };
        if let Some(s) = s {
            close_session(s).await;
        }
    }

    /// Drops every session of a saved connection (edit or delete); sessions in use die when
    /// their command hands them back.
    pub async fn close_conn(&self, conn_id: &str) {
        let dead: Vec<TabSession> = {
            let mut g = self.lock();
            let ids: Vec<String> = g
                .tabs
                .iter_mut()
                .filter_map(|(k, v)| match v {
                    Slot::Live(s) if s.conn_id == conn_id => Some(k.clone()),
                    Slot::InUse { conn_id: c, closed } if c == conn_id => {
                        *closed = true;
                        None
                    }
                    _ => None,
                })
                .collect();
            ids.iter()
                .filter_map(|id| match g.tabs.remove(id) {
                    Some(Slot::Live(s)) => Some(s),
                    _ => None,
                })
                .collect()
        };
        for s in dead {
            close_session(s).await;
        }
    }

    /// Tabs with a session holding uncommitted work; the quit intercept reads it. A session
    /// in use by a run counts: the run may be writing right now.
    pub fn pending_tab_ids(&self) -> Vec<String> {
        self.lock()
            .tabs
            .iter()
            .filter(|(_, v)| match v {
                Slot::Live(s) => s.tx_open && (s.dirty || s.aborted),
                Slot::InUse { closed, .. } => !closed,
            })
            .map(|(k, _)| k.clone())
            .collect()
    }
}

pub async fn close_session(s: TabSession) {
    s.conn.close().await;
}

/// Hands the session back after a command. It is kept only while it holds an open
/// transaction on a live connection: without one there is nothing to commit, and the
/// connection is closed rather than held (its SET / USE state must not reach the pool).
async fn settle(ctx: &Ctx<'_>, tab_id: &str, mut s: TabSession) {
    if s.tx_open && s.conn.alive(CLOSE_TIMEOUT).await {
        ctx.sessions.put(tab_id, s).await;
    } else {
        ctx.sessions.discard(tab_id, Some(s)).await;
    }
}

/// The tab's session for (`conn`, `database`), or a new one. A session on a stale target is
/// closed (its transaction rolls back) and replaced. The caller owns the slot until it hands
/// the session back (`finish`, `put`, `discard`).
pub async fn for_tab(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    tab_id: &str,
) -> Result<TabSession, AppError> {
    let old = match ctx.sessions.take_or_reserve(tab_id, &conn.id)? {
        Some(s) if s.conn_id == conn.id && s.database == database => return Ok(s),
        old => old,
    };
    if let Some(s) = old {
        close_session(s).await;
    }
    match open(ctx, conn, database).await {
        Ok(s) => Ok(s),
        Err(e) => {
            ctx.sessions.discard(tab_id, None).await;
            Err(e)
        }
    }
}

/// What to do with a tab session after a read that borrowed it.
pub enum Settle {
    /// Nothing changed: hand it back as it is.
    Keep,
    /// The read may have ended or dirtied the transaction: refresh it, then keep it while a
    /// transaction is open and close it otherwise.
    Refresh,
    /// The connection cannot be trusted (a result was dropped mid-stream): close it, rolling
    /// its transaction back.
    Discard,
}

/// Runs `f` on the tab's session when it is on (`conn.id`, `database`) and returns `None`
/// otherwise, so the caller reads through the pool. A session on another target is put back
/// untouched: a read on another database must not roll back the tab's open transaction.
pub async fn with_tab<T>(
    ctx: &Ctx<'_>,
    conn: &Connection,
    database: &str,
    tab_id: Option<&str>,
    f: impl std::ops::AsyncFnOnce(&mut TabSession) -> (T, Settle),
) -> Result<Option<T>, AppError> {
    let Some(tab_id) = tab_id else {
        return Ok(None);
    };
    let Some(mut s) = ctx.sessions.take(tab_id)? else {
        return Ok(None);
    };
    if s.conn_id != conn.id || s.database != database {
        ctx.sessions.put(tab_id, s).await;
        return Ok(None);
    }
    let (out, how) = f(&mut s).await;
    match how {
        Settle::Keep => ctx.sessions.put(tab_id, s).await,
        Settle::Refresh => {
            refresh(&mut s).await;
            finish(ctx, tab_id, s, false).await;
        }
        Settle::Discard => ctx.sessions.discard(tab_id, Some(s)).await,
    }
    Ok(Some(out))
}

/// A pool connection, detached at once: the session must not count against the pool's
/// connections while the user decides, and it never returns to it.
async fn open(ctx: &Ctx<'_>, conn: &Connection, database: &str) -> Result<TabSession, AppError> {
    let c = Conn::detached(&ctx.pool_for(conn, Some(database))?).await?;
    Ok(TabSession {
        conn_id: conn.id.clone(),
        database: database.to_string(),
        conn: c,
        tx_open: false,
        dirty: false,
        aborted: false,
    })
}

/// Opens the session's transaction when none is open and clears the probe flags for it: the
/// start of every run that may write, so a run and a grid save cannot drift.
pub async fn begin_if_needed(s: &mut TabSession) -> Result<(), AppError> {
    if s.tx_open {
        return Ok(());
    }
    s.conn.begin().await?;
    s.tx_open = true;
    s.dirty = false;
    s.aborted = false;
    Ok(())
}

/// Refreshes `tx_open` / `aborted` / `dirty` after a run. MySQL's probe doubles as the
/// implicit-commit detector; PG's handles the aborted state (25P02 still counts as "in
/// transaction") and the write transaction.
pub async fn refresh(s: &mut TabSession) {
    if let Some(tx) = s.conn.tx(TX_TIMEOUT).await {
        s.dirty = tx.dirty(s.dirty);
        s.tx_open = tx.open;
        s.aborted = tx.aborted;
    }
}

/// Whether the transaction holds changes worth asking about ("pending"), asked of the server
/// where the engine reports it precisely.
pub async fn pending(s: &mut TabSession) -> bool {
    if !s.tx_open || s.aborted {
        return s.tx_open;
    }
    s.conn.has_changes(s.dirty).await
}

/// Ends a transaction that holds nothing worth asking about, once a run is done: a plain
/// read must not leave the tab idle in a transaction the user cannot end (Commit / Rollback
/// are offered only for pending work) on a stale snapshot. ROLLBACK, not COMMIT: nothing is
/// pending, so nothing is lost, and no write ever commits without the user deciding.
async fn end_if_clean(s: &mut TabSession) {
    if s.tx_open && !s.aborted && !pending(s).await {
        // A failed ROLLBACK cannot change the outcome: `settle` closes a session that is not
        // in a transaction, and closing rolls back on the server.
        let _ = s.conn.end_tx(false).await;
        s.tx_open = false;
        s.dirty = false;
        s.aborted = false;
    }
}

/// Hands the session back after a run: a lost connection is dropped (its transaction rolls
/// back on the server, and its SET / USE state must not reach the pool), otherwise a
/// transaction that holds nothing is ended and the session is kept.
pub async fn finish(ctx: &Ctx<'_>, tab_id: &str, mut s: TabSession, lost: bool) {
    if lost {
        ctx.sessions.discard(tab_id, Some(s)).await;
    } else {
        end_if_clean(&mut s).await;
        settle(ctx, tab_id, s).await;
    }
}

pub async fn close_tab(state: &impl TabSessions, tab_id: &str) {
    state.tab_sessions().close(tab_id).await;
}

/// Drops every tab session, e.g. when the user quits and accepted losing uncommitted work.
pub async fn close_all(state: &impl TabSessions) {
    for id in state.tab_sessions().pending_tab_ids() {
        state.tab_sessions().close(&id).await;
    }
}

/// COMMIT (`commit: true`) or ROLLBACK the tab's transaction. The session closes either way:
/// without a transaction it has nothing to keep, and a failed end must take its transaction
/// with it.
pub async fn end_tx_for(
    state: &impl TabSessions,
    tab_id: &str,
    commit: bool,
) -> Result<(), AppError> {
    let registry = state.tab_sessions();
    let mut s = registry
        .take(tab_id)?
        .ok_or_else(|| AppError::not_found("No session for this tab"))?;
    let r = s.conn.end_tx(commit).await.map_err(|e| {
        AppError::new(
            crate::core::error::ErrorCode::Database,
            format!(
                "{} failed; the transaction may still be open",
                if commit { "Commit" } else { "Rollback" }
            ),
        )
        .with_detail(e.message)
    });
    registry.discard(tab_id, Some(s)).await;
    r
}

/// Reports the tab's transaction state. `None` when the tab has no session (a session whose
/// transaction ended or whose connection died is closed here).
pub async fn status(
    state: &impl TabSessions,
    tab_id: &str,
) -> Result<Option<TabStatus>, AppError> {
    let registry = state.tab_sessions();
    let Some(mut s) = registry.take(tab_id)? else {
        return Ok(None);
    };
    if !s.conn.alive(CLOSE_TIMEOUT).await {
        registry.discard(tab_id, Some(s)).await;
        return Ok(None);
    }
    refresh(&mut s).await;
    if !s.tx_open {
        registry.discard(tab_id, Some(s)).await;
        return Ok(None);
    }
    let out = TabStatus {
        open: true,
        tx_open: true,
        pending: pending(&mut s).await,
        aborted: s.aborted,
    };
    registry.put(tab_id, s).await;
    Ok(Some(out))
}

/// The statement may change data. A statement that is not a plain read but parses as a query
/// (`SELECT f()`) may write through the function.
pub fn may_write(kind: Option<StatementKind>) -> bool {
    !matches!(
        kind,
        Some(StatementKind::Show | StatementKind::Explain { analyze: false })
            | Some(StatementKind::Transaction | StatementKind::Pragma)
    )
}

/// The statement may end the open transaction: user transaction control, and on MySQL every
/// statement with an implicit commit (DDL, LOCK, SET autocommit, CALL, GRANT, …). Plain DML
/// and reads never do, so they skip the probe. Deliberately conservative: every `Other`
/// statement is probed, so this is not the user-facing fact; the approval preview uses
/// [`StatementFacts::implicit_commit`](crate::core::sql::facts::StatementFacts::implicit_commit),
/// which names only the statements MySQL really commits at.
pub fn may_end_tx(kind: Option<StatementKind>) -> bool {
    !matches!(
        kind,
        Some(
            StatementKind::Query
                | StatementKind::Show
                | StatementKind::Explain { .. }
                | StatementKind::Insert
                | StatementKind::Update
                | StatementKind::Delete
                | StatementKind::Merge
        )
    )
}

/// The notice when statement `i` of `n` closed the transaction implicitly (MySQL), naming
/// the earlier uncommitted changes it committed along with it (`swept`).
pub fn implicit_commit_notice(
    kind: Option<StatementKind>,
    swept: bool,
    i: usize,
    n: usize,
) -> String {
    let ddl = kind.is_some_and(StatementKind::is_ddl);
    let mut m = if n > 1 {
        format!("Statement {} of {n}: ", i + 1)
    } else {
        String::new()
    };
    m.push_str(if ddl {
        "MySQL committed this DDL immediately"
    } else {
        "MySQL committed the transaction implicitly at this statement"
    });
    if swept {
        m.push_str(", including the uncommitted changes made before it");
    }
    if i + 1 < n {
        m.push_str("; the statements after it run in a new transaction");
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_commit_notice_names_swept_changes() {
        let k = Some(StatementKind::Alter);
        assert_eq!(
            implicit_commit_notice(k, false, 0, 1),
            "MySQL committed this DDL immediately"
        );
        let m = implicit_commit_notice(k, true, 0, 2);
        assert!(m.starts_with("Statement 1 of 2: MySQL committed this DDL immediately"));
        assert!(m.contains("including the uncommitted changes"), "{m}");
        assert!(m.contains("new transaction"), "{m}");
    }

    #[test]
    fn probe_and_dirty_rules() {
        use StatementKind as K;
        assert!(!may_end_tx(Some(K::Insert)));
        assert!(!may_end_tx(Some(K::Query)));
        assert!(may_end_tx(Some(K::Alter)));
        assert!(may_end_tx(Some(K::Transaction)));
        assert!(may_end_tx(Some(K::Other)));
        assert!(may_end_tx(None), "unparsable text is probed");
        assert!(may_write(Some(K::Query)));
        assert!(may_write(None));
        assert!(!may_write(Some(K::Transaction)));
    }

    fn busy(conn_id: &str) -> Slot {
        Slot::InUse {
            conn_id: conn_id.into(),
            closed: false,
        }
    }

    #[tokio::test]
    async fn reservation_blocks_a_second_take_and_discard_clears_it() {
        let reg = SessionRegistry::default();
        assert!(reg.take_or_reserve("t", "c").unwrap().is_none());
        assert!(
            reg.take("t").is_err(),
            "the reserved slot must read as in use"
        );
        assert!(reg.take_or_reserve("t", "c").is_err());
        assert_eq!(reg.pending_tab_ids(), vec!["t".to_string()]);
        reg.discard("t", None).await;
        assert!(!reg.has("t"));
        assert!(reg.take("t").unwrap().is_none());
        assert!(!reg.has("t"), "a plain take reserves nothing");
    }

    #[tokio::test]
    async fn close_marks_in_use_slots() {
        let reg = SessionRegistry::default();
        reg.lock().tabs.insert("a".into(), busy("c1"));
        reg.lock().tabs.insert("b".into(), busy("c2"));
        reg.close("a").await;
        reg.close_conn("c2").await;
        for t in ["a", "b"] {
            assert!(
                matches!(
                    reg.lock().tabs.get(t),
                    Some(Slot::InUse { closed: true, .. })
                ),
                "{t}"
            );
            assert!(reg.take(t).is_err(), "{t}: still in use until handed back");
        }
        assert!(reg.pending_tab_ids().is_empty());
        reg.discard("a", None).await;
        assert!(!reg.has("a"));
    }
}
