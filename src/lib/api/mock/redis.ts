import { quoteRedisArg } from '../../redis/text';
import { PREVIEW_BYTES } from '../generated/constants';
import type { CellValue, ColumnInfo, ColumnMeta, Connection, KeyList, RedisKeyInfo, SchemaSnapshot, TableProperties } from '../generated/wire';
import { ApiError } from '../types';
import { redisDatabase } from './uri';

const KEY_LIST_MAX = 1_000;
const FIELD_SAMPLE = 100;
const SCRIPT_MAX_ELEMENTS = 10_000;
const EXPORT_BATCH = 1_000;
const utf8 = new TextEncoder();

type Key = { bytes: Uint8Array; token: string; info: RedisKeyInfo; value?: string | Uint8Array };

function key(name: string | Uint8Array, keyType: string, encoding: string, length: number, memoryBytes: number, ttlMs: number | null = null): Key {
  const bytes = typeof name === 'string' ? utf8.encode(name) : name;
  return { bytes, token: quoteRedisArg(bytes), info: { keyType, encoding, length, memoryBytes, ttlMs } };
}

function stringKey(name: string | Uint8Array, value: string | Uint8Array, encoding: string, memoryBytes: number, ttlMs: number | null = null): Key {
  const length = typeof value === 'string' ? utf8.encode(value).length : value.length;
  return { ...key(name, 'string', encoding, length, memoryBytes, ttlMs), value };
}

function group(name: string, keyType: string, encoding: string, length: number, memoryBytes: number): Key[] {
  return Array.from({ length: 10 }, (_, i) => key(i === 0 ? name : `${name}:${i + 1}`, keyType, encoding, length, memoryBytes));
}

function byBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}

// Ten keys of each type across the two databases. TTL is fixed so the demo is repeatable.
const keyspace: Record<string, Key[]> = {
  '0': [
    stringKey('greeting', 'hello', 'embstr', 64),
    stringKey('counter', '42', 'int', 56),
    stringKey('bin:1', Uint8Array.of(0xff, 0x00, 0xfe), 'embstr', 64),
    stringKey(Uint8Array.of(...utf8.encode('bin:'), 0xff), Uint8Array.of(0xff, 0x00, 0xfe), 'embstr', 64),
    stringKey('ttl:1', 'expires', 'embstr', 64, 3_720_000),
    stringKey('literal*key', 'hello', 'embstr', 64),
    stringKey('space key', 'hello', 'embstr', 64),
    stringKey('seed:done', '1', 'int', 56),
    stringKey('empty:string', '', 'embstr', 72),
    ...Array.from({ length: 9 }, (_, i) => key(`user:${i + 1}`, 'hash', 'listpack', 3, 120)),
    key('big:hash', 'hash', 'hashtable', 100_000, 6_400_000),
    ...group('queue:jobs', 'list', 'quicklist', 50, 640),
    ...group('tags', 'set', 'listpack', 20, 248),
    ...group('leaderboard', 'zset', 'skiplist', 1_000, 105_440),
    ...group('events', 'stream', 'stream', 10_000, 220_160),
  ].sort((a, b) => byBytes(a.bytes, b.bytes)),
  '1': [stringKey('db1:only', 'yes', 'embstr', 64)],
};

const [STAR, ANY, OPEN, CLOSE, NOT, DASH, ESCAPE] = utf8.encode('*?[]^-\\');
type GlobToken = '*' | '?' | number | Uint8Array;

/** Redis MATCH operates on bytes. Backslash escapes the next byte, even inside a class. */
function globTokens(bytes: Uint8Array): GlobToken[] {
  const tokens: GlobToken[] = [];
  for (let i = 0; i < bytes.length; i++) {
    const byte = bytes[i];
    if (byte === STAR) tokens.push('*');
    else if (byte === ANY) tokens.push('?');
    else if (byte === ESCAPE && i + 1 < bytes.length) tokens.push(bytes[++i]);
    else if (byte === OPEN) {
      const allowed = new Uint8Array(256);
      const negated = bytes[i + 1] === NOT;
      i += negated ? 2 : 1;
      for (; i < bytes.length && bytes[i] !== CLOSE; i++) {
        const start = bytes[i];
        if (start === ESCAPE && i + 1 < bytes.length) allowed[bytes[++i]] = 1;
        else if (bytes[i + 1] === DASH && i + 2 < bytes.length) {
          const end = bytes[i + 2];
          allowed.fill(1, Math.min(start, end), Math.max(start, end) + 1);
          i += 2;
        } else allowed[start] = 1;
      }
      tokens.push(negated ? allowed.map((byte) => 1 - byte) : allowed);
    } else tokens.push(byte);
  }
  return tokens;
}

function matches(tokens: GlobToken[], bytes: Uint8Array): boolean {
  let i = 0;
  let j = 0;
  let star = -1;
  let consumed = 0;
  while (j < bytes.length) {
    const token = tokens[i];
    if (token === '*') {
      star = i++;
      consumed = j;
    } else if (token === '?' || token === bytes[j] || (token instanceof Uint8Array && token[bytes[j]] === 1)) {
      i++;
      j++;
    } else if (star >= 0) {
      // Try one more byte at the last star before testing its suffix again.
      i = star + 1;
      j = ++consumed;
    } else return false;
  }
  while (tokens[i] === '*') i++;
  return i === tokens.length;
}

function keysIn(conn: Connection, database: string): Key[] {
  const db = conn.redis?.mode === 'cluster' ? '0' : redisDatabase(database);
  return keyspace[db] ?? [];
}

export function redisDatabases(conn: Connection): string[] {
  if (conn.redis?.mode === 'cluster') return ['0'];
  return [...new Set([...Object.keys(keyspace), conn.database])].sort((a, b) => Number(a) - Number(b));
}

export function redisListKeys(conn: Connection, database: string, pattern: string, limit?: number | null): KeyList {
  const max = Math.min(10_000, Math.max(1, Math.trunc(limit ?? KEY_LIST_MAX)));
  const tokens = pattern === '*' ? null : globTokens(utf8.encode(pattern));
  const keys: KeyList['keys'] = [];
  for (const { bytes, token, info } of keysIn(conn, database)) {
    if (tokens && !matches(tokens, bytes)) continue;
    keys.push({ name: token, kind: 'table', rowEstimate: info.length ?? undefined, redisType: info.keyType });
    // Like SCAN, the list stops at the cap without checking for another match.
    if (keys.length === max) return { keys, truncated: true };
  }
  return { keys, truncated: false };
}

function keyByToken(conn: Connection, database: string, token: string): Key {
  const found = keysIn(conn, database).find((key) => key.token === token);
  if (!found) throw new ApiError({ code: 'not_found', message: `Key ${JSON.stringify(token)} does not exist` });
  return found;
}

const column = (name: string, dataType = 'string', isPk = false): ColumnInfo => ({
  name, dataType, nullable: false, isPk, ...(isPk ? { pkOrdinal: 1 } : {}),
});

function hashEntry(token: string, i: number): [string, string] {
  if (token === 'big:hash') return [`field:${i + 1}`, `value:${i + 1}`];
  const n = Number(token.slice('user:'.length));
  if (i === 0) return ['name', `User ${n}`];
  if (i === 1) return ['email', `user${n}@example.test`];
  return ['age', String(20 + n % 60)];
}

export function redisColumns(conn: Connection, database: string, token: string): ColumnInfo[] {
  const { keyType, length } = keyByToken(conn, database, token).info;
  switch (keyType) {
    case 'hash': {
      const fields = Array.from({ length: Math.min(length ?? 0, FIELD_SAMPLE) }, (_, i) => hashEntry(token, i)[0]);
      return [column('field', 'string', true), column('value'), ...fields.map((name) => ({ ...column(name, 'hash field'), sampleSize: fields.length }))];
    }
    case 'list': return [column('index', 'integer', true), column('value')];
    case 'set': return [column('member', 'string', true)];
    case 'zset': return [column('member', 'string', true), column('score', 'double')];
    case 'stream': return [column('id', 'stream id', true), column('type'), column('amount')];
    default: return [column('value')];
  }
}

export function redisProperties(conn: Connection, database: string, token: string): TableProperties {
  return { indexes: [], constraints: [], foreignKeys: [], references: [], redis: { ...keyByToken(conn, database, token).info } };
}

export function redisSnapshot(conn: Connection, database: string): SchemaSnapshot {
  const { keys, truncated } = redisListKeys(conn, database, '*');
  return { database, truncated, tables: keys.map(({ name, kind }) => ({ name, kind, columns: [] })) };
}

const jobValue = (i: number) => `job:${i + 1}`;
const tagMember = (i: number) => `tag:${i + 1}`;
const playerEntry = (i: number): [number, string] => [i + 1, `player:${i + 1}`];

export function redisScript(conn: Connection, database: string, token: string): string {
  const { value, info: { keyType, length, ttlMs } } = keyByToken(conn, database, token);
  const count = length ?? 0;
  if (count > SCRIPT_MAX_ELEMENTS) {
    throw new ApiError({ code: 'invalid_input', message: `The key has ${count} elements. Use Export script… for big keys.` });
  }

  // The key token already contains the correct quotes and byte escapes.
  const lines = [`# ${keyType} ${token}`, `DEL ${token}`];
  function batches(command: string, entry: (i: number) => string[], size = EXPORT_BATCH) {
    for (let start = 0; start < count; start += size) {
      const args = [command, token];
      const end = Math.min(start + size, count);
      for (let i = start; i < end; i++) args.push(...entry(i).map(quoteRedisArg));
      lines.push(args.join(' '));
    }
  }

  switch (keyType) {
    case 'string': lines.push(`SET ${token} ${quoteRedisArg(value!)}`); break;
    case 'hash': batches('HSET', (i) => hashEntry(token, i)); break;
    case 'list': batches('RPUSH', (i) => [jobValue(i)]); break;
    case 'set': batches('SADD', (i) => [tagMember(i)]); break;
    case 'zset':
      batches('ZADD', (i) => {
        const [score, member] = playerEntry(i);
        return [String(score), member];
      });
      break;
    case 'stream': batches('XADD', (i) => [`${i + 1}-0`, 'type', 'order', 'amount', String((i + 1) % 100)], 1); break;
    default: throw new ApiError({ code: 'invalid_input', message: `Script export does not support type ${keyType}` });
  }
  if (ttlMs !== null) lines.push(`PEXPIRE ${token} ${ttlMs}`);
  return `${lines.join('\n')}\n`;
}

type Shaped = { columns: ColumnMeta[]; rows: CellValue[][]; notice?: string };
const col = (name: string, dataType = 'string'): ColumnMeta => ({ name, dataType });
const decoded = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
const text = new TextDecoder('utf-8', { ignoreBOM: true });

function bulk(bytes: Uint8Array): CellValue {
  try {
    const value = decoded.decode(bytes);
    if (bytes.length <= PREVIEW_BYTES) return value;
    let stop = PREVIEW_BYTES;
    while ((bytes[stop] & 0xc0) === 0x80) stop--;
    return `${decoded.decode(bytes.subarray(0, stop))}…`;
  } catch {
    return { $bytes: btoa(String.fromCharCode(...bytes.subarray(0, PREVIEW_BYTES))), len: bytes.length };
  }
}

function serverError(message: string, code = 'ERR'): never {
  throw new ApiError({ code: 'database', message, detail: `Redis error ${code}` });
}

function arity(args: Uint8Array[], command: string, n: number) {
  if (args.length !== n) serverError(`wrong number of arguments for '${command.toLowerCase()}' command`);
}

function typedKey(key: Key | undefined, type: string) {
  if (key && key.info.keyType !== type) serverError('Operation against a key holding the wrong kind of value', 'WRONGTYPE');
  return key;
}

const U64_MAX = (1n << 64n) - 1n;
const I64_MAX = U64_MAX >> 1n;

function integer(arg: Uint8Array, min = -I64_MAX - 1n, max = I64_MAX): bigint {
  const s = text.decode(arg);
  if (!s || s.length > 20 || /[^0-9-]/.test(s) || !/^(?:0|-?[1-9]\d*)$/.test(s)) serverError('value is not an integer or out of range');
  const n = BigInt(s);
  if (n < min || n > max) serverError('value is not an integer or out of range');
  return n;
}

/** Redis ranks have an inclusive stop and negative numbers are offsets from the end. */
function rankedRows(start: bigint, stop: bigint, length: number, row: (i: number, offset: number) => CellValue[]): CellValue[][] {
  const len = BigInt(length);
  const first = start >= 0n ? start : len + start;
  const last = stop >= 0n ? stop : len + stop;
  const from = Number(first > 0n ? first : 0n);
  const to = Number(last < len ? last : len - 1n);
  return Array.from({ length: Math.max(0, to - from + 1) }, (_, offset) => row(from + offset, offset));
}

function scan(keys: Key[], args: Uint8Array[]): Shaped {
  if (args.length < 2) arity(args, 'SCAN', 2);
  const cursor = integer(args[1], 0n, U64_MAX);
  let count = 10n;
  let pattern: GlobToken[] | undefined;
  let type: string | undefined;
  for (let i = 2; i < args.length; i += 2) {
    if (!args[i + 1]) serverError('syntax error');
    switch (text.decode(args[i]).toUpperCase()) {
      case 'COUNT': count = integer(args[i + 1], 1n); break;
      case 'MATCH': pattern = globTokens(args[i + 1]); break;
      case 'TYPE': type = text.decode(args[i + 1]).toLowerCase(); break;
      default: serverError('syntax error');
    }
  }
  // Stable cursor pages for the demo; as with Redis, MATCH filters after the page is selected.
  const next = cursor + count;
  const rows = keys.slice(Number(cursor), Number(next))
    .filter(({ bytes, info }) => (!pattern || matches(pattern, bytes)) && (!type || type === info.keyType))
    .map(({ bytes }) => [bulk(bytes)]);
  return { columns: [col('key')], rows, ...(next < keys.length ? { notice: `Next cursor: ${next}` } : {}) };
}

/** Shapes the supported demo reads using the same keyspace as the catalog. */
export function redisReply(conn: Connection, database: string, command: string, args: Uint8Array[]): Shaped | undefined {
  const keys = keysIn(conn, database);
  const find = () => {
    let lo = 0, hi = keys.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      const order = byBytes(keys[mid].bytes, args[1]);
      if (order === 0) return keys[mid];
      if (order < 0) lo = mid + 1;
      else hi = mid;
    }
    return undefined;
  };
  switch (command) {
    case 'GET': {
      arity(args, command, 2);
      const value = typedKey(find(), 'string')?.value;
      return { columns: [col('key'), col('value')], rows: [[bulk(args[1]), value instanceof Uint8Array ? bulk(value) : value ?? null]] };
    }
    case 'HGETALL': {
      arity(args, command, 2);
      const key = typedKey(find(), 'hash');
      const rows = key ? Array.from({ length: key.info.length ?? 0 }, (_, i) => hashEntry(key.token, i)) : [];
      return { columns: [col('field'), col('value')], rows };
    }
    case 'LRANGE': {
      arity(args, command, 4);
      const start = integer(args[2]);
      const stop = integer(args[3]);
      const length = typedKey(find(), 'list')?.info.length ?? 0;
      const rows = rankedRows(start, stop, length, (i, offset) => [start >= 0n ? i : offset, jobValue(i)]);
      return { columns: [col('index', 'integer'), col('value')], rows };
    }
    case 'SMEMBERS': {
      arity(args, command, 2);
      const length = typedKey(find(), 'set')?.info.length ?? 0;
      return { columns: [col('member')], rows: Array.from({ length }, (_, i) => [tagMember(i)]) };
    }
    case 'ZRANGE': {
      if (args.length < 4) arity(args, command, 4);
      let reverse = false;
      let scores = false;
      for (const option of args.slice(4)) {
        switch (text.decode(option).toUpperCase()) {
          case 'REV': reverse = true; break;
          case 'WITHSCORES': scores = true; break;
          case 'BYSCORE': case 'BYLEX': case 'LIMIT': throw new ApiError({ code: 'unsupported', message: 'The demo build runs only rank ranges with ZRANGE' });
          default: serverError('syntax error');
        }
      }
      const start = integer(args[2]);
      const stop = integer(args[3]);
      const length = typedKey(find(), 'zset')?.info.length ?? 0;
      const rows = rankedRows(start, stop, length, (i) => {
        const [score, member] = playerEntry(reverse ? length - i - 1 : i);
        return scores ? [member, score] : [member];
      });
      return { columns: scores ? [col('member'), col('score', 'double')] : [col('member')], rows };
    }
    case 'SCAN': return scan(keys, args);
    case 'TYPE':
      arity(args, command, 2);
      return { columns: [col('value')], rows: [[find()?.info.keyType ?? 'none']] };
    case 'DBSIZE':
      arity(args, command, 1);
      return { columns: [col('value', 'integer')], rows: [[keys.length]] };
    default: return undefined;
  }
}
