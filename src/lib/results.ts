import type { BytesValue, QueryRun, SchemaSnapshot, SnapshotTable, StatementResult } from './api';
import type { Cell, Engine, QueryResult, Schema } from './mock';

export const isBytes = (v: unknown): v is BytesValue => !!v && typeof v === 'object' && '$bytes' in v && 'len' in v;

/** Longest text cell the backend sends (`PREVIEW_BYTES` in `core/db/value.rs`); longer text is cut and ends with `…`. */
const PREVIEW_BYTES = 4096;

/** A text cell the backend cut at the preview limit. The cut backs off at most 3 bytes to a character start and `…` adds 3, so a cut cell has at least `PREVIEW_BYTES` UTF-8 bytes. */
export function isCutOff(v: unknown): v is string {
  return typeof v === 'string' && v.endsWith('…') && new TextEncoder().encode(v).length >= PREVIEW_BYTES;
}

/** Short text for a JSON or bytes cell. */
export function formatCell(v: object): string {
  if (isBytes(v)) {
    const head = atob(v.$bytes.slice(0, 12)).slice(0, 8);
    const hex = [...head].map((c) => c.charCodeAt(0).toString(16).padStart(2, '0')).join('');
    return `\\x${hex}${v.len > 8 ? '…' : ''} (${v.len} B)`;
  }
  const s = JSON.stringify(v);
  return s.length > 200 ? s.slice(0, 199) + '…' : s;
}

// ---- MongoDB: the `documents` of a result arrive as relaxed Extended JSON ----

/** The one `$…` key of a relaxed EJSON wrapper (`{"$oid": …}`), or undefined for a plain object or array. */
export function ejsonWrapper(v: unknown): [string, unknown] | undefined {
  if (v === null || typeof v !== 'object' || Array.isArray(v)) return undefined;
  const keys = Object.keys(v);
  return keys.length === 1 && keys[0].startsWith('$') ? [keys[0], (v as Record<string, unknown>)[keys[0]]] : undefined;
}

/** ISO text of a relaxed EJSON date: an ISO string, or the canonical `{"$numberLong": millis}`. */
function ejsonDate(x: unknown): string {
  if (typeof x === 'string') return x;
  const ms = Number((x as Record<string, unknown> | undefined)?.$numberLong);
  return Number.isFinite(ms) ? new Date(ms).toISOString() : String(x);
}

/** Hyphenated hex of a 16-byte UUID sent as base64, as mongosh shows it; other lengths stay base64. */
function uuidText(base64: string): string {
  let bytes: string;
  try {
    bytes = atob(base64);
  } catch {
    return base64;
  }
  if (bytes.length !== 16) return base64;
  const hex = [...bytes].map((c) => c.charCodeAt(0).toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** mongosh-style one-line text of a relaxed EJSON value: ObjectId("…"), ISODate("…"), 3, [2], {4}, … */
export function ejsonText(v: unknown): string {
  if (v === null) return 'null';
  if (typeof v !== 'object') return typeof v === 'string' ? JSON.stringify(v) : String(v);
  if (Array.isArray(v)) return v.length ? `[${v.length}]` : '[]';
  const wrap = ejsonWrapper(v);
  if (wrap) {
    const [k, x] = wrap;
    switch (k) {
      case '$oid': return `ObjectId("${String(x)}")`;
      case '$date': return `ISODate("${ejsonDate(x)}")`;
      case '$numberDecimal': return `NumberDecimal("${String(x)}")`;
      case '$numberLong': return `NumberLong("${String(x)}")`;
      case '$binary': {
        const b = x as { base64: string; subType: string };
        return b.subType === '04' ? `UUID("${uuidText(b.base64)}")` : `BinData(${parseInt(b.subType, 16)}, "${b.base64}")`;
      }
      case '$regularExpression': {
        const r = x as { pattern: string; options: string };
        return `/${r.pattern}/${r.options}`;
      }
      case '$timestamp': {
        const t = x as { t: unknown; i: unknown };
        return `Timestamp({ t: ${Number(t.t)}, i: ${Number(t.i)} })`;
      }
      case '$minKey': return 'MinKey()';
      case '$maxKey': return 'MaxKey()';
    }
  }
  const keys = Object.keys(v);
  return keys.length ? `{${keys.length}}` : '{}';
}

/** The value as plain JSON for copying: wrappers become their values (ObjectId → its string, …); binary, timestamps and min/max keys have no plain form and stay as they are. */
export function ejsonPlain(v: unknown): unknown {
  if (v === null || typeof v !== 'object') return v;
  if (Array.isArray(v)) return v.map(ejsonPlain);
  const wrap = ejsonWrapper(v);
  if (wrap) {
    const [k, x] = wrap;
    if (k === '$oid' || k === '$numberDecimal' || k === '$numberLong') return x;
    if (k === '$date') return ejsonDate(x);
    if (k === '$regularExpression') {
      const r = x as { pattern: string; options: string };
      return `/${r.pattern}/${r.options}`;
    }
    return v;
  }
  return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, ejsonPlain(x)]));
}

/** Copy text of a value: a string goes out bare, everything else as indented JSON of the unwrapped value. */
export function copyValue(v: unknown): string {
  return typeof v === 'string' ? v : JSON.stringify(ejsonPlain(v), null, 2);
}

/** CSV text of a cell: NULL is empty, bytes are base64 (the loaded preview), JSON is JSON. */
export function csvCell(v: Cell): string {
  const s = v == null ? '' : typeof v === 'object' ? (isBytes(v) ? v.$bytes : JSON.stringify(v)) : String(v);
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

/** Always quoted: names from the tree may be reserved words, mixed case or contain any character. */
export function quoteIdent(engine: Engine, name: string): string {
  return engine === 'mysql' ? '`' + name.replace(/`/g, '``') + '`' : '"' + name.replace(/"/g, '""') + '"';
}

/** Palette and autocomplete name of a snapshot table: PG tables outside `public` are `schema.table`. */
export const snapshotKey = (t: SnapshotTable, engine: Engine) => (engine === 'pg' && t.schema && t.schema !== 'public' ? `${t.schema}.${t.name}` : t.name);

/** Table → columns for autocomplete and the palette, keyed by `snapshotKey`. */
export function snapshotSchema(s: SchemaSnapshot | null, engine: Engine): Schema {
  const out: Schema = {};
  for (const t of s?.tables ?? []) out[snapshotKey(t, engine)] = t.columns.map((c) => c.name);
  return out;
}

const shortSql = (sql: string) => {
  const s = sql.replace(/\s+/g, ' ').trim();
  return s.length > 60 ? s.slice(0, 59) + '…' : s;
};

const outcome = (s: StatementResult) =>
  s.columns.length ? `${s.rowCount}${s.truncated ? '+' : ''} row${s.rowCount === 1 ? '' : 's'}` : `${s.affected ?? 0} affected`;

// Grid rows are keyed by column name, so a repeated name (`SELECT a.id, b.id`) gets a suffix.
function uniqueNames(names: string[]) {
  const seen = new Map<string, number>();
  return names.map((n) => {
    const k = (seen.get(n) ?? 0) + 1;
    seen.set(n, k);
    return k === 1 ? n : `${n} (${k})`;
  });
}

/** One grid result for a statement that returned columns. */
function rowResult(s: StatementResult, mock: boolean, limit: number, database: string): QueryResult {
  const columns = uniqueNames(s.columns.map((c) => c.name));
  const rows = s.rows.map((r) => Object.fromEntries(columns.map((c, i) => [c, r[i] as Cell])));
  return {
    columns,
    types: s.columns.map((c) => c.dataType),
    rows,
    truncated: s.truncated,
    limit,
    isRead: s.isRead,
    sortable: s.isRead && s.sortable,
    table: s.table,
    sql: s.sql,
    ms: s.elapsedMs,
    // A Mongo `use` earlier in the run moves the statement to another database; edits and re-runs go there.
    database: s.database ?? database,
    live: !mock,
    documents: s.documents,
    cellTypes: s.cellTypes,
  };
}

/** Largest row limit of a run, as `MAX_LIMIT` in `core/db/query.rs`. */
const MAX_LIMIT = 100_000;

/**
 * Whether Load more can bring more rows. It runs the statement again with twice the row limit, so it helps only when
 * the rows filled the limit. A MongoDB page that stopped at the 16 MB byte limit gets no new rows from a larger limit.
 */
export function moreRows(r: QueryResult) {
  return r.truncated && r.isRead && !r.summary && !!r.sql && !!r.limit && r.rows.length >= r.limit && r.limit < MAX_LIMIT;
}

/**
 * The result tabs of a run: one per statement that returned columns, plus one Messages tab for
 * statements that changed rows or returned none. The active tab is the last grid result.
 */
export function toResults(run: QueryRun, mock: boolean, limit: number, database: string): { results: QueryResult[]; active: number } {
  const results: QueryResult[] = [];
  const messages: string[] = [];
  let lastRow = -1;
  for (const [i, s] of run.statements.entries()) {
    if (s.columns.length) {
      results.push(rowResult(s, mock, limit, database));
      lastRow = results.length - 1;
    } else {
      messages.push(`${i + 1}. ${shortSql(s.sql)} — ${outcome(s)} · ${s.elapsedMs} ms`);
    }
  }
  if (messages.length) {
    const only = run.statements.length === 1 ? run.statements[0] : undefined;
    const note = only
      ? mock
        ? 'Statement ran (mock). 0 rows affected.'
        : `Statement ran. ${only.affected ?? 0} row${only.affected === 1 ? '' : 's'} affected.`
      : `${run.statements.length} statements ran.`;
    results.push({
      columns: [],
      rows: [],
      ms: run.elapsedMs,
      note,
      title: only ? undefined : 'Messages',
      summary: only ? undefined : messages,
      database,
      live: !mock,
    });
    if (lastRow < 0) lastRow = results.length - 1;
  }
  const notices = run.notices?.length ? run.notices : undefined;
  const last = results.at(-1);
  if (notices && last) last.notices = notices;
  return { results, active: Math.max(0, lastRow) };
}

/** The active result of a run, as a single grid result. */
export function toResult(run: QueryRun, mock: boolean, limit: number, database: string): QueryResult {
  const { results, active } = toResults(run, mock, limit, database);
  return results[active] ?? { columns: [], rows: [], ms: run.elapsedMs, note: 'Statement ran.' };
}
