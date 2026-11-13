import { api, type Cell, type QueryResult, type TypedValue } from './api';
import { MONGO_WRITABLE_TYPES } from './api/generated/constants';
import type { Edits, Stmt } from './editModel';
import type { Conn } from './connections.svelte';
import { ejsonText, ejsonWrapper, isCutOff } from './results';
import { INT64_MAX, INT64_MIN, MONGO_NUMERIC, mongoLiteral } from './mongo/value';
import { mongoName, PLAIN_IDENT } from './tableActions';

/**
 * Client-side check of the editor text per writable type; the backend is authoritative. A type
 * without an entry (today only `null`, which the grid sets by clearing the cell) cannot be typed.
 */
const MONGO_VALID: Partial<Record<(typeof MONGO_WRITABLE_TYPES)[number], (s: string) => boolean>> = {
  string: () => true,
  int32: (s) => /^-?\d+$/.test(s) && +s >= -(2 ** 31) && +s < 2 ** 31,
  int64: (s) => /^-?\d+$/.test(s) && BigInt(s) >= INT64_MIN && BigInt(s) <= INT64_MAX,
  double: (s) => MONGO_NUMERIC.test(s) || /^(?:NaN|-?Infinity)$/.test(s),
  decimal128: (s) => MONGO_NUMERIC.test(s) || /^(?:NaN|-?Infinity)$/.test(s),
  bool: (s) => s === 'true' || s === 'false',
  date: (s) => /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/.test(s) && !Number.isNaN(Date.parse(s)),
  objectId: (s) => /^[0-9a-f]{24}$/i.test(s),
  uuid: (s) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s),
};

export const validMongoValue = (type: string, text: string) => MONGO_VALID[type]?.(text) ?? false;

export function mongoColumnType(r: QueryResult | null, column: string): string {
  return r?.types?.[r.columns.indexOf(column)]?.split(' | ').find((t) => t in MONGO_VALID) ?? 'string';
}

export function mongoEditProblem(r: QueryResult | null, idx: number): string | null {
  if (!r || mongoId(r, idx)) return null;
  const ci = r.columns.indexOf('_id');
  return `This document's _id type (${(ci < 0 ? undefined : r.cellTypes?.[idx]?.[ci]) ?? 'missing'}) cannot be edited in the grid`;
}

/** The typed value of a grid cell, or undefined when the cell does not hold the value exactly (documents, arrays, cut-off text, other BSON types). */
function mongoTyped(cell: Cell, type: string): TypedValue | undefined {
  if (type === 'null' || type === 'missing') return { type };
  if (!MONGO_WRITABLE_TYPES.includes(type) || cell === null || typeof cell === 'object') return undefined;
  if (type === 'string' && isCutOff(cell)) return undefined;
  return { type, value: String(cell) };
}

export function mongoOriginal(r: QueryResult, i: number, col: string): TypedValue | undefined {
  return r.loaded?.[i]?.[col] ?? mongoTyped(r.rows[i][col], r.cellTypes?.[i]?.[r.columns.indexOf(col)] ?? 'missing');
}

/**
 * Loads the full value of a cell by its row's `_id` and keeps it on the result as the cell's old value. The map lives on
 * the result, so a new result (re-run, load more) starts without it. A string comes back raw, any other value as
 * mongosh text; no value means the field is gone.
 */
export async function loadMongoCell(r: QueryResult, conn: Conn, i: number, col: string, tabId?: string): Promise<TypedValue> {
  const text = await api.cellValue({ request: { connId: conn.id, database: r.database ?? conn.database, sql: r.sql!, tabId, pk: { _id: mongoId(r, i)! }, column: col } });
  const type = r.cellTypes?.[i]?.[r.columns.indexOf(col)];
  const v: TypedValue = text === null ? { type: 'missing' } : type === 'string' ? { type, value: text } : { type: 'shell', value: text };
  r.loaded ??= {};
  r.loaded[i] = { ...r.loaded[i], [col]: v };
  return v;
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
export function mongoStatements(e: Edits, r: QueryResult): Stmt[] {
  const coll = `db.${mongoName(r.edit?.table ?? '')}`;
  const id = (i: number) => mongoId(r, i) ?? { type: 'missing' };
  const out: Stmt[] = [];
  for (const i of Object.keys(e.deleted)) {
    const _id = id(+i);
    out.push({ kind: 'DELETE', key: 'r' + i, pk: { _id }, set: {}, sql: `${coll}.deleteOne({ _id: ${mongoLiteral(_id)} })` });
  }
  for (const [i, set] of Object.entries(e.changed)) {
    if (e.deleted[+i]) continue;
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
  for (const { key, doc = '{}' } of e.added) {
    out.push({ kind: 'INSERT', key: 'a' + key, pk: {}, set: {}, document: doc, sql: `${coll}.insertOne(${doc})` });
  }
  return out;
}
