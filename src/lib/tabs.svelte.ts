import type { ResultSort, TabStatus } from './api';
import type { QueryResult, Row } from './mock';
import { edits, pendingCount, resetEdits } from './edits.svelte';
import { active, type Conn } from './state.svelte';
import type { TableFilter, TableRef } from './tableActions';

/** An editor tab. `id` is also the backend `tabId` of its session. */
export interface Tab {
  id: string;
  title: string;
  connId: string;
  /** Database the editor runs against; PG switches it when a table of another database opens. */
  database: string;
  query: string;
  result: QueryResult | null;
  /** Results of the last run, one tab per statement that returned columns, plus Messages for writes. */
  results: QueryResult[];
  /** Index of the result shown in the grid. */
  resultIndex: number;
  /** The connection that produced `result`. */
  resultConn: Conn;
  /** Tree key and SQL of the table opened from the tree or the palette (real backends). */
  opened: { key: string; sql: string } | null;
  status: TabStatus | null;
  /** Set on a table tab (double-click or ⌘-click in the tree): it shows the table's data or properties, not the editor. */
  table: TableRef | null;
  /** Structured filters for the table Data view. */
  filters: TableFilter[];
  /** Database-side sort shared by table and query results. */
  sort?: ResultSort;
  view: 'data' | 'properties';
  /** The saved query this tab edits; ⌘S writes to it. */
  savedId: string | null;
  /** Grid edits set aside while another tab is active; they come back with this tab. */
  pending?: PendingEdits;
}

/** Pending grid edits of one tab while another tab is active. */
type PendingEdits = {
  on: boolean;
  /** The result the row indexes belong to; another result makes the edits unusable. */
  result: QueryResult | null;
  changed: Record<number, Row>;
  added: { key: number; row: Row; doc?: string }[];
  deleted: Record<number, true>;
  error: { message: string; key: string } | null;
};

/** Moves the live edit state onto `t`, so the next tab starts with none. */
function parkEdits(t: Tab | undefined) {
  if (!t || (!edits.on && !edits.error && pendingCount() === 0)) return;
  t.pending = { on: edits.on, result: t.result, changed: edits.changed, added: edits.added, deleted: edits.deleted, error: edits.error };
  edits.on = false;
  edits.changed = {};
  edits.added = [];
  edits.deleted = {};
  edits.error = null;
}

/** Brings the edits of `t` back into the live state, if it has any. */
function restoreEdits(t: Tab | undefined) {
  const p = t?.pending;
  if (t) t.pending = undefined;
  // Row indexes belong to one result: another result means the edits no longer point at their rows.
  const stale = !!p && p.result !== (t?.result ?? null);
  edits.on = stale ? false : (p?.on ?? false);
  edits.changed = stale ? {} : (p?.changed ?? {});
  edits.added = stale ? [] : (p?.added ?? []);
  edits.deleted = stale ? {} : (p?.deleted ?? {});
  edits.error = stale ? { message: 'Unsaved grid edits were dropped: the tab has a new result.', key: '' } : (p?.error ?? null);
}

/** Unsaved grid changes a tab holds: its live edits when active, its parked edits otherwise. */
export function pendingOf(t: Tab): number {
  if (t.id === tabs.activeId) return pendingCount();
  const p = t.pending;
  if (!p) return 0;
  const del = Object.keys(p.deleted).length;
  const upd = Object.keys(p.changed).filter((i) => !p.deleted[+i]).length;
  return upd + p.added.length + del;
}

/** A tab's result changes or clears, so its parked edits are gone; the active tab's live edits go too. */
function dropEdits(t: Tab) {
  t.pending = undefined;
  if (t.id === tabs.activeId) resetEdits();
}

// state.svelte.ts imports this module too; neither touches the other at load time, so the cycle is safe.
export const tabs = $state({ list: [] as Tab[], activeId: '' });

export const defaultDb = (c: Conn) => (c.engine === 'sqlite' ? 'main' : c.database);

export function activeTab(): Tab | undefined {
  return tabs.list.find((t) => t.id === tabs.activeId) ?? tabs.list[0];
}

/** Points the tab at another connection, on its default database. */
export function setTabConn(t: Tab, c: Conn) {
  t.connId = c.id;
  t.database = defaultDb(c);
}

/** The lowest N no open tab uses as "Query N". */
function nextQueryTitle(): string {
  const used = new Set(tabs.list.map((t) => /^Query (\d+)$/.exec(t.title)?.[1]).filter(Boolean).map(Number));
  let n = 1;
  while (used.has(n)) n++;
  return `Query ${n}`;
}

/** Opens a tab on the active tab's connection and database, and makes it active. */
export function newTab(query = '', title = nextQueryTitle()): Tab {
  parkEdits(tabs.list.find((t) => t.id === tabs.activeId));
  const c = active();
  const from = activeTab();
  tabs.list.push({
    id: crypto.randomUUID(),
    title,
    connId: from?.connId ?? c.id,
    database: from?.database ?? defaultDb(c),
    query,
    result: null,
    results: [],
    resultIndex: 0,
    resultConn: c,
    opened: null,
    status: null,
    table: null,
    filters: [],
    view: 'data',
    savedId: null,
  });
  // The pushed proxy, so callers mutate the stored tab.
  const t = tabs.list[tabs.list.length - 1];
  tabs.activeId = t.id;
  return t;
}

/** Replaces the result tabs after a run. `active` is the tab to show. */
export function setResults(t: Tab, results: QueryResult[], active = results.length ? results.length - 1 : 0) {
  dropEdits(t);
  t.results = results;
  t.resultIndex = results.length ? Math.min(Math.max(active, 0), results.length - 1) : 0;
  // Read the stored proxy, not the plain object: re-runs find their target by identity.
  t.result = t.results[t.resultIndex] ?? null;
}

/** A re-run of one result (Load more, sort, refresh) updates that tab, not the whole set. */
export function replaceResult(t: Tab, target: QueryResult, r: QueryResult) {
  const i = t.results.indexOf(target);
  if (i < 0) return false;
  dropEdits(t);
  // The sort belongs to the result, so the replacement keeps it.
  r.sort = target.sort;
  t.results[i] = r;
  if (t.resultIndex === i) t.result = t.results[i];
  return true;
}

/** Replaces the result currently shown; used when the caller knows it is still active. */
export function replaceActiveResult(t: Tab, r: QueryResult) {
  dropEdits(t);
  const target = t.results[t.resultIndex];
  if (target) {
    replaceResult(t, target, r);
    return;
  }
  t.results = [r];
  t.resultIndex = 0;
  t.result = t.results[0];
}

export function activateResult(t: Tab, i: number) {
  if (!t.results[i] || t.resultIndex === i) return;
  t.resultIndex = i;
  t.result = t.results[i];
  t.sort = t.result.sort;
}

/** Removes one result tab; the next one at that position becomes active. */
export function closeResult(t: Tab, i: number) {
  if (i < 0 || i >= t.results.length) return;
  const idx = t.resultIndex;
  if (i === idx) dropEdits(t);
  t.results.splice(i, 1);
  if (!t.results.length) {
    t.result = null;
    t.resultIndex = 0;
    return;
  }
  // Closing right of the active tab must not move the cursor to that removed tab.
  t.resultIndex = i < idx ? idx - 1 : i === idx ? Math.min(i, t.results.length - 1) : idx;
  t.result = t.results[t.resultIndex];
  t.sort = t.result.sort;
}

export function clearResults(t: Tab) {
  dropEdits(t);
  t.results = [];
  t.resultIndex = 0;
  t.result = null;
}

/** Opens a table tab on `c`, or selects the tab that already shows this table. Returns the tab and if it is new. */
export function openTableTab(c: Conn, t: TableRef): { tab: Tab; created: boolean } {
  const same = tabs.list.find((x) => x.table && x.connId === c.id && x.table.database === t.database && x.table.schema === t.schema && x.table.name === t.name);
  if (same) {
    selectTab(same.id);
    return { tab: same, created: false };
  }
  const nt = newTab('', t.name);
  nt.connId = c.id;
  nt.database = c.engine === 'sqlite' ? 'main' : t.database;
  nt.resultConn = c;
  nt.table = t;
  return { tab: nt, created: true };
}

export function selectTab(id: string) {
  if (id === tabs.activeId) return;
  const next = tabs.list.find((t) => t.id === id);
  if (!next) return;
  parkEdits(tabs.list.find((t) => t.id === tabs.activeId));
  tabs.activeId = id;
  restoreEdits(next);
}

/** Closing the last tab leaves a fresh empty "Query 1" on the same connection, so the editor never disappears. */
export function closeTab(id: string) {
  const i = tabs.list.findIndex((t) => t.id === id);
  if (i < 0) return;
  if (tabs.list.length === 1) {
    tabs.activeId = id;
    newTab('', 'Query 1');
    tabs.list.splice(i, 1);
    return;
  }
  tabs.list.splice(i, 1);
  if (tabs.activeId !== id) return;
  tabs.activeId = tabs.list[Math.min(i, tabs.list.length - 1)].id;
  restoreEdits(tabs.list.find((t) => t.id === tabs.activeId));
}
