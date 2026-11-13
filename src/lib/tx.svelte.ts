import { api, ApiError, errorText } from './api';
import { db, canWrite, type Conn } from './connections.svelte';
import { activeTab, type Tab } from './tabs.svelte';
import { ask } from './ask.svelte';
import PendingTxDialog from './components/PendingTxDialog.svelte';

/** Writable Production connections run in the tab's own session: changes wait for Commit or Rollback. */
export const manualCommit = (c: Conn | undefined): c is Conn => !!c && c.env === 'production' && canWrite(c);

/** A failed read keeps the last known status; it must never break the run that asked for it. */
export async function refreshStatus(t: Tab) {
  if (!manualCommit(db.connections.find((c) => c.id === t.connId))) {
    t.status = null;
    return;
  }
  try {
    t.status = await api.tabStatus({ tabId: t.id });
  } catch {}
}

/** The Commit/Rollback buttons: one call in flight for the active tab, and its error. */
export const txButtons = $state({ busy: false, error: null as { tab: string; text: string } | null });

export async function endActive(commit: boolean) {
  const t = activeTab();
  if (!t) return;
  txButtons.busy = true;
  txButtons.error = null;
  try {
    await endTx(t, commit);
  } catch (e) {
    txButtons.error = { tab: t.id, text: ApiError.from(e).message };
  } finally {
    txButtons.busy = false;
  }
}

/** Throws the backend error; a failed end closes the session on the backend, which the refresh picks up. */
export async function endTx(t: Tab, commit: boolean) {
  try {
    await (commit ? api.commitTransaction({ tabId: t.id }) : api.rollbackTransaction({ tabId: t.id }));
  } finally {
    await refreshStatus(t);
  }
}

/**
 * Drops the session of a tab that moves to another connection or database, so it does not hold a connection.
 * Always asks the backend (a no-op without a session): `status` can be null after a failed refresh.
 */
export function leaveSession(t: Tab) {
  api.closeTabSession({ tabId: t.id }).catch(() => {});
  t.status = null;
}

async function endPending(tabs: Tab[], commit: boolean) {
  for (const t of tabs) {
    if (!t.status?.pending) continue;
    try {
      await endTx(t, commit);
    } catch (e) {
      throw new Error(`${t.title}: ${errorText(e)}`);
    }
  }
}

// The answer to the open question, for a `queue` caller that must wait its turn.
let open: Promise<boolean> | null = null;

/**
 * Resolves `true` when none of `list` holds uncommitted changes, after asking the user (PendingTxDialog)
 * to commit or roll back those that do. `false`: the user cancelled, and the caller must not proceed.
 * One decision covers every tab in `list`. While another question is open the answer is `false`, unless
 * `queue`: then it waits for that question, gives up if it was cancelled, and asks its own afterwards
 * (a quit must not be dropped just because a tab close was asking at the time).
 */
export async function settlePending(list: Tab[], action: string, queue = false): Promise<boolean> {
  for (;;) {
    // The status from the last run can be stale (session dropped, a failed read): ask the backend first.
    await Promise.all(list.map(refreshStatus));
    if (!list.some((t) => t.status?.pending)) return true;
    if (!open) break;
    if (!queue || !(await open)) return false;
  }
  const tabs = list.filter((t) => t.status?.pending);
  const question = ask<boolean>(PendingTxDialog, {
    tabs,
    action,
    connName: (id: string) => db.connections.find((c) => c.id === id)?.name ?? id,
    end: (commit: boolean) => endPending(tabs, commit),
  });
  open = question;
  try {
    return await question;
  } finally {
    open = null;
  }
}
