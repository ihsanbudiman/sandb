import { untrack } from 'svelte';
import type { QueryResult, RunSort, TabStatus } from './api';
import { db, defaultDb, type Conn } from './connections.svelte';
import { newEdits, pendingCount, resetEdits, type Edits } from './editModel';
import { tableIdOf, tableKey, type TableId } from './tableId';
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
  resultIndex: number;
  resultConn: Conn;
  /** The table opened from the tree or the palette (real backends), and the read that shows it. */
  opened: { id: TableId; sql: string } | null;
  status: TabStatus | null;
  /** Set on a table tab (double-click or ⌘-click in the tree): it shows the table's data or properties, not the editor. */
  table: TableRef | null;
  filters: TableFilter[];
  sort?: RunSort;
  view: 'data' | 'properties';
  /** The saved query this tab edits; ⌘S writes to it. */
  savedId: string | null;
  /** Unsaved grid edits of this tab's shown result; they live and die with the tab. */
  edits: Edits;
}

// A tab's connection and the first tab on the app's first connection while none is loaded.
const placeholder: Conn = { id: '', name: '', engine: 'pg', version: '', env: 'local', host: '', port: 0, database: '', user: '', ssl: 'prefer', file: '', aiWrite: false, readOnly: false, uri: '', authSource: '', topology: null };

export const tabs = $state({ list: [] as Tab[], activeId: '' });

export function activeTab(): Tab | undefined {
  return tabs.list.find((t) => t.id === tabs.activeId) ?? tabs.list[0];
}

export function active(): Conn {
  const id = activeTab()?.connId;
  return db.connections.find((c) => c.id === id) ?? db.connections[0] ?? placeholder;
}

// A tab always points at a connection that exists: when the list loads, or a connection goes, its tabs
// move to the first one. The tab keeps its own database only through `setTabConn`.
$effect.root(() => {
  $effect(() => {
    const first = db.connections[0];
    if (!first) return;
    untrack(() => {
      for (const t of tabs.list) if (!db.connections.some((c) => c.id === t.connId)) setTabConn(t, first);
    });
  });
});

export const tabDatabase = (t: Tab, c: Conn) => t.database || defaultDb(c);

export function setTabConn(t: Tab, c: Conn) {
  t.connId = c.id;
  t.database = defaultDb(c);
  // The results and their edits belong to this connection too; a switch clears them.
  t.resultConn = c;
}

const QUERY_TITLE = /^Query \d+$/;
/** A tab title the app made up (not a name the user or a saved query chose). */
export const isPlaceholderTitle = (title: string) => QUERY_TITLE.test(title);

function nextQueryTitle(): string {
  const used = new Set(tabs.list.map((t) => /^Query (\d+)$/.exec(t.title)?.[1]).filter(Boolean).map(Number));
  let n = 1;
  while (used.has(n)) n++;
  return `Query ${n}`;
}

export const pendingOf = (t: Tab) => pendingCount(t.edits);

export function newTab(query = '', title = nextQueryTitle()): Tab {
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
    edits: newEdits(),
  });
  // The pushed proxy, so callers mutate the stored tab.
  const t = tabs.list[tabs.list.length - 1];
  tabs.activeId = t.id;
  return t;
}

/** A tab on `c` (and `database`, on every engine but SQLite) holding `sql`; nothing runs. */
export function openSqlTab(c: Conn, database: string | null, sql: string): Tab {
  const t = newTab(sql);
  setTabConn(t, c);
  if (c.engine !== 'sqlite' && database) t.database = database;
  return t;
}

function resultChanged(t: Tab, keepOn = false) {
  resetEdits(t.edits, keepOn);
}

export function setResults(t: Tab, results: QueryResult[], active = results.length ? results.length - 1 : 0) {
  resultChanged(t);
  t.results = results;
  t.resultIndex = results.length ? Math.min(Math.max(active, 0), results.length - 1) : 0;
  // Read the stored proxy, not the plain object: re-runs find their target by identity.
  t.result = t.results[t.resultIndex] ?? null;
}

export function replaceResult(t: Tab, target: QueryResult, r: QueryResult) {
  const i = t.results.indexOf(target);
  if (i < 0) return false;
  resultChanged(t);
  // The sort belongs to the result, so the replacement keeps it.
  r.sort = target.sort;
  t.results[i] = r;
  if (t.resultIndex === i) t.result = t.results[i];
  return true;
}

export function replaceActiveResult(t: Tab, r: QueryResult) {
  resultChanged(t);
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
  resultChanged(t, true);
}

export function closeResult(t: Tab, i: number) {
  if (i < 0 || i >= t.results.length) return;
  const idx = t.resultIndex;
  if (i === idx) resultChanged(t, true);
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
  resultChanged(t);
  t.results = [];
  t.resultIndex = 0;
  t.result = null;
}

export function openTableTab(c: Conn, t: TableRef): { tab: Tab; created: boolean } {
  const key = tableKey(tableIdOf(c.id, t));
  const same = tabs.list.find((x) => x.table && x.connId === c.id && tableKey(tableIdOf(x.connId, x.table)) === key);
  if (same) {
    selectTab(same.id);
    return { tab: same, created: false };
  }
  const nt = newTab('', t.name);
  setTabConn(nt, c);
  if (c.engine !== 'sqlite') nt.database = t.database;
  nt.table = t;
  return { tab: nt, created: true };
}

export function selectTab(id: string) {
  if (id === tabs.activeId) return;
  if (!tabs.list.some((t) => t.id === id)) return;
  tabs.activeId = id;
}

/** Closing the last tab leaves a fresh empty "Query 1" on the same connection, so the editor never disappears. */
export function closeTab(id: string) {
  const i = tabs.list.findIndex((t) => t.id === id);
  if (i < 0) return;
  const closed = tabs.list[i];
  const only = tabs.list.length === 1;
  tabs.list.splice(i, 1);
  if (only) {
    const nt = newTab('');
    const c = db.connections.find((x) => x.id === closed.connId);
    if (c) {
      setTabConn(nt, c);
      nt.database = closed.database;
    }
    return;
  }
  if (tabs.activeId !== id) return;
  tabs.activeId = tabs.list[Math.min(i, tabs.list.length - 1)].id;
}
