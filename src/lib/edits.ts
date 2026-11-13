import { api, ApiError, type Cell, type QueryResult, type Row } from './api';
import { logQuery } from './audit.svelte';
import type { Conn } from './connections.svelte';
import { pendingCount, resetEdits, type Edits, type Stmt } from './editModel';
import { mongoStatements } from './mongoEdit';
import { isNumericText, isNumericType, quoteIdent, sqlLiteral } from './sql/value';

let addSeq = 0;

export function confirmDiscard(e: Edits) {
  const n = pendingCount(e);
  if (n === 0) return true;
  if (!confirm(`Discard ${n} unsaved change(s)?`)) return false;
  resetEdits(e, true);
  return true;
}

/** Why the result cannot be edited, or null if it can. The backend's metadata loads on the first edit attempt. */
export function editBlocker(r: QueryResult | null): string | null {
  if (!r || r.error || r.note || !r.sql || r.isRead === false) return 'Run a SELECT to edit its rows';
  return r.editReason ?? null;
}

const editLookups = new WeakMap<QueryResult, Promise<string | null>>();

export function prepareEdit(r: QueryResult, conn: Conn): Promise<string | null> {
  const blocked = editBlocker(r);
  if (blocked || r.edit) return Promise.resolve(blocked);
  const existing = editLookups.get(r);
  if (existing) return existing;
  const lookup = api.editableResult({ request: { connId: conn.id, database: r.database ?? conn.database, sql: r.sql! } })
    .then((meta) => {
      if (meta.reason) r.editReason = meta.reason;
      else r.edit = meta;
      return editBlocker(r);
    })
    .catch((e) => {
      editLookups.delete(r);
      return e instanceof Error ? e.message : String(e);
    });
  editLookups.set(r, lookup);
  return lookup;
}

export function setCell(e: Edits, i: number, orig: Row, col: string, v: Cell) {
  const next = { ...e.changed[i] };
  if (orig[col] === v) delete next[col];
  else next[col] = v;
  if (Object.keys(next).length) e.changed[i] = next;
  else delete e.changed[i];
}

export function addRow(e: Edits, row: Row, doc?: string) {
  e.added.push({ key: ++addSeq, row, doc });
}

export function parseInput(text: string, sample: Cell, dataType = ''): Cell | undefined {
  const numeric = typeof sample === 'number' || isNumericType(dataType);
  if (numeric) {
    if (text.trim() === '') return null;
    if (typeof sample !== 'number') return isNumericText(text.trim()) ? text : undefined;
    const n = Number(text);
    return Number.isFinite(n) ? n : undefined;
  }
  if (typeof sample === 'boolean' || /bool/i.test(dataType)) {
    if (!/^(true|false|1|0)$/i.test(text.trim())) return undefined;
    return /^(true|1)$/i.test(text.trim());
  }
  if (typeof sample === 'object' && sample !== null) {
    try { return JSON.parse(text) as Cell; } catch { return undefined; }
  }
  return text;
}

export function buildStatements(r: QueryResult, conn: Conn, e: Edits): Stmt[] {
  if (conn.engine === 'mongo') return mongoStatements(e, r);
  const meta = r.edit;
  if (!meta?.table) return [];
  const t = meta.table;
  const en = conn.engine;
  const namespace = meta.schema ?? '';
  const table = (namespace ? quoteIdent(en, namespace) + '.' : '') + quoteIdent(en, t);
  const pk = meta.pk;
  const where = (row: Row) => pk.map((k) => `${quoteIdent(en, k)} = ${sqlLiteral(en, row[k])}`).join(' AND ');
  const pkOfRow = (row: Row) => Object.fromEntries(pk.map((k) => [k, row[k]]));
  const out: Stmt[] = [];
  // Deletes run first so an update or insert can reuse a deleted row's primary key.
  for (const i of Object.keys(e.deleted)) {
    const row = r.rows[+i];
    out.push({ kind: 'DELETE', key: 'r' + i, pk: pkOfRow(row), set: {}, sql: `DELETE FROM ${table} WHERE ${where(row)};` });
  }
  for (const [i, set] of Object.entries(e.changed)) {
    if (e.deleted[+i]) continue;
    const row = r.rows[+i];
    const assigns = Object.entries(set).map(([c, v]) => `${quoteIdent(en, c)} = ${sqlLiteral(en, v)}`).join(', ');
    out.push({ kind: 'UPDATE', key: 'r' + i, pk: pkOfRow(row), set, sql: `UPDATE ${table} SET ${assigns} WHERE ${where(row)};` });
  }
  for (const { key, row } of e.added) {
    const cols = r.columns.filter((c) => row[c] !== null);
    out.push({ kind: 'INSERT', key: 'a' + key, pk: pkOfRow(row), set: row, sql: `INSERT INTO ${table} (${cols.map((c) => quoteIdent(en, c)).join(', ')}) VALUES (${cols.map((c) => sqlLiteral(en, row[c])).join(', ')});` });
  }
  return out;
}

/**
 * Saves every operation in one backend transaction and keeps the failed row highlighted.
 * A standalone MongoDB server has no transactions: when it saved some changes before one failed
 * (`partial`), the edits are cleared and true comes back so the caller loads the server state.
 */
export async function saveEdits(r: QueryResult, conn: Conn, e: Edits, tabId?: string, queryId?: string): Promise<boolean> {
  const stmts = buildStatements(r, conn, e);
  if (!stmts.length) return true;
  try {
    const out = await api.applyEdits({
      request: {
        connId: conn.id,
        database: r.database ?? conn.database,
        sql: r.sql!,
        tabId,
        queryId,
        operations: stmts.map((s) => ({
          key: s.key,
          kind: s.kind.toLowerCase() as 'update' | 'insert' | 'delete',
          pk: s.pk,
          set: s.kind === 'INSERT' ? Object.fromEntries(Object.entries(s.set).filter(([, v]) => v !== null)) : s.set,
          old: s.old,
          document: s.document,
        })),
      },
    });
    if (out.failed) {
      const k = out.failed.index;
      const failed = stmts[k];
      if (out.partial) {
        for (const s of stmts.slice(0, k)) logQuery({ kind: 'WRITE', source: 'You', sql: s.sql, result: { status: 'ok', rows: 1 }, ms: 0, conn });
        if (failed) logQuery({ kind: 'WRITE', source: 'You', sql: failed.sql, result: { status: 'error', message: out.failed.message }, ms: 0, conn });
        resetEdits(e, true);
        // Not a row key: the reloaded result has other rows. A non-empty key keeps the message until it is dismissed.
        e.error = { message: `${k} of ${stmts.length} changes saved. Change ${k + 1} failed: ${out.failed.message.replace(/\.$/, '')}. The server has no transactions, so the saved changes stay.`, key: 'partial' };
        return true;
      }
      const noTx = conn.engine === 'mongo' && r.edit?.transactions === false;
      const message = /rolled back/i.test(out.failed.message) ? out.failed.message : `${out.failed.message} ${noTx ? 'Nothing was saved.' : 'Transaction rolled back, nothing was saved.'}`;
      e.error = { message, key: failed?.key ?? stmts[0].key };
      if (failed) logQuery({ kind: 'WRITE', source: 'You', sql: failed.sql, result: { status: 'error', message: out.failed.message }, ms: 0, conn });
      return false;
    }
  } catch (err) {
    // Keep the backend's detail: a cancelled save says there whether it may have applied.
    const detail = err instanceof ApiError ? err.detail : undefined;
    const base = err instanceof Error ? err.message : String(err);
    const message = detail ? `${base}. ${detail}` : base;
    e.error = { message, key: stmts[0].key };
    logQuery({ kind: 'WRITE', source: 'You', sql: stmts[0].sql, result: { status: 'error', message }, ms: 0, conn });
    return false;
  }
  for (const s of stmts) logQuery({ kind: 'WRITE', source: 'You', sql: s.sql, result: { status: 'ok', rows: 1 }, ms: 0, conn });
  resetEdits(e, true);
  return true;
}
