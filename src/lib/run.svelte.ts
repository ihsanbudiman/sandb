import { api, ApiError, type DocumentExportFormat, type EjsonFlavor, type ExportResult, type QueryResult, type RunSort, type StatementResult } from './api';
import { DEFAULT_LIMIT } from './api/generated/constants';
import { logQuery } from './audit.svelte';
import { afterRun, loadProperties, structureChanged } from './catalog.svelte';
import { canWrite, db, type Conn } from './connections.svelte';
import { confirmDiscard } from './edits';
import { notify } from './notice.svelte';
import { moreRows, toResult, toResults } from './results';
import { ui } from './state.svelte';
import { activeTab, replaceActiveResult, replaceResult, setResults, tabDatabase, type Tab } from './tabs.svelte';
import { displayName, mongoCollection, qualified, tableDataSql, type Danger, type TableRef } from './tableActions';
import { tableIdOf } from './tableId';
import { refreshStatus } from './tx.svelte';
import { classify, type Classification } from './writeCheck';
import { ask } from './ask.svelte';
import MongoWriteDialog from './components/MongoWriteDialog.svelte';
import ExportDocumentsDialog from './components/ExportDocumentsDialog.svelte';
import TableDangerDialog from './components/TableDangerDialog.svelte';

export interface Running {
  id: string;
  started: number;
  /** A file run: `done`/`total` come from the backend while it runs. */
  script?: { name: string; done: number; total: number };
}

export const runs = $state<Record<string, Running>>({});

export const clock = $state({ now: Date.now() });

$effect.root(() => {
  $effect(() => {
    if (!Object.keys(runs).length) return;
    const timer = setInterval(() => (clock.now = Date.now()), 250);
    return () => clearInterval(timer);
  });
});

const connOfTab = (t: Tab): Conn | undefined => db.connections.find((c) => c.id === t.connId) ?? db.connections[0];

/**
 * Runs the selection, or the statement at the cursor (as DBeaver's Ctrl+Enter); App passes the text.
 * `again` re-runs the shown result with a higher limit. With `append` the results open as new result
 * tabs after the current ones; `script` is a file run (the toolbar shows statement progress).
 * `known`: the caller's classification of the same text, so the guard is asked once.
 */
export async function run(pick: { sql: string; script?: string; sort?: RunSort; append?: boolean }, t = activeTab(), known?: Classification): Promise<void> {
  if (!t || runs[t.id] || !confirmDiscard(t.edits)) return;
  const c = connOfTab(t);
  if (!c) return;
  const cls = known ?? (await classify(c.engine, pick.sql, t.database));
  if (cls.empty) return;
  // A production Mongo write asks first, like the table danger dialogs; text the backend could not
  // parse is a write too, so it cannot slip past the guard.
  if (c.engine === 'mongo' && c.env === 'production' && canWrite(c) && cls.write) {
    const info = cls.info ?? { statements: [], warnings: [{ code: 'unparsable' as const, message: 'The text could not be parsed, so it is treated as a write.' }] };
    if (!(await ask<boolean>(MongoWriteDialog, { info, conn: c }))) return;
  }
  await execute(t, c, pick.sql, t.database, cls, pick);
}

/** Re-runs the shown result with a higher limit, the grid's sort, or on Refresh; it never asks for a target. */
export async function rerun(t: Tab, again: { sql: string; limit: number; sort?: RunSort }): Promise<void> {
  if (runs[t.id]) return;
  const c = t.resultConn;
  // A re-run goes to the database the result came from: a Mongo `use` may have changed it.
  const database = t.result?.database || tabDatabase(t, c);
  const cls = await classify(c.engine, again.sql, database);
  if (cls.empty) return;
  await execute(t, c, again.sql, database, cls, { sql: again.sql, sort: again.sort, again });
}

async function execute(
  t: Tab,
  c: Conn,
  sql: string,
  dbName: string,
  cls: Classification,
  opts: { sql: string; script?: string; sort?: RunSort; append?: boolean; again?: { sql: string; limit: number; sort?: RunSort } },
): Promise<void> {
  // A re-run keeps its target: switching or closing result tabs during the await must not move its rows.
  const targetResult = opts.again ? t.result : null;
  const limit = opts.again?.limit;
  const sort = opts.again?.sort ?? opts.sort;
  if (!opts.again) t.sort = opts.sort;
  const id = crypto.randomUUID();
  const started = Date.now();
  runs[t.id] = { id, started, script: opts.script ? { name: opts.script, done: 0, total: 0 } : undefined };
  clock.now = started;
  // A run should show its result, so it opens a hidden grid.
  ui.resultsOpen = true;
  // Polls until the run ends; a poll that fails only leaves the count where it was.
  const poll = opts.script
    ? setInterval(() => {
        api.queryProgress({ queryId: id }).then((p) => {
          const s = runs[t.id]?.id === id ? runs[t.id].script : undefined;
          if (p && s) [s.done, s.total] = [p.done, p.total];
        }, () => {});
      }, 300)
    : undefined;
  let r: QueryResult;
  let all: QueryResult[] | undefined;
  let activeIndex = 0;
  try {
    const queryRun = await api.runQuery({ request: { connId: c.id, database: dbName, sql, queryId: id, limit, tabId: t.id, sort } });
    if (opts.again) {
      r = toResult(queryRun, dbName);
    } else {
      const converted = toResults(queryRun, dbName);
      all = converted.results;
      activeIndex = converted.active;
      r = all[activeIndex] ?? { columns: [], rows: [], ms: queryRun.elapsedMs, note: 'Statement ran.' };
    }
  } catch (e) {
    const err = ApiError.from(e);
    r = { columns: [], rows: [], ms: Date.now() - started, error: err.code === 'cancelled' ? (err.detail ? `Query cancelled. ${err.detail}` : 'Query cancelled') : err.message };
  } finally {
    clearInterval(poll);
    delete runs[t.id];
    void refreshStatus(t);
  }
  if (opts.again) {
    if (targetResult) replaceResult(t, targetResult, r);
    else replaceActiveResult(t, r);
    t.resultConn = c;
    return;
  }
  const fresh = all?.length ? all : [r];
  if (opts.append) setResults(t, [...t.results, ...fresh], t.results.length + (all?.length ? activeIndex : 0));
  else setResults(t, fresh, all?.length ? activeIndex : 0);
  t.resultConn = c;
  // One classification decides the audit kind and the tree refresh; the run's own connection is logged.
  logQuery({
    kind: cls.write ? 'WRITE' : 'READ',
    source: 'You',
    sql: sql.trim(),
    result: r.error ? { status: 'error', message: r.error } : { status: 'ok', rows: r.rows.length },
    ms: r.ms,
    conn: c,
  });
  if (!r.error) afterRun(c, cls, dbName);
}

export function cancel() {
  const t = activeTab();
  if (t) cancelRun(t);
}

/** Stops the run of `t`, if it has one; the server may already have finished. */
export function cancelRun(t: Tab) {
  const r = runs[t.id];
  if (r) api.cancelQuery({ queryId: r.id }).catch(() => {});
}

export function loadTableData(t: Tab) {
  const ref = t.table;
  const c = ref && db.connections.find((x) => x.id === t.connId);
  if (!ref || !c) return;
  // No LIMIT: the run limit pages the rows, so "Load more" can fetch further. A filter value that
  // the read cannot hold as typed stops here, so nothing malformed reaches the server.
  let sql: string;
  try {
    sql = tableDataSql(c.engine, ref, t.filters, t.result?.columns ?? [], t.result?.types ?? []);
  } catch (e) {
    notify(false, e instanceof Error ? e.message : String(e));
    return;
  }
  t.query = sql;
  t.opened = { id: tableIdOf(c.id, ref), sql };
  // The grid's foreign key links come from the catalog, loaded once per table.
  void loadProperties(c.id, ref.database, ref.schema, ref.name);
  void run({ sql, sort: t.sort }, t);
}

/** The sort the grid shows: a table tab keeps it on the tab, any other result on the result itself. */
const sortOf = (t: Tab, r: QueryResult) => (t.table ? t.sort : r.sort);

export function loadMore(t: Tab) {
  const r = t.result;
  if (r?.sql && r.limit && moreRows(r)) void rerun(t, { sql: r.sql, limit: r.limit * 2, sort: sortOf(t, r) });
}

export function sortResult(t: Tab, sort: RunSort) {
  const r = t.result;
  if (!r?.sql || !r.sortable || r.summary || runs[t.id] || !confirmDiscard(t.edits)) return;
  if (t.table) t.sort = sort;
  else r.sort = sort;
  void rerun(t, { sql: r.sql, limit: r.limit ?? DEFAULT_LIMIT, sort });
}

export function refreshResult(t: Tab) {
  const r = t.result;
  if (r?.sql) void rerun(t, { sql: r.sql, limit: r.limit ?? DEFAULT_LIMIT, sort: sortOf(t, r) });
}

export async function countRows(c: Conn, t: TableRef) {
  const sql = `SELECT count(*) FROM ${qualified(c.engine, t)}`;
  try {
    const s = await runPooled(c, t.database, sql, 'READ');
    const n = Number(s?.rows[0]?.[0]);
    notify(true, `${displayName(t)}: ${Number.isFinite(n) ? n.toLocaleString() : '?'} rows`);
  } catch (err) {
    notify(false, ApiError.from(err).message);
  }
}

export async function exportTable(c: Conn, t: TableRef, a: 'exportDdl' | 'exportSql' | 'exportCsv') {
  try {
    const out = a === 'exportCsv' ? await api.exportCsv({ connId: c.id, database: t.database, sql: `SELECT * FROM ${qualified(c.engine, t)}` }) : await api.exportTableSql({ connId: c.id, database: t.database, schema: t.schema, table: t.name, data: a === 'exportSql' });
    if (out.cancelled) return;
    const what = c.engine === 'mongo' ? 'script' : 'DDL';
    if (a === 'exportCsv') notify(true, `Exported ${out.rows} rows to ${out.path}`);
    else notify(true, a === 'exportSql' && t.kind === 'table' ? `Exported the ${what} and ${out.rows} rows to ${out.path}` : `Exported the ${what} to ${out.path}`);
  } catch (err) {
    notify(false, ApiError.from(err).message);
  }
}

export async function exportCollection(c: Conn, t: TableRef) {
  const sql = `${mongoCollection(t.name)}.find({})`;
  const answer = await ask<{ format: DocumentExportFormat; out: ExportResult } | null>(ExportDocumentsDialog, {
    run: (format: DocumentExportFormat, ejson: EjsonFlavor) => (format === 'csv' ? api.exportCsv({ connId: c.id, database: t.database, sql }) : api.exportDocuments({ connId: c.id, database: t.database, sql, format, ejson })),
  });
  if (answer) notify(true, `Exported ${answer.out.rows} ${answer.format === 'csv' ? 'rows' : 'documents'} to ${answer.out.path}`);
}

export function askDanger(d: Danger) {
  return ask<boolean>(TableDangerDialog, { danger: d, run: (sql: string) => runDanger(d, sql) });
}

/**
 * Runs one statement through the pool (no tab, so it commits at once) and logs it; a failure
 * rethrows after its log row, so the caller keeps the error.
 */
async function runPooled(c: Conn, database: string, sql: string, kind: 'READ' | 'WRITE'): Promise<StatementResult | undefined> {
  const started = Date.now();
  try {
    const r = await api.runQuery({ request: { connId: c.id, database, sql, queryId: crypto.randomUUID(), limit: 1 } });
    const s = r.statements[0];
    logQuery({ kind, source: 'You', sql, result: { status: 'ok', rows: (kind === 'WRITE' ? s?.affected : s?.rowCount) ?? 0 }, ms: Date.now() - started, conn: c });
    return s;
  } catch (err) {
    logQuery({ kind, source: 'You', sql, result: { status: 'error', message: ApiError.from(err).message }, ms: Date.now() - started, conn: c });
    throw err;
  }
}

/** Runs one statement of a dialog and logs it; the caller shows the error. */
export async function runOnce(c: Conn, database: string, sql: string, kind: 'READ' | 'WRITE') {
  await runPooled(c, database, sql, kind);
}

// Runs through the pool without a tabId, so it commits at once, also on Production (the dialog asks first).
// Runs the statement the dialog previewed; it throws so the dialog keeps the error.
export async function runDanger(d: Danger, sql: string) {
  await runPooled(d.c, d.t.database, sql, 'WRITE');
  structureChanged(d.c);
  notify(true, `${d.action === 'drop' ? 'Dropped' : 'Truncated'} ${displayName(d.t)}`);
}
