import { nsOf, pkOf, type Cell, type QueryResult, type Row } from './mock';
import { logQuery } from './audit.svelte';
import { api, ApiError, type TypedValue } from './api';
import { ejsonText, ejsonWrapper, isCutOff } from './results';
import type { Conn } from './state.svelte';
import { mongoName, PLAIN_IDENT } from './tableActions';

/** Pending grid edits. Keys of `changed`/`deleted` are row indexes in the current result. */
export const edits = $state({
  on: false,
  /** The new cell values; on MongoDB each one is a `TypedValue`. */
  changed: {} as Record<number, Row>,
  /** On MongoDB `doc` is the new document as mongosh text; its grid row stays all NULL. */
  added: [] as { key: number; row: Row; doc?: string }[],
  deleted: {} as Record<number, true>,
  error: null as { message: string; key: string } | null,
  /** A save is in flight; tabs cannot close or switch until it ends. */
  saving: false,
});

let addSeq = 0;

export function pendingCount() {
  const del = Object.keys(edits.deleted).length;
  const upd = Object.keys(edits.changed).filter((i) => !edits.deleted[+i]).length;
  return upd + edits.added.length + del;
}

export function resetEdits(keepOn = false) {
  edits.changed = {};
  edits.added = [];
  edits.deleted = {};
  edits.error = null;
  if (!keepOn) edits.on = false;
}

/** Asks before pending edits are thrown away. Returns true if it is fine to continue. */
export function confirmDiscard() {
  if (pendingCount() === 0) return true;
  if (!confirm(`Discard ${pendingCount()} unsaved change(s)?`)) return false;
  resetEdits(true);
  return true;
}

/** Why the result cannot be edited, or null if it can. Live metadata loads on the first edit attempt. */
export function editBlocker(r: QueryResult | null): string | null {
  if (!r || r.error || r.note || !r.sql || r.isRead === false) return 'Run a SELECT to edit its rows';
  if (r.live) return r.editReason ?? null;
  if (!r.table) return 'Run a SELECT to edit its rows';
  const sql = r.sql.replace(/--.*$/gm, '');
  if (/^\s*with\b/i.test(sql) || /\bunion\b/i.test(sql)) return 'Read-only: result is not from a single table';
  if (/\bjoin\b/i.test(sql) || /\bfrom\s+[\w.]+(\s+\w+)?\s*,/i.test(sql)) return 'Read-only: result joins several tables';
  if (/\b(group\s+by|distinct)\b|\b(count|sum|avg|min|max)\s*\(/i.test(sql)) return 'Read-only: result has aggregates';
  const list = sql.match(/select\s+([\s\S]*?)\s+from\s/i)?.[1]?.trim() ?? '';
  if (list !== '*' && !/^\w+(\s*,\s*\w+)*$/.test(list)) return 'Read-only: result has computed columns';
  const cols = list === '*' ? r.columns : list.split(/\s*,\s*/);
  const pk = pkOf(r.table);
  if (!pk.every((k) => cols.includes(k))) return `Read-only: primary key (${pk.join(', ')}) is not in the result`;
  return null;
}

const editLookups = new WeakMap<QueryResult, Promise<string | null>>();

/** Loads and caches the source table and primary key for a live result. */
export function prepareEdit(r: QueryResult, conn: Conn): Promise<string | null> {
  const blocked = editBlocker(r);
  if (blocked || !r.live || r.edit) return Promise.resolve(blocked);
  const existing = editLookups.get(r);
  if (existing) return existing;
  const lookup = api.editableResult({ connId: conn.id, database: r.database ?? conn.database, sql: r.sql! })
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

/** Stores a cell edit; editing a value back to the original clears it. */
export function setCell(i: number, orig: Row, col: string, v: Cell) {
  const next = { ...edits.changed[i] };
  if (orig[col] === v) delete next[col];
  else next[col] = v;
  if (Object.keys(next).length) edits.changed[i] = next;
  else delete edits.changed[i];
}

export function addRow(row: Row, doc?: string) {
  edits.added.push({ key: ++addSeq, row, doc });
}

// ---- MongoDB: cells are typed values ----

/** BSON types the cell editor writes from text, in the order of its type list. */
export const MONGO_SCALARS = ['string', 'int32', 'int64', 'double', 'decimal128', 'bool', 'date', 'objectId', 'uuid', 'null'];

/** The typed value of a grid cell, or undefined when the cell does not hold the value exactly (documents, arrays, cut-off text, other BSON types). */
export function mongoTyped(cell: Cell, type: string): TypedValue | undefined {
  if (type === 'null' || type === 'missing') return { type, value: null };
  if (!MONGO_SCALARS.includes(type) || cell === null || typeof cell === 'object') return undefined;
  if (type === 'string' && isCutOff(cell)) return undefined;
  return { type, value: String(cell) };
}

/** A result cell as it was loaded: the full value from `cellValue` when the grid has it, else the cell itself. */
export function mongoOriginal(r: QueryResult, i: number, col: string): TypedValue | undefined {
  return r.loaded?.[i]?.[col] ?? mongoTyped(r.rows[i][col], r.cellTypes?.[i]?.[r.columns.indexOf(col)] ?? 'missing');
}

/**
 * Loads the full value of a cell by its row's `_id` and keeps it on the result as the cell's old value. The map lives on
 * the result, so a new result (re-run, load more) starts without it. A string comes back raw, any other value as
 * mongosh text; no value means the field is gone.
 */
export async function loadMongoCell(r: QueryResult, conn: Conn, i: number, col: string, tabId?: string): Promise<TypedValue> {
  const text = await api.cellValue({ connId: conn.id, database: r.database ?? conn.database, sql: r.sql!, tabId, pk: { _id: mongoId(r, i)! }, column: col });
  const type = r.cellTypes?.[i]?.[r.columns.indexOf(col)];
  const v: TypedValue = text === null ? { type: 'missing', value: null } : type === 'string' ? { type, value: text } : { type: 'shell', value: text };
  r.loaded ??= {};
  r.loaded[i] = { ...r.loaded[i], [col]: v };
  return v;
}

/** mongosh literal of a typed value; `shell` text goes as it is. */
export function mongoLiteral(v: TypedValue): string {
  const s = v.value ?? '';
  switch (v.type) {
    case 'null':
    case 'missing':
      return 'null';
    case 'string': return JSON.stringify(s);
    case 'int64': return `NumberLong(${JSON.stringify(s)})`;
    case 'decimal128': return `NumberDecimal(${JSON.stringify(s)})`;
    case 'objectId': return `ObjectId(${JSON.stringify(s)})`;
    case 'date': return `ISODate(${JSON.stringify(s)})`;
    case 'uuid': return `UUID(${JSON.stringify(s)})`;
    // The shell reads `42` as an int32; a whole double keeps its `.0`.
    case 'double': return /^-?\d+$/.test(s) ? s + '.0' : s;
    default: return s;
  }
}

const mongoKey = (path: string) => (PLAIN_IDENT.test(path) ? path : JSON.stringify(path));

/** The `_id` of a result row as a typed value: a scalar, or binData as its `BinData(…)` literal from the raw document. Documents, arrays and other types have none, so the grid does not edit those rows. */
export function mongoId(r: QueryResult, i: number): TypedValue | undefined {
  const ci = r.columns.indexOf('_id');
  const t = ci < 0 ? 'missing' : (r.cellTypes?.[i]?.[ci] ?? 'missing');
  if (t === 'binData') {
    // The grid cell holds a byte preview without the subtype; the document has both in full.
    const id = (r.documents?.[i] as Record<string, unknown> | undefined)?._id;
    return ejsonWrapper(id)?.[0] === '$binary' ? { type: 'shell', value: ejsonText(id) } : undefined;
  }
  return t === 'missing' ? undefined : mongoTyped(r.rows[i]._id, t);
}

/** One mongosh command per document: deletes, then updates (with the old values in the filter), then inserts. */
function mongoStatements(r: QueryResult): Stmt[] {
  const coll = `db.${mongoName(r.edit?.table ?? '')}`;
  const id = (i: number) => mongoId(r, i) ?? { type: 'missing', value: null };
  const out: Stmt[] = [];
  for (const i of Object.keys(edits.deleted)) {
    const _id = id(+i);
    out.push({ kind: 'DELETE', key: 'r' + i, pk: { _id }, set: {}, sql: `${coll}.deleteOne({ _id: ${mongoLiteral(_id)} })` });
  }
  for (const [i, set] of Object.entries(edits.changed)) {
    if (edits.deleted[+i]) continue;
    const _id = id(+i);
    const typed = set as Record<string, TypedValue>;
    const old: Record<string, TypedValue> = {};
    for (const p of Object.keys(typed)) {
      const v = mongoOriginal(r, +i, p);
      if (v) old[p] = v;
    }
    // The backend matches a null old value by `$type` and a missing one by `$exists`, not by `field: null`.
    const conds = Object.entries(old).map(([p, v]) => `${mongoKey(p)}: ${v.type === 'null' ? '{ $type: "null" }' : v.type === 'missing' ? '{ $exists: false }' : mongoLiteral(v)}`);
    const filter = [`_id: ${mongoLiteral(_id)}`, ...conds].join(', ');
    const assigns = Object.entries(typed).map(([p, v]) => `${mongoKey(p)}: ${mongoLiteral(v)}`).join(', ');
    out.push({ kind: 'UPDATE', key: 'r' + i, pk: { _id }, set, old, sql: `${coll}.updateOne({ ${filter} }, { $set: { ${assigns} } })` });
  }
  for (const { key, doc = '{}' } of edits.added) {
    out.push({ kind: 'INSERT', key: 'a' + key, pk: {}, set: {}, document: doc, sql: `${coll}.insertOne(${doc})` });
  }
  return out;
}

/** Turns typed text into a value compatible with the column. Returns undefined for invalid numbers, booleans or JSON. */
export function parseInput(text: string, sample: Cell, dataType = ''): Cell | undefined {
  const numeric = typeof sample === 'number' || /^(int|integer|smallint|tinyint|mediumint|bigint|int[248]|serial|bigserial|smallserial|real|double|float|float[48]|decimal|numeric|number|money)\b/i.test(dataType);
  if (numeric) {
    if (text.trim() === '') return null;
    if (typeof sample !== 'number') return /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(text.trim()) ? text : undefined;
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

function lit(v: Cell, engine: Conn['engine']) {
  if (v === null) return 'NULL';
  if (typeof v === 'number') return String(v);
  if (typeof v === 'boolean') return engine === 'pg' ? String(v).toUpperCase() : v ? '1' : '0';
  // Edit mode runs on mock rows only, which hold no JSON or bytes; serialize just in case.
  let s = (typeof v === 'object' ? JSON.stringify(v) : v).replace(/'/g, "''");
  if (engine === 'mysql') s = s.replace(/\\/g, '\\\\');
  return `'${s}'`;
}

function ident(name: string, engine: Conn['engine']) {
  if (/^[a-z_][a-z0-9_]*$/.test(name)) return name;
  return engine === 'mysql' ? `\`${name.replace(/`/g, '``')}\`` : `"${name.replace(/"/g, '""')}"`;
}

export interface Stmt {
  kind: 'UPDATE' | 'INSERT' | 'DELETE';
  /** SQL, or the mongosh command on MongoDB. */
  sql: string;
  /** Grid row key: `r<index>` for result rows, `a<key>` for new rows. */
  key: string;
  pk: Row;
  set: Row;
  /** MongoDB update: the old value of each changed path. */
  old?: Record<string, TypedValue>;
  /** MongoDB insert: the new document as mongosh text. */
  document?: string;
}

export function buildStatements(r: QueryResult, conn: Conn): Stmt[] {
  if (conn.engine === 'mongo') return mongoStatements(r);
  const meta = r.live ? r.edit : undefined;
  const t = meta?.table ?? r.table!;
  const e = conn.engine;
  const namespace = meta?.schema ?? (e === 'pg' ? nsOf(e, t) : '');
  const table = (namespace ? ident(namespace, e) + '.' : '') + ident(t, e);
  const pk = meta?.pk ?? pkOf(t);
  const where = (row: Row) => pk.map((k) => `${ident(k, e)} = ${lit(row[k], e)}`).join(' AND ');
  const pkOfRow = (row: Row) => Object.fromEntries(pk.map((k) => [k, row[k]]));
  const out: Stmt[] = [];
  // Deletes run first so an update or insert can reuse a deleted row's primary key.
  for (const i of Object.keys(edits.deleted)) {
    const row = r.rows[+i];
    out.push({ kind: 'DELETE', key: 'r' + i, pk: pkOfRow(row), set: {}, sql: `DELETE FROM ${table} WHERE ${where(row)};` });
  }
  for (const [i, set] of Object.entries(edits.changed)) {
    if (edits.deleted[+i]) continue;
    const row = r.rows[+i];
    const assigns = Object.entries(set).map(([c, v]) => `${ident(c, e)} = ${lit(v, e)}`).join(', ');
    out.push({ kind: 'UPDATE', key: 'r' + i, pk: pkOfRow(row), set, sql: `UPDATE ${table} SET ${assigns} WHERE ${where(row)};` });
  }
  for (const { key, row } of edits.added) {
    const cols = r.columns.filter((c) => row[c] !== null);
    out.push({ kind: 'INSERT', key: 'a' + key, pk: pkOfRow(row), set: row, sql: `INSERT INTO ${table} (${cols.map((c) => ident(c, e)).join(', ')}) VALUES (${cols.map((c) => lit(row[c], e)).join(', ')});` });
  }
  return out;
}

/**
 * Saves every operation in one backend transaction and keeps the failed row highlighted.
 * A standalone MongoDB server has no transactions: when it saved some changes before one failed
 * (`partial`), the edits are cleared and true comes back so the caller loads the server state.
 */
export async function saveEdits(r: QueryResult, conn: Conn, tabId?: string, queryId?: string): Promise<boolean> {
  const stmts = buildStatements(r, conn);
  if (!stmts.length) return true;
  try {
    const out = await api.applyEdits({
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
    });
    if (out.failed) {
      const k = out.failed.index;
      const failed = stmts[k];
      if (out.partial) {
        for (const s of stmts.slice(0, k)) logQuery({ kind: 'WRITE', source: 'You', sql: s.sql, result: { status: 'ok', rows: 1 }, ms: 0, conn });
        if (failed) logQuery({ kind: 'WRITE', source: 'You', sql: failed.sql, result: { status: 'error', message: out.failed.message }, ms: 0, conn });
        resetEdits(true);
        // Not a row key: the reloaded result has other rows. A non-empty key keeps the message until it is dismissed.
        edits.error = { message: `${k} of ${stmts.length} changes saved. Change ${k + 1} failed: ${out.failed.message.replace(/\.$/, '')}. The server has no transactions, so the saved changes stay.`, key: 'partial' };
        return true;
      }
      const noTx = conn.engine === 'mongo' && r.edit?.transactions === false;
      const message = /rolled back/i.test(out.failed.message) ? out.failed.message : `${out.failed.message} ${noTx ? 'Nothing was saved.' : 'Transaction rolled back, nothing was saved.'}`;
      edits.error = { message, key: failed?.key ?? stmts[0].key };
      if (failed) logQuery({ kind: 'WRITE', source: 'You', sql: failed.sql, result: { status: 'error', message: out.failed.message }, ms: 0, conn });
      return false;
    }
  } catch (e) {
    // Keep the backend's detail: a cancelled save says there whether it may have applied.
    const detail = e instanceof ApiError ? e.detail : undefined;
    const base = e instanceof Error ? e.message : String(e);
    const message = detail ? `${base}. ${detail}` : base;
    edits.error = { message, key: stmts[0].key };
    logQuery({ kind: 'WRITE', source: 'You', sql: stmts[0].sql, result: { status: 'error', message }, ms: 0, conn });
    return false;
  }
  for (const s of stmts) logQuery({ kind: 'WRITE', source: 'You', sql: s.sql, result: { status: 'ok', rows: 1 }, ms: 0, conn });
  resetEdits(true);
  return true;
}
