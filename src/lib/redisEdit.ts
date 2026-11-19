import { api, type Cell, type QueryResult, type Row, type TypedValue } from './api';
import { PREVIEW_BYTES } from './api/generated/constants';
import type { Conn } from './connections.svelte';
import type { Edits, Stmt } from './editModel';
import { quoteRedisArg } from './redis/text';
import { isBytes, isCutOff } from './results';

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { ignoreBOM: true });

const REDIS_STRING_ROW = 'A string key has one value. Use Delete key in the tree.';

function redisPk(r: QueryResult, row: Row): Row {
  return Object.fromEntries(r.edit!.pk.map((c) => [c, row[c]]));
}

/** The exact text to compare on save, never an abbreviated or binary preview. */
export function redisOriginal(r: QueryResult, i: number, col: string): string | undefined {
  const v = r.rows[i]?.[col];
  return r.loaded?.[i]?.[col]?.value ?? (typeof v === 'number' || (typeof v === 'string' && !isCutOff(v)) ? String(v) : undefined);
}

function identityProblem(r: QueryResult, i: number): string | null {
  for (const c of r.edit?.pk ?? []) {
    const v = r.rows[i]?.[c];
    if (isBytes(v) || isCutOff(v) || (typeof v !== 'number' && typeof v !== 'string')) {
      return 'Rows with a binary, truncated or missing identity are read-only.';
    }
  }
  return null;
}

function writableColumn(r: QueryResult, col: string): boolean {
  return col !== 'key' && col !== 'index' && r.columns.includes(col);
}

export function redisCellProblem(r: QueryResult, i: number, col: string): string | null {
  if (!writableColumn(r, col)) return 'This Redis column is read-only.';
  if (i < 0) return null;
  const bad = identityProblem(r, i);
  if (bad) return bad;
  if (isBytes(r.rows[i]?.[col])) return 'Binary preview cells are read-only.';
  if (col === 'field' && redisOriginal(r, i, 'value') === undefined) {
    return isCutOff(r.rows[i].value) ? 'Open the full value before renaming this field.' : 'This field cannot be renamed without its original value.';
  }
  return null;
}

export function redisDeleteProblem(r: QueryResult, i: number): string | null {
  if (r.edit?.pk[0] === 'key') return REDIS_STRING_ROW;
  if (i < 0) return null;
  return identityProblem(r, i) ?? (r.columns.some((c) => redisOriginal(r, i, c) === undefined)
    ? 'This row cannot be deleted without its full original value.' : null);
}

/** The server's UTF-8 preview cap backs off to the start of a code point. */
function preview(text: string): string {
  const bytes = encoder.encode(text);
  if (bytes.length <= PREVIEW_BYTES) return text;
  let end = PREVIEW_BYTES;
  while ((bytes[end] & 0xc0) === 0x80) end--;
  return `${decoder.decode(bytes.subarray(0, end))}…`;
}

/** A newly loaded baseline must at least reproduce the text originally shown in the grid. */
export async function loadRedisCell(r: QueryResult, conn: Conn, i: number, col: string, tabId?: string): Promise<string> {
  const text = await api.cellValue({ request: { connId: conn.id, database: r.database ?? conn.database, sql: r.sql!, tabId, pk: redisPk(r, r.rows[i]), column: col } });
  if (text === null || preview(text) !== r.rows[i][col]) {
    throw new Error('The value changed after you loaded it. Reload and try again.');
  }
  return text;
}

function updateColumns(r: QueryResult, set: Row): string[] {
  const columns = Object.keys(set);
  // A field/member rename must also compare the value or score it moves.
  if ('field' in set && !('value' in set)) columns.push('value');
  if ('member' in set && r.columns.includes('score') && !('score' in set)) columns.push('score');
  return columns;
}

export function redisEditProblem(e: Edits, r: QueryResult): Edits['error'] {
  for (const i of Object.keys(e.deleted)) {
    const message = redisDeleteProblem(r, +i);
    if (message) return { key: `r${i}`, message };
  }
  for (const [i, set] of Object.entries(e.changed)) {
    if (e.deleted[+i]) continue;
    for (const c of updateColumns(r, set)) {
      const message = redisCellProblem(r, +i, c)
        ?? (redisOriginal(r, +i, c) === undefined ? 'Open the full original value before saving this change.' : null);
      if (message) return { key: `r${i}`, message };
    }
  }
  for (const { key, row } of e.added) {
    if (r.edit?.pk[0] === 'key') return { key: `a${key}`, message: REDIS_STRING_ROW };
    const missing = r.columns.find((c) => {
      if (!writableColumn(r, c)) return false;
      const v = row[c];
      return typeof v !== 'string' || (c === 'score' && !v.trim());
    });
    if (missing) return { key: `a${key}`, message: `Enter ${missing} for the new row.` };
  }
  return null;
}

/** Readable redis-cli preview per grid operation. The backend saves using one Lua script instead. */
export function redisStatements(e: Edits, r: QueryResult): Stmt[] {
  const table = r.edit?.table;
  if (table === undefined) return [];
  const keyColumn = r.edit!.pk[0];
  const zset = r.columns.includes('score');
  const arg = (v: Cell) => quoteRedisArg(String(v));
  const old = (i: number, columns: string[]): Record<string, TypedValue> => Object.fromEntries(columns.map((c) => [c, { type: 'string', value: redisOriginal(r, i, c)! }]));
  const out: Stmt[] = [];
  for (const i of Object.keys(e.deleted)) {
    const row = r.rows[+i];
    const sql = keyColumn === 'field' ? `HDEL ${table} ${arg(row.field)}`
      : keyColumn === 'index' ? `LREM ${table} 1 ${arg(redisOriginal(r, +i, 'value')!)}`
      : `${zset ? 'ZREM' : 'SREM'} ${table} ${arg(row.member)}`;
    out.push({ kind: 'DELETE', key: `r${i}`, pk: redisPk(r, row), set: {}, old: old(+i, r.columns), sql });
  }
  for (const [i, set] of Object.entries(e.changed)) {
    if (e.deleted[+i]) continue;
    const row = r.rows[+i];
    let sql: string;
    if (keyColumn === 'key') sql = `SET ${table} ${arg(set.value)} KEEPTTL`;
    else if (keyColumn === 'field') {
      sql = `HSET ${table} ${arg(set.field ?? row.field)} ${arg(set.value ?? redisOriginal(r, +i, 'value')!)}`;
      if ('field' in set) sql += `\nHDEL ${table} ${arg(row.field)}`;
    } else if (keyColumn === 'index') sql = `LSET ${table} ${arg(row.index)} ${arg(set.value)}`;
    else if (zset) {
      sql = `ZADD ${table}${'member' in set ? '' : ' XX'} ${arg(set.score ?? redisOriginal(r, +i, 'score')!)} ${arg(set.member ?? row.member)}`;
      if ('member' in set) sql += `\nZREM ${table} ${arg(row.member)}`;
    } else sql = `SADD ${table} ${arg(set.member)}\nSREM ${table} ${arg(row.member)}`;
    out.push({ kind: 'UPDATE', key: `r${i}`, pk: redisPk(r, row), set, old: old(+i, updateColumns(r, set)), sql });
  }
  for (const { key, row } of e.added) {
    const sql = keyColumn === 'field' ? `HSET ${table} ${arg(row.field)} ${arg(row.value)}`
      : keyColumn === 'index' ? `RPUSH ${table} ${arg(row.value)}`
      : zset ? `ZADD ${table} NX ${arg(row.score)} ${arg(row.member)}` : `SADD ${table} ${arg(row.member)}`;
    const set = Object.fromEntries(r.columns.filter((c) => writableColumn(r, c)).map((c) => [c, row[c]]));
    out.push({ kind: 'INSERT', key: `a${key}`, pk: keyColumn === 'index' ? {} : redisPk(r, row), set, sql });
  }
  return out;
}
