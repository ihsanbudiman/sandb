import { ApiError } from '../types';
import type { CellValue, Connection, IndexInfo, JsonValue, MongoCollectionStats, RunSort, StatementResult, TableProperties } from '../generated/wire';
import { DEFAULT_LIMIT } from '../generated/constants';
import { mongoCollections, mongoDocs, mongoFlatColumn, mongoSchema, mongoSpecs, type MockIndex } from './data';

/** Key document of an index as mongosh text: plain identifiers stay bare. */
const mongoKeyDoc = (keys: Record<string, 1 | -1 | string>) =>
  `{ ${Object.entries(keys)
    .map(([k, v]) => `${/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(k) ? k : JSON.stringify(k)}: ${typeof v === 'number' ? v : JSON.stringify(v)}`)
    .join(', ')} }`;

const mongoIndexInfo = (coll: string, d: MockIndex): IndexInfo => {
  const options = [d.unique ? 'unique: true' : '', d.sparse ? 'sparse: true' : '', d.hidden ? 'hidden: true' : '', d.ttl !== undefined ? `expireAfterSeconds: ${d.ttl}` : ''].filter(Boolean);
  return {
    name: d.name,
    columns: Object.keys(d.keys),
    unique: !!d.unique,
    primary: d.name === '_id_',
    definition: `db.getCollection(${JSON.stringify(coll)}).createIndex(${mongoKeyDoc(d.keys)}${d.name === '_id_' ? '' : `, { name: ${JSON.stringify(d.name)}${options.length ? `, ${options.join(', ')}` : ''} }`})`,
    mongo: { keys: JSON.stringify(d.keys), sparse: !!d.sparse, hidden: !!d.hidden, ...(d.ttl !== undefined ? { ttlSeconds: d.ttl } : {}) },
  };
};

/** The seed collection and its spec; an unknown name fails like the backend's `not_found`. */
function specOf(database: string, coll: string) {
  const info = (mongoCollections[database] ?? []).find((t) => t.name === coll);
  if (!info) throw new ApiError({ code: 'not_found', message: `Collection ${coll} not found` });
  return { info, spec: mongoSpecs[coll] ?? {} };
}

const prettyJson = (o: unknown) => JSON.stringify(o, null, 2);

/** Collection kind, validator, options and stats, as the backend sends them for MongoDB. */
export function mongoProperties(database: string, coll: string): TableProperties {
  const { info, spec } = specOf(database, coll);
  const view = info.kind !== 'table';
  // The `_id_` index exists on every collection; the mock's indexes leave it out of the views.
  const indexes = view ? [] : (spec.indexes ?? [{ name: '_id_', keys: { _id: 1 } }]).map((d) => mongoIndexInfo(coll, d));
  const count = info.rowEstimate ?? mongoDocs(coll).length;
  const avgObjSize = 168;
  const stats: MongoCollectionStats | undefined = view
    ? undefined
    : { count, size: count * avgObjSize, avgObjSize, storageSize: Math.round(count * avgObjSize * 1.4), totalIndexSize: 24576 * indexes.length, nindexes: indexes.length };
  return {
    indexes,
    constraints: [],
    foreignKeys: [],
    references: [],
    mongo: {
      kind: view ? 'view' : 'collection',
      ...(spec.validator ? { validator: prettyJson(spec.validator), validationLevel: spec.level ?? 'strict', validationAction: spec.action ?? 'error' } : {}),
      options: prettyJson(spec.options ?? {}),
      ...(stats ? { stats } : {}),
    },
  };
}

export function mongoScript(database: string, coll: string): string {
  const { info, spec } = specOf(database, coll);
  const name = JSON.stringify(coll);
  if (info.kind !== 'table') {
    const o = spec.options ?? {};
    return `db.createView(${name}, ${JSON.stringify(o.viewOn)}, ${prettyJson(o.pipeline ?? [])});`;
  }
  const options = {
    ...spec.options,
    ...(spec.validator ? { validator: spec.validator, validationLevel: spec.level ?? 'strict', validationAction: spec.action ?? 'error' } : {}),
  };
  const lines = [`db.createCollection(${name}, ${prettyJson(options)});`];
  for (const d of spec.indexes ?? []) {
    if (d.name === '_id_') continue;
    const extra = [d.unique ? 'unique: true' : '', d.sparse ? 'sparse: true' : '', d.hidden ? 'hidden: true' : '', d.ttl !== undefined ? `expireAfterSeconds: ${d.ttl}` : ''].filter(Boolean);
    lines.push(`db.getCollection(${name}).createIndex(${mongoKeyDoc(d.keys)}, { name: ${JSON.stringify(d.name)}${extra.length ? `, ${extra.join(', ')}` : ''} });`);
  }
  return lines.join('\n\n');
}

function lookupPath(doc: Record<string, unknown>, path: string): unknown {
  let cur: unknown = doc;
  for (const seg of path.split('.')) {
    if (cur === null || typeof cur !== 'object') return undefined;
    cur = (cur as Record<string, unknown>)[seg];
  }
  return cur;
}

/** Every value at a dotted path; a segment on an array tests each element, as MongoDB matches any of them. */
function pathValues(value: unknown, segs: string[]): unknown[] {
  if (!segs.length) return [value];
  if (Array.isArray(value)) return value.flatMap((el) => pathValues(el, segs));
  if (value === null || typeof value !== 'object') return [];
  const head = segs[0];
  if (!Object.prototype.hasOwnProperty.call(value, head)) return [];
  return pathValues((value as Record<string, unknown>)[head], segs.slice(1));
}

function mongoCell(e: unknown): { v: CellValue; t: string } {
  if (e === undefined) return { v: null, t: 'missing' };
  if (e === null) return { v: null, t: 'null' };
  if (Array.isArray(e)) return { v: e.map((x) => mongoCell(x).v), t: 'array' };
  if (typeof e === 'object') {
    const o = e as Record<string, unknown>;
    if (typeof o.$oid === 'string') return { v: o.$oid, t: 'objectId' };
    if (typeof o.$date === 'string') return { v: o.$date, t: 'date' };
    if (typeof o.$numberDecimal === 'string') return { v: o.$numberDecimal, t: 'decimal128' };
    if (typeof o.$numberLong === 'string') return { v: o.$numberLong, t: 'int64' };
    const out: Record<string, CellValue> = {};
    for (const [k, x] of Object.entries(o)) out[k] = mongoCell(x).v;
    return { v: out, t: 'object' };
  }
  if (typeof e === 'number') return { v: e, t: Number.isInteger(e) ? 'int32' : 'double' };
  if (typeof e === 'boolean') return { v: e, t: 'bool' };
  return { v: e as string, t: 'string' };
}

type MongoLiteral =
  | { t: 'num'; v: number }
  | { t: 'str' | 'date' | 'oid' | 'dec' | 'long'; v: string }
  | { t: 'bool'; v: boolean }
  | { t: 'null' }
  | { t: 'regex'; v: RegExp }
  | { t: 'arr'; v: MongoLiteral[] }
  | { t: 'doc'; v: Record<string, MongoLiteral> };

const findFail = (why: string) => new ApiError({ code: 'invalid_input', message: `The mock backend cannot read this find(): ${why}` });

const skipWs = (s: string, i: number) => {
  while (i < s.length && /\s/.test(s[i])) i++;
  return i;
};

function strAt(s: string, i: number): [string, number] {
  const quote = s[i];
  let out = '';
  let j = i + 1;
  while (j < s.length) {
    const c = s[j];
    if (c === quote) return [out, j + 1];
    if (c === '\\') {
      const n = s[j + 1];
      out += n === 'n' ? '\n' : n === 't' ? '\t' : n === 'r' ? '\r' : n;
      j += 2;
      continue;
    }
    out += c;
    j++;
  }
  throw findFail('unterminated string');
}

function litAt(s: string, i: number): [MongoLiteral, number] {
  i = skipWs(s, i);
  const c = s[i];
  if (c === '{') {
    const out: Record<string, MongoLiteral> = {};
    i = skipWs(s, i + 1);
    while (s[i] !== '}') {
      if (i >= s.length) throw findFail('unterminated document');
      let key: string;
      if (s[i] === '"' || s[i] === "'") [key, i] = strAt(s, i);
      else {
        const m = /^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*/.exec(s.slice(i));
        if (!m) throw findFail(`bad field name at "${s.slice(i, i + 16)}"`);
        key = m[0];
        i += key.length;
      }
      i = skipWs(s, i);
      if (s[i] !== ':') throw findFail(`expected ':' after "${key}"`);
      const [v, j] = litAt(s, i + 1);
      out[key] = v;
      i = skipWs(s, j);
      if (s[i] === ',') i = skipWs(s, i + 1);
    }
    return [{ t: 'doc', v: out }, i + 1];
  }
  if (c === '[') {
    const out: MongoLiteral[] = [];
    i = skipWs(s, i + 1);
    while (s[i] !== ']') {
      if (i >= s.length) throw findFail('unterminated array');
      const [v, j] = litAt(s, i);
      out.push(v);
      i = skipWs(s, j);
      if (s[i] === ',') i = skipWs(s, i + 1);
    }
    return [{ t: 'arr', v: out }, i + 1];
  }
  if (c === '"' || c === "'") {
    const [v, j] = strAt(s, i);
    return [{ t: 'str', v }, j];
  }
  const rest = s.slice(i);
  if (rest[0] === '/') {
    let j = i + 1;
    let cls = false;
    while (j < s.length) {
      if (s[j] === '\\') j += 2;
      else if (s[j] === '[') { cls = true; j++; }
      else if (s[j] === ']') { cls = false; j++; }
      else if (s[j] === '/' && !cls) break;
      else j++;
    }
    const flags = /^[a-z]*/.exec(s.slice(j + 1))![0];
    return [{ t: 'regex', v: new RegExp(s.slice(i + 1, j), flags) }, j + 1 + flags.length];
  }
  const typed = /^(ObjectId|ISODate|NumberLong|NumberDecimal|UUID)\s*\(\s*("[^"]*"|'[^']*')\s*\)/.exec(rest);
  if (typed) {
    const kind = { ObjectId: 'oid', ISODate: 'date', NumberLong: 'long', NumberDecimal: 'dec', UUID: 'str' }[typed[1] as 'ObjectId' | 'ISODate' | 'NumberLong' | 'NumberDecimal' | 'UUID'];
    const [v, j] = strAt(typed[2], 0);
    return [{ t: kind, v } as MongoLiteral, i + typed[0].length];
  }
  const word = /^(true|false|null)\b/.exec(rest);
  if (word) return [word[1] === 'null' ? { t: 'null' } : { t: 'bool', v: word[1] === 'true' }, i + word[1].length];
  const num = /^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(rest);
  if (num) return [{ t: 'num', v: Number(num[0]) }, i + num[0].length];
  throw findFail(`unsupported value "${rest.slice(0, 16)}"`);
}

function fromDoc(v: unknown): MongoLiteral | undefined {
  if (v === undefined) return undefined;
  if (v === null) return { t: 'null' };
  if (typeof v === 'number') return { t: 'num', v };
  if (typeof v === 'string') return { t: 'str', v };
  if (typeof v === 'boolean') return { t: 'bool', v };
  if (Array.isArray(v)) return { t: 'arr', v: v.map((x) => fromDoc(x)!) };
  const o = v as Record<string, unknown>;
  if (typeof o.$oid === 'string') return { t: 'oid', v: o.$oid };
  if (typeof o.$date === 'string') return { t: 'date', v: o.$date };
  if (typeof o.$numberDecimal === 'string') return { t: 'dec', v: o.$numberDecimal };
  if (typeof o.$numberLong === 'string') return { t: 'long', v: o.$numberLong };
  return { t: 'doc', v: Object.fromEntries(Object.entries(o).map(([k, x]) => [k, fromDoc(x)!])) };
}

const numberOf = (v: MongoLiteral) => (v.t === 'num' ? v.v : v.t === 'dec' || v.t === 'long' ? Number(v.v) : NaN);

function equalTo(a: MongoLiteral | undefined, b: MongoLiteral): boolean {
  if (a === undefined) return false;
  if (a.t === 'null' && b.t === 'null') return true;
  const an = numberOf(a);
  const bn = numberOf(b);
  if (!Number.isNaN(an) && !Number.isNaN(bn)) return an === bn;
  if (a.t === 'arr' && b.t === 'arr') return a.v.length === b.v.length && a.v.every((x, i) => equalTo(x, b.v[i]));
  if (a.t === 'doc' && b.t === 'doc') {
    const ak = Object.keys(a.v);
    const bk = Object.keys(b.v);
    return ak.length === bk.length && ak.every((k) => equalTo(a.v[k], b.v[k]));
  }
  return 'v' in a && 'v' in b && a.v === b.v;
}

function order(a: MongoLiteral, b: MongoLiteral): number | null {
  const an = numberOf(a);
  const bn = numberOf(b);
  if (!Number.isNaN(an) && !Number.isNaN(bn)) return an - bn;
  if ((a.t === 'str' && b.t === 'str') || (a.t === 'date' && b.t === 'date')) return a.v < b.v ? -1 : a.v > b.v ? 1 : 0;
  return null;
}

/** MongoDB equality: the whole value, or any element of an array value. */
function eqMatch(v: MongoLiteral | undefined, arg: MongoLiteral): boolean {
  if (v === undefined) return arg.t === 'null';
  if (equalTo(v, arg)) return true;
  return v.t === 'arr' && v.v.some((x) => eqMatch(x, arg));
}

/** A comparison against a value, or any element of an array value. */
function cmpMatch(v: MongoLiteral | undefined, arg: MongoLiteral, test: (r: number) => boolean): boolean {
  if (v === undefined) return false;
  if (v.t === 'arr') return v.v.some((x) => cmpMatch(x, arg, test));
  const r = order(v, arg);
  return r !== null && test(r);
}

/** A regular expression against a string, or any string element of an array value. */
function regexMatch(v: MongoLiteral | undefined, re: RegExp): boolean {
  if (v === undefined) return false;
  if (v.t === 'arr') return v.v.some((x) => regexMatch(x, re));
  if (v.t !== 'str') return false;
  re.lastIndex = 0;
  return re.test(v.v);
}

/** `$in`: the whole value or any element matches one entry; a regex entry tests strings. */
function inMatch(v: MongoLiteral | undefined, arg: MongoLiteral): boolean {
  if (arg.t !== 'arr') return false;
  return arg.v.some((x) => (x.t === 'regex' ? regexMatch(v, x.v) : eqMatch(v, x)));
}

function matchOp(v: MongoLiteral | undefined, op: string, arg: MongoLiteral): boolean {
  switch (op) {
    case '$eq':
      return eqMatch(v, arg);
    case '$ne':
      return !eqMatch(v, arg);
    case '$gt':
      return cmpMatch(v, arg, (r) => r > 0);
    case '$gte':
      return cmpMatch(v, arg, (r) => r >= 0);
    case '$lt':
      return cmpMatch(v, arg, (r) => r < 0);
    case '$lte':
      return cmpMatch(v, arg, (r) => r <= 0);
    case '$in':
      return inMatch(v, arg);
    case '$nin':
      return !inMatch(v, arg);
    case '$exists':
      return (v !== undefined) === (arg.t === 'bool' ? arg.v : arg.t !== 'null');
    case '$regex':
      if (arg.t === 'regex') return regexMatch(v, arg.v);
      return arg.t === 'str' ? regexMatch(v, new RegExp(arg.v)) : false;
    default:
      throw findFail(`the mock backend does not support the ${op} operator`);
  }
}

function matchCond(v: MongoLiteral | undefined, cond: MongoLiteral): boolean {
  if (cond.t === 'doc' && Object.keys(cond.v).some((k) => k.startsWith('$'))) {
    return Object.entries(cond.v).every(([op, arg]) => matchOp(v, op, arg));
  }
  if (cond.t === 'regex') return regexMatch(v, cond.v);
  return eqMatch(v, cond);
}

/** A field condition: a dotted path can resolve to several values, and `$ne`/`$nin` judge them together. */
function matchField(doc: Record<string, unknown>, path: string, cond: MongoLiteral): boolean {
  const values = pathValues(doc, path.split('.'));
  if (!values.length) return matchCond(undefined, cond);
  const lits = values.map((v) => fromDoc(v));
  if (cond.t === 'doc' && Object.keys(cond.v).some((k) => k.startsWith('$'))) {
    return Object.entries(cond.v).every(([op, arg]) => {
      if (op === '$ne') return !lits.some((v) => eqMatch(v, arg));
      if (op === '$nin') return !lits.some((v) => inMatch(v, arg));
      if (op === '$exists') return arg.t === 'bool' ? arg.v : arg.t !== 'null';
      return lits.some((v) => matchOp(v, op, arg));
    });
  }
  return lits.some((v) => matchCond(v, cond));
}

function matchDoc(doc: Record<string, unknown>, filter: MongoLiteral | undefined): boolean {
  if (!filter) return true;
  if (filter.t !== 'doc') throw findFail('the filter must be an object');
  return Object.entries(filter.v).every(([key, cond]) => {
    if (key === '$and') return cond.t === 'arr' && cond.v.every((c) => matchDoc(doc, c));
    if (key === '$or') return cond.t === 'arr' && cond.v.some((c) => matchDoc(doc, c));
    if (key.startsWith('$')) throw findFail(`the mock backend does not support the ${key} operator`);
    return matchField(doc, key, cond);
  });
}

type FindCall = { coll: string; filter?: MongoLiteral; projection?: MongoLiteral; sort?: [string, number][]; skip?: number; limit?: number };

function mongoFindCall(part: string): FindCall | null {
  const m = /^db\s*\.\s*(?:getCollection\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')\s*\)|([A-Za-z_$][\w$]*))\s*\.\s*find\s*\(/.exec(part);
  if (!m) return null;
  const coll = m[2] ?? strAt(m[1], 0)[0];
  let i = m[0].length;
  const args: MongoLiteral[] = [];
  i = skipWs(part, i);
  while (part[i] !== ')') {
    const [v, j] = litAt(part, i);
    args.push(v);
    i = skipWs(part, j);
    if (part[i] === ',') i = skipWs(part, i + 1);
  }
  i = skipWs(part, i + 1);
  const out: FindCall = { coll };
  if (args[0]) out.filter = args[0];
  if (args[1]) out.projection = args[1];
  // The cursor chain is read in order; every other method stays ignored, as before.
  for (;;) {
    const chain = /^\.\s*(sort|skip|limit|project)\s*\(/.exec(part.slice(i));
    if (!chain) break;
    i += chain[0].length;
    const [v, j] = litAt(part, i);
    i = skipWs(part, j);
    if (part[i] === ')') i++;
    if (chain[1] === 'sort' && v.t === 'doc') out.sort = Object.entries(v.v).map(([k, x]) => [k, x.t === 'num' && x.v < 0 ? -1 : 1]);
    else if (chain[1] === 'skip' && v.t === 'num') out.skip = v.v;
    else if (chain[1] === 'limit' && v.t === 'num') out.limit = v.v;
    else if (chain[1] === 'project') out.projection = v;
  }
  return out;
}

/** Without `requested`, the last statement's own `.limit(n)` above the default sizes the run, like `result::limit`. */
export function mongoRun(c: Connection, sql: string, requested: number | undefined, sort?: RunSort) {
  const parts = sql.split(';').map((p) => p.trim()).filter((p) => p.replace(/\/\/.*$/gm, '').trim());
  if (!parts.length) throw new ApiError({ code: 'invalid_input', message: 'Nothing to run' });
  const started = Date.now();
  const limit = requested ?? Math.max(mongoFindCall(parts.at(-1)!)?.limit ?? 0, DEFAULT_LIMIT);
  const statements: StatementResult[] = parts.map((part) => {
    const call = mongoFindCall(part);
    if (!call) throw new ApiError({ code: 'unsupported', message: 'The mock backend runs only find() on MongoDB' });
    const { coll } = call;
    const fields = mongoSchema[coll] ?? [];
    let cols = [...new Set(fields.map((f) => mongoFlatColumn(f.name)))];
    const arrayCols = new Set(fields.filter((f) => f.name.includes('[]')).map((f) => mongoFlatColumn(f.name)));
    // The projection keeps or drops field paths; an include list also adds paths the sample lacks.
    // Projection operators and nested documents are not implemented, so they fail like filter operators.
    if (call.projection?.t === 'doc') {
      const entries = Object.entries(call.projection.v);
      for (const [key, x] of entries) {
        if (key.startsWith('$')) throw findFail(`the mock backend does not support the ${key} projection`);
        if (x.t === 'doc') {
          const op = Object.keys(x.v).find((k) => k.startsWith('$'));
          throw findFail(op ? `the mock backend does not support the ${op} projection` : 'the mock backend does not support nested projections');
        }
        if (x.t !== 'num' && x.t !== 'bool') throw findFail(`the mock backend does not support the ${key} projection`);
      }
      const keep = (x: MongoLiteral) => !(x.t === 'num' && x.v === 0) && !(x.t === 'bool' && !x.v);
      const included = entries.filter(([, x]) => keep(x)).map(([k]) => mongoFlatColumn(k));
      const excluded = new Set(entries.filter(([, x]) => !keep(x)).map(([k]) => mongoFlatColumn(k)));
      cols = included.length ? included.filter((x) => x !== '_id' || !excluded.has('_id')) : cols.filter((x) => !excluded.has(x));
      if (included.length && !excluded.has('_id') && !cols.includes('_id')) cols.unshift('_id');
    }
    const sortKeys: [string, number][] = sort ? [[sort.column, sort.ascending ? 1 : -1]] : (call.sort ?? []);
    let docs = mongoDocs(coll).filter((d) => matchDoc(d, call.filter));
    if (sortKeys.length) {
      docs = [...docs].sort((a, b) => {
        for (const [k, dir] of sortKeys) {
          const x = fromDoc(lookupPath(a, k));
          const y = fromDoc(lookupPath(b, k));
          const r = x && y ? order(x, y) : x ? -1 : y ? 1 : 0;
          if (r) return r * dir;
        }
        return 0;
      });
    }
    if (call.skip) docs = docs.slice(call.skip);
    const n = Math.min(call.limit ?? Infinity, limit);
    docs = docs.slice(0, n);
    const cells = docs.map((d) => cols.map((col) => mongoCell(lookupPath(d, col))));
    const rows = cells.map((row) => row.map((x) => x.v));
    return {
      sql: part,
      columns: cols.map((name) => ({ name, dataType: arrayCols.has(name) ? 'array' : fields.find((f) => mongoFlatColumn(f.name) === name)?.dataType ?? 'array' })),
      rows,
      rowCount: rows.length,
      truncated: false,
      limit,
      isRead: true,
      sortable: true,
      elapsedMs: 1,
      documents: docs as unknown as JsonValue[],
      cellTypes: cells.map((row) => row.map((x) => x.t)),
    };
  });
  return { statements, elapsedMs: Date.now() - started };
}
