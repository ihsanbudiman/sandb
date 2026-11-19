import { parseRedisStatements, quoteRedisArg } from '../../redis/text';
import { DEFAULT_LIMIT, MAX_LIMIT, REDIS_COMMAND_SPECS } from '../generated/constants';
import type { CellValue, Connection, GuardCheck, QueryRun, RunRequest, WriteInfo } from '../generated/wire';
import { ApiError } from '../types';
import { redisReply } from './redis';
import { redisDatabase } from './uri';

type Supported = 'read' | 'write' | 'flush' | 'admin' | 'script' | 'select' | 'unknown';
type Spec = readonly [name: string, kind: Supported, firstKey: number] | readonly [name: string, kind: 'unsupported', firstKey: number, refusal: string];
type Command = { command: string; firstKey: number } & ({ kind: Supported } | { kind: 'unsupported'; refusal: string });
type Statement = Command & { text: string; args: Uint8Array[] };
const commandSpecs: readonly Spec[] = REDIS_COMMAND_SPECS;
const specs = new Map<string, Spec>(commandSpecs.map((spec) => [spec[0], spec]));
const containers = new Set(commandSpecs.flatMap(([name]) => name.includes(' ') ? [name.split(' ', 1)[0]] : []));
const blocking = 'Blocking commands are not supported.';
const invalid = (message: string) => new ApiError({ code: 'invalid_input', message });
const utf8 = new TextDecoder('utf-8', { ignoreBOM: true });
const encoder = new TextEncoder();

/** Redis truncates option tokens at a NUL byte when it compares them on the server. */
function option(arg: Uint8Array): string {
  const nul = arg.indexOf(0);
  return asciiUpper(nul === -1 ? arg : arg.subarray(0, nul));
}

function asciiUpper(arg: Uint8Array | undefined): string {
  return arg ? utf8.decode(arg).replace(/[a-z]/g, (ch) => ch.toUpperCase()) : '';
}

function commandOf(args: Uint8Array[]): Command {
  const name = asciiUpper(args[0]);
  const container = containers.has(name);
  const spec = container ? specs.get(`${name} ${asciiUpper(args[1])}`) : !name.includes(' ') ? specs.get(name) : undefined;
  if (!spec) {
    const command = args.slice(0, container ? 2 : 1).map((arg) => quoteRedisArg(arg.map((byte) => byte >= 97 && byte <= 122 ? byte - 32 : byte))).join(' ');
    return { command, kind: 'unknown', firstKey: 0 };
  }
  const [command, base, firstKey, refusal] = spec;
  if ((name === 'XREAD' || name === 'XREADGROUP') && args.slice(1).some((arg) => option(arg) === 'BLOCK')) {
    return { command, kind: 'unsupported', firstKey, refusal: blocking };
  }
  if (name === 'SORT' || name === 'GEORADIUS' || name === 'GEORADIUSBYMEMBER') {
    const store = args.slice(1).some((arg) => {
      const flag = option(arg);
      return flag === 'STORE' || flag === 'STOREDIST';
    });
    if (store) return { command, kind: 'write', firstKey };
  }
  return base === 'unsupported' ? { command, kind: base, firstKey, refusal } : { command, kind: base, firstKey };
}

function parse(text: string): Statement[] {
  return parseRedisStatements(text).map((statement) => ({ ...statement, ...commandOf(statement.args) }));
}

function selectedDb(args: Uint8Array[]): string {
  const db = utf8.decode(args[1]);
  if (args.length !== 2 || !db || /[^0-9]/.test(db) || Number(db) > 0xffff_ffff) {
    throw invalid('SELECT needs one database number, for example SELECT 0');
  }
  return String(Number(db));
}

function changedKey({ command, args, kind, firstKey }: Statement): string | undefined {
  let i = firstKey;
  if (kind === 'script') {
    const count = utf8.decode(args[2]);
    if (!count || /[^0-9]/.test(count) || Number(count) < 1 || Number(count) > 0xffff_ffff) return undefined;
  } else if (command === 'XREAD' || command === 'XREADGROUP') {
    i = 0;
    for (let at = 1; at < args.length; at++) {
      const flag = option(args[at]);
      if (flag === 'STREAMS') {
        i = at + 1;
        break;
      }
      if (flag === 'GROUP') at += 2;
      else if (flag === 'COUNT' || flag === 'BLOCK') at++;
    }
  }
  return i > 0 && args[i] ? quoteRedisArg(args[i]) : undefined;
}

export function redisGuardCheck(text: string, origin: 'assistant' | 'schedule' = 'assistant'): GuardCheck {
  let statements: Statement[];
  try {
    statements = parse(text);
  } catch (err) {
    return { allowed: false, reason: (err as Error).message };
  }
  if (statements.length !== 1) return { allowed: false, reason: statements.length ? `Exactly one statement is allowed, found ${statements.length}` : 'The query has no statement' };
  const [s] = statements;
  if (s.command === 'KEYS') return { allowed: false, reason: 'Use SCAN with MATCH and COUNT instead of KEYS.' };
  if (s.kind === 'unsupported') return { allowed: false, reason: s.refusal };
  if (s.kind !== 'read') return { allowed: false, reason: `${s.command} is not a read` };
  if (origin === 'assistant' && ['CONFIG GET', 'ACL LIST', 'CLIENT LIST', 'CLIENT INFO'].includes(s.command)) {
    return { allowed: false, reason: `${s.command} can expose private server details. Use the editor instead.` };
  }
  return { allowed: true, kind: s.command };
}

export function redisClassifyWrite(text: string, database?: string | null): WriteInfo {
  let statements: Statement[];
  try {
    statements = parse(text);
  } catch (err) {
    return { statements: [], warnings: [{ code: 'unparsable', message: (err as Error).message }] };
  }
  const info: WriteInfo = { statements: [], warnings: [] };
  let db = database || '0';
  for (const [statement, s] of statements.entries()) {
    if (s.kind === 'read') continue;
    if (s.kind === 'select') {
      try {
        db = selectedDb(s.args);
      } catch (err) {
        info.warnings.push({ code: 'unparsable', message: (err as Error).message, statement });
      }
      continue;
    }
    const { command, kind } = s;
    const table = changedKey(s);
    info.statements.push({ kind: command, ...(table !== undefined ? { table } : {}), database: db, read: false, structure: kind === 'admin' || kind === 'flush' });
    if (kind === 'flush') {
      info.warnings.push({ code: 'flush', message: command === 'FLUSHDB' ? `FLUSHDB deletes every key of database ${db}` : 'FLUSHALL deletes every key of every database', statement });
    } else if (kind === 'admin') {
      info.warnings.push({ code: 'server_admin', message: `${command} changes the server, not only data`, statement });
    } else if (kind === 'script') {
      info.warnings.push({ code: 'unknown_command', message: `${command} runs a script; sandb cannot tell which keys it changes`, statement });
    } else if (kind === 'unknown') {
      info.warnings.push({ code: 'unknown_command', message: `sandb does not know what ${command} does`, statement });
    }
  }
  return info;
}

const numeric = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i;

function compare(a: CellValue, b: CellValue, asNumber: boolean, ascending: boolean): number {
  if (a == null) return b == null ? 0 : 1;
  if (b == null) return -1;
  const x = typeof a === 'object' && '$bytes' in a ? String(a.$bytes) : String(a);
  const y = typeof b === 'object' && '$bytes' in b ? String(b.$bytes) : String(b);
  const direction = ascending ? 1 : -1;
  if (asNumber) {
    const nx = Number(x), ny = Number(y);
    return nx === ny ? 0 : (nx < ny ? -1 : 1) * direction;
  }
  const xb = encoder.encode(x), yb = encoder.encode(y);
  for (let i = 0, n = Math.min(xb.length, yb.length); i < n; i++) {
    if (xb[i] !== yb[i]) return (xb[i] - yb[i]) * direction;
  }
  return (xb.length - yb.length) * direction;
}

export function redisRun(conn: Connection, request: RunRequest): QueryRun {
  let statements: Statement[];
  try {
    statements = parse(request.sql);
  } catch (err) {
    throw invalid((err as Error).message);
  }
  if (!statements.length) throw invalid('Nothing to run');
  const refused = statements.find((s) => s.kind === 'unsupported');
  if (refused?.kind === 'unsupported') throw new ApiError({ code: 'unsupported', message: refused.refusal });
  if (conn.readOnly) {
    const write = statements.find((s) => s.kind !== 'read' && s.kind !== 'select');
    if (write) throw new ApiError({ code: 'connection_read_only', message: `Connection "${conn.name}" is read-only: ${write.command} is not a read` });
  }
  const sort = request.sort;
  if (sort && (statements.length !== 1 || statements[0].kind !== 'read')) throw invalid('Sort needs exactly one read command');
  const limit = Math.min(MAX_LIMIT, Math.max(1, Math.trunc(request.limit ?? DEFAULT_LIMIT)));
  const cluster = conn.redis?.mode === 'cluster';
  let database = redisDatabase(request.database?.trim() || conn.database);
  if (cluster && database !== '0') throw invalid('Redis Cluster has only database 0');
  const out: QueryRun = { statements: [], elapsedMs: 0 };
  const started = Date.now();
  for (const s of statements) {
    if (s.kind === 'select') {
      database = selectedDb(s.args);
      if (cluster && database !== '0') throw invalid('Redis Cluster has only database 0');
      (out.notices ??= []).push(`Using database ${database} for the rest of this run`);
      continue;
    }
    const start = Date.now();
    const reply = s.kind === 'read' ? redisReply(conn, database, s.command, s.args) : undefined;
    if (!reply) throw new ApiError({ code: 'unsupported', message: 'The demo build runs only read commands on Redis' });
    const { columns, rows, notice } = reply;
    if (sort) {
      const { column, columnIndex, ascending } = sort;
      if (columns[columnIndex]?.name !== column) throw invalid('Sort column does not exist');
      const asNumber = rows.every((row) => row[columnIndex] == null || numeric.test(String(row[columnIndex])));
      rows.sort((a, b) => compare(a[columnIndex], b[columnIndex], asNumber, ascending));
    }
    const truncated = rows.length > limit;
    if (truncated) rows.length = limit;
    out.statements.push({ sql: s.text, columns, rows, rowCount: rows.length, truncated, limit, isRead: true, sortable: true, elapsedMs: Date.now() - start, database });
    if (notice) (out.notices ??= []).push(notice);
  }
  out.elapsedMs = Date.now() - started;
  return out;
}

/** Scheduled reads skip the extra chat restrictions, but still refuse writes. */
export function redisAiRead(conn: Connection, database: string | null, command: string, maxRows: number, origin: 'assistant' | 'schedule') {
  const check = redisGuardCheck(command, origin);
  if (!check.allowed) throw new ApiError({ code: 'guard_rejected', message: `Blocked by the read-only guard: ${check.reason}` });
  const run = redisRun(conn, { connId: conn.id, database, sql: command, queryId: '', limit: maxRows });
  const { columns, rows, truncated, elapsedMs } = run.statements[0];
  const notice = run.notices?.[0];
  return { columns, rows, truncated, elapsedMs, ...(notice ? { notice } : {}) };
}
